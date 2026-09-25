//! Options-hash reading under json 3's unknown-key rule: every key of an
//! options hash must be one the entry point reads, else ArgumentError
//! in json 3's wording ("unknown keyword: foo"). A reader counts the
//! keys it finds, so a clean hash costs one length compare; only a
//! shortfall walks the hash to name the keys nobody read.

use magnus::r_hash::ForEach;
use magnus::value::ReprValue;
use magnus::{Error, RHash, Ruby, Symbol, Value};

macro_rules! options {
    ($($variant:ident => $name:literal,)*) => {
        /// Every option any entry point reads. The discriminant is the
        /// option's bit in [`OptReader`]'s masks, so a key two decoders
        /// read from one hash (reformat takes parse and generate
        /// options together) counts once.
        #[derive(Clone, Copy)]
        pub(crate) enum Opt {
            $($variant,)*
        }

        /// Ruby option names, indexed by [`Opt`] discriminant.
        const NAMES: &[&str] = &[$($name,)*];
    };
}

options! {
    SymbolizeNames => "symbolize_names",
    Freeze => "freeze",
    MaxNesting => "max_nesting",
    AllowNan => "allow_nan",
    AllowTrailingComma => "allow_trailing_comma",
    AllowDuplicateKey => "allow_duplicate_key",
    ObjectClass => "object_class",
    ArrayClass => "array_class",
    DecimalClass => "decimal_class",
    OnLoad => "on_load",
    CreateAdditions => "create_additions",
    AllowComments => "allow_comments",
    AllowControlCharacters => "allow_control_characters",
    AllowInvalidEscape => "allow_invalid_escape",
    Indent => "indent",
    Space => "space",
    SpaceBefore => "space_before",
    ObjectNl => "object_nl",
    ArrayNl => "array_nl",
    AsciiOnly => "ascii_only",
    ScriptSafe => "script_safe",
    Strict => "strict",
    Depth => "depth",
    BufferInitialLength => "buffer_initial_length",
    SortKeys => "sort_keys",
    AsJson => "as_json",
}

const _: () = assert!(NAMES.len() <= u64::BITS as usize);

impl Opt {
    fn bit(self) -> u64 {
        1 << self as u32
    }

    pub(crate) fn name(self) -> &'static str {
        NAMES[self as usize]
    }
}

pub(crate) struct OptReader<'a> {
    ruby: &'a Ruby,
    hash: RHash,
    /// The hash's key count, read once.
    len: usize,
    /// Options read and present in the hash.
    found: u64,
    /// Options accepted only while falsy (see [`OptReader::tolerate`]).
    tolerated: u64,
}

impl<'a> OptReader<'a> {
    pub(crate) fn new(ruby: &'a Ruby, hash: RHash) -> Self {
        Self {
            ruby,
            hash,
            len: hash.len(),
            found: 0,
            tolerated: 0,
        }
    }

    pub(crate) fn ruby(&self) -> &'a Ruby {
        self.ruby
    }

    fn all_found(&self) -> bool {
        self.found.count_ones() as usize == self.len
    }

    /// The value under `opt`'s Symbol key; an explicit nil is present.
    /// Once every key is found, the rest are absent without a lookup
    /// (`{symbolize_names: true}` costs one lookup, not six). An option
    /// read twice (reformat reads a few for both parsing and
    /// generating) is looked up again.
    pub(crate) fn get(&mut self, opt: Opt) -> Option<Value> {
        if self.all_found() && self.found & opt.bit() == 0 {
            return None;
        }
        // An interned StaticSymbol: no String allocation per lookup.
        let value = self.hash.get(self.ruby.sym_new(opt.name()));
        if value.is_some() {
            self.found |= opt.bit();
        }
        value
    }

    pub(crate) fn truthy(&mut self, opt: Opt) -> bool {
        self.get(opt).is_some_and(|v| v.to_bool())
    }

    /// Accept these json options, which NOSJ does not implement, only
    /// while falsy: their default behavior is NOSJ's, anything else
    /// would be silently ignored. No lookup happens here: such a key can
    /// only be present when the reads leave keys unfound, so
    /// [`OptReader::finish`] checks them on its cold path.
    pub(crate) fn tolerate(&mut self, opts: &[Opt]) {
        for opt in opts {
            self.tolerated |= opt.bit();
        }
    }

    /// Raise for keys no read asked for, and for tolerated options set
    /// to something truthy.
    pub(crate) fn finish(self) -> Result<(), Error> {
        if self.all_found() {
            return Ok(());
        }
        self.leftover_keys()
    }

    #[cold]
    #[inline(never)]
    fn leftover_keys(self) -> Result<(), Error> {
        let (found, tolerated) = (self.found, self.tolerated);
        let option_of = |key: Value| {
            let name = Symbol::from_value(key)?.name().ok()?;
            NAMES.iter().position(|known| *known == name)
        };
        let mut unknown = Vec::new();
        let mut unsupported = None;
        self.hash.foreach(|key: Value, value: Value| {
            match option_of(key).map(|index| 1u64 << index) {
                Some(bit) if found & bit != 0 => {}
                Some(bit) if tolerated & bit != 0 => {
                    if value.to_bool() {
                        unsupported = Some(key.to_string());
                        return Ok(ForEach::Stop);
                    }
                }
                _ => unknown.push(key.to_string()),
            }
            Ok(ForEach::Continue)
        })?;
        let message = match (unsupported, unknown.as_slice()) {
            (Some(name), _) => format!("NOSJ does not support the {name} option"),
            (None, []) => return Ok(()),
            (None, [key]) => format!("unknown keyword: {key}"),
            (None, keys) => format!("unknown keywords: {}", keys.join(", ")),
        };
        Err(Error::new(self.ruby.exception_arg_error(), message))
    }
}

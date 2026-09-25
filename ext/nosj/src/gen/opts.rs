//! JSON.generate-compatible option decoding: formatting strings, escape
//! mode, nesting limits, and the buffer size hint.

use magnus::value::ReprValue;
use magnus::{Error, RHash, RString, Ruby, Value};
use nosj::emit::EscapeMode;

use crate::opt_reader::{Opt, OptReader};
use crate::sink::MAX_NESTING;

pub(crate) struct GenConfig {
    pub(crate) indent: Vec<u8>,
    pub(crate) space: Vec<u8>,
    pub(crate) space_before: Vec<u8>,
    pub(crate) object_nl: Vec<u8>,
    pub(crate) array_nl: Vec<u8>,
    /// 0 = unlimited.
    pub(super) max_nesting: usize,
    pub(super) start_depth: usize,
    pub(crate) allow_nan: bool,
    pub(super) strict: bool,
    /// ActiveSupport walk semantics: non-native values recurse through
    /// as_json instead of splicing to_json, and non-finite floats emit
    /// null (Float#as_json parity). Set only by the Rails entry, never
    /// from user option hashes.
    pub(super) rails: bool,
    /// json 3: keys that render the same (`"a"` and `:a`) raise unless
    /// this is set. The Rails configs keep ActiveSupport's own handling.
    pub(super) allow_duplicate_key: bool,
    pub(crate) mode: EscapeMode,
    /// Precomputed "any formatting string set": scanning the five
    /// vectors per call was measurable on tiny documents.
    pub(super) pretty: bool,
}

/// The json gem's defaults under an escape mode, for the plain walk or
/// the Rails encoder's (which keeps ActiveSupport's own key handling,
/// so it allows keys that render alike). A const fn because statics
/// cannot struct-update a type with `Vec` fields; `Vec::new` is const
/// and allocation-free.
const fn defaults(rails: bool, mode: EscapeMode) -> GenConfig {
    GenConfig {
        indent: Vec::new(),
        space: Vec::new(),
        space_before: Vec::new(),
        object_nl: Vec::new(),
        array_nl: Vec::new(),
        max_nesting: MAX_NESTING,
        start_depth: 0,
        allow_nan: false,
        strict: false,
        rails,
        allow_duplicate_key: rails,
        mode,
        pretty: false,
    }
}

/// The nil-options configuration, shared instead of rebuilt: stamping
/// a fresh ~140-byte GenConfig onto the stack per call was measurable
/// on tiny documents (the json gem likewise reuses a cached State for
/// the default options). Safe as a static: generation only ever
/// borrows the config.
pub(crate) static DEFAULT_CONFIG: GenConfig = defaults(false, EscapeMode::Standard);

/// The Rails-encoder configuration for ActiveSupport's default escape
/// flags (HTML entities and JS separators both on, the overwhelmingly
/// common case): escaping is fused into the crate's HtmlSafe kernels,
/// one pass, no post-scan.
pub(super) static RAILS_HTML_SAFE_CONFIG: GenConfig = defaults(true, EscapeMode::HtmlSafe);

/// Rails-encoder configuration with HTML entities on and JS separators
/// off.
pub(super) static RAILS_HTML_ENTITIES_CONFIG: GenConfig = defaults(true, EscapeMode::HtmlEntities);

/// Rails-encoder configuration with JS separators on and HTML entities
/// off.
pub(super) static RAILS_JS_SEPARATORS_CONFIG: GenConfig = defaults(true, EscapeMode::JsSeparators);

/// The Rails-encoder configuration with every escape flag off
/// (encode(escape: false)). Mirrors JSONGemEncoder#stringify, which
/// generates with the json gem's defaults.
pub(super) static RAILS_CONFIG: GenConfig = defaults(true, EscapeMode::Standard);

impl Default for GenConfig {
    fn default() -> Self {
        defaults(false, EscapeMode::Standard)
    }
}

/// A formatting string option's bytes; empty when absent or nil.
fn opt_bytes(r: &mut OptReader, opt: Opt) -> Result<Vec<u8>, Error> {
    let Some(v) = r.get(opt).filter(|v| !v.is_nil()) else {
        return Ok(Vec::new());
    };
    let s = RString::from_value(v).ok_or_else(|| {
        Error::new(
            r.ruby().exception_type_error(),
            format!("{} must be a String", opt.name()),
        )
    })?;
    Ok(unsafe { s.as_slice() }.to_vec())
}

/// Decode a generate options hash (nil takes [`DEFAULT_CONFIG`] at the
/// call site without constructing anything); keys it does not read
/// raise ArgumentError, like json 3.
pub(crate) fn parse_gen_opts(ruby: &Ruby, opts: Value) -> Result<(GenConfig, usize), Error> {
    if opts.is_nil() {
        return Ok((GenConfig::default(), 0));
    }
    let opts = RHash::from_value(opts)
        .ok_or_else(|| Error::new(ruby.exception_type_error(), "options must be a Hash or nil"))?;
    let mut reader = OptReader::new(ruby, opts);
    let decoded = read_gen_opts(&mut reader)?;
    reader.finish()?;
    Ok(decoded)
}

/// Read the json 3 generate options and the buffer size hint. sort_keys
/// and as_json, which NOSJ does not implement, raise unless falsy.
pub(crate) fn read_gen_opts(r: &mut OptReader) -> Result<(GenConfig, usize), Error> {
    let mut cfg = GenConfig {
        indent: opt_bytes(r, Opt::Indent)?,
        space: opt_bytes(r, Opt::Space)?,
        space_before: opt_bytes(r, Opt::SpaceBefore)?,
        object_nl: opt_bytes(r, Opt::ObjectNl)?,
        array_nl: opt_bytes(r, Opt::ArrayNl)?,
        allow_nan: r.truthy(Opt::AllowNan),
        strict: r.truthy(Opt::Strict),
        allow_duplicate_key: r.truthy(Opt::AllowDuplicateKey),
        ..GenConfig::default()
    };
    let mut cap_hint = 0usize;
    r.tolerate(&[Opt::SortKeys, Opt::AsJson]);
    let ascii = r.truthy(Opt::AsciiOnly);
    let script = r.truthy(Opt::ScriptSafe);
    if ascii {
        cfg.mode = EscapeMode::AsciiOnly;
        if script {
            return Err(Error::new(
                r.ruby().exception_arg_error(),
                "NOSJ.generate: ascii_only and script_safe cannot be combined",
            ));
        }
    } else if script {
        cfg.mode = EscapeMode::ScriptSafe;
    }
    if let Some(v) = r.get(Opt::MaxNesting) {
        // nil/false → unlimited; true → keep the default 100; Integer → limit.
        if !v.to_bool() {
            cfg.max_nesting = 0;
        } else if let Ok(n) = <i64 as magnus::TryConvert>::try_convert(v) {
            cfg.max_nesting = if n <= 0 { 0 } else { n as usize };
        }
    }
    if let Some(v) = r.get(Opt::Depth) {
        if let Ok(n) = <i64 as magnus::TryConvert>::try_convert(v) {
            cfg.start_depth = if n <= 0 { 0 } else { n as usize };
        }
    }
    if let Some(v) = r.get(Opt::BufferInitialLength) {
        if let Ok(n) = <i64 as magnus::TryConvert>::try_convert(v) {
            if n > 0 {
                cap_hint = n as usize;
            }
        }
    }
    cfg.pretty = !(cfg.indent.is_empty()
        && cfg.space.is_empty()
        && cfg.space_before.is_empty()
        && cfg.object_nl.is_empty()
        && cfg.array_nl.is_empty());
    Ok((cfg, cap_hint))
}

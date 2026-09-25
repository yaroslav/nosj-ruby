//! `NOSJ.stats` / `NOSJ.stats_file`: one full-parser pass into a
//! counting sink (the `NOSJ.valid?` machinery with counters instead of
//! discards), answering "what is this 40 MB blob" without building a
//! single Ruby value for the document. Only the small result Hash is
//! allocated, after the pass.

use ahash::AHashMap;
use magnus::value::ReprValue;
use magnus::{Error, RHash, RString, Ruby, Value};

use crate::files::with_mapped_file;
use crate::opt_reader::{Opt, OptReader};
use crate::parse::{drive_error, max_nesting_of, options_hash, utf8_input, ParseNativeOpts};
use crate::sink::SinkAbort;
use crate::state::with_pull_state;

/// What the document's root value was; reported as a Symbol. The
/// Default is never observable (a successful pass always saw a root
/// event); it only makes the sink derivable.
#[derive(Clone, Copy, Default)]
enum RootKind {
    Object,
    Array,
    String,
    Integer,
    Float,
    Boolean,
    #[default]
    Null,
}

impl RootKind {
    fn name(self) -> &'static str {
        match self {
            Self::Object => "object",
            Self::Array => "array",
            Self::String => "string",
            Self::Integer => "integer",
            Self::Float => "float",
            Self::Boolean => "boolean",
            Self::Null => "null",
        }
    }
}

/// Counting sink: every event increments counters; nothing else is
/// retained except the key histogram (memory proportional to the
/// number of UNIQUE keys, unbounded by design: this is a diagnostic
/// pass over a document the caller already holds).
#[derive(Default)]
struct StatsSink {
    depth: usize,
    max_nesting: usize,
    root: Option<RootKind>,
    max_depth: usize,
    objects: u64,
    arrays: u64,
    max_object_entries: u64,
    max_array_length: u64,
    nulls: u64,
    booleans: u64,
    integers: u64,
    floats: u64,
    strings: u64,
    string_bytes: u64,
    max_string_bytes: u64,
    keys: u64,
    histogram: AHashMap<Box<str>, u64>,
}

impl StatsSink {
    fn root(&mut self, kind: RootKind) {
        if self.root.is_none() {
            self.root = Some(kind);
        }
    }

    fn string(&mut self, bytes: u64) {
        self.strings += 1;
        self.string_bytes += bytes;
        self.max_string_bytes = self.max_string_bytes.max(bytes);
    }

    fn count_key(&mut self, key: &str) {
        self.keys += 1;
        *self.histogram.entry(Box::from(key)).or_insert(0) += 1;
    }

    fn enter_container(&mut self, kind: RootKind) -> Result<(), SinkAbort> {
        self.root(kind);
        self.depth += 1;
        if self.depth > self.max_nesting {
            return Err(SinkAbort::TooDeep);
        }
        self.max_depth = self.max_depth.max(self.depth);
        Ok(())
    }

    fn total_values(&self) -> u64 {
        self.objects
            + self.arrays
            + self.strings
            + self.integers
            + self.floats
            + self.booleans
            + self.nulls
    }
}

impl nosj::Sink for StatsSink {
    type Error = SinkAbort;

    fn null(&mut self) -> Result<(), SinkAbort> {
        self.root(RootKind::Null);
        self.nulls += 1;
        Ok(())
    }

    fn boolean(&mut self, _: bool) -> Result<(), SinkAbort> {
        self.root(RootKind::Boolean);
        self.booleans += 1;
        Ok(())
    }

    fn int(&mut self, _: i64) -> Result<(), SinkAbort> {
        self.root(RootKind::Integer);
        self.integers += 1;
        Ok(())
    }

    fn float(&mut self, _: f64) -> Result<(), SinkAbort> {
        self.root(RootKind::Float);
        self.floats += 1;
        Ok(())
    }

    fn big_int(&mut self, _: &str) -> Result<(), SinkAbort> {
        self.root(RootKind::Integer);
        self.integers += 1;
        Ok(())
    }

    fn str(&mut self, value: &str) -> Result<(), SinkAbort> {
        self.root(RootKind::String);
        self.string(value.len() as u64);
        Ok(())
    }

    fn str_bytes(&mut self, value: &[u8]) -> Result<(), SinkAbort> {
        self.root(RootKind::String);
        self.string(value.len() as u64);
        Ok(())
    }

    fn key(&mut self, key: &str) -> Result<(), SinkAbort> {
        self.count_key(key);
        Ok(())
    }

    fn key_bytes(&mut self, key: &[u8]) -> Result<(), SinkAbort> {
        // Broken-WTF-8 keys are pathological; a lossy conversion keeps
        // the histogram total consistent with the key count.
        self.count_key(&String::from_utf8_lossy(key));
        Ok(())
    }

    fn begin_array(&mut self) -> Result<(), SinkAbort> {
        self.enter_container(RootKind::Array)
    }

    fn begin_object(&mut self) -> Result<(), SinkAbort> {
        self.enter_container(RootKind::Object)
    }

    fn mark(&self) -> usize {
        0
    }

    fn end_array(&mut self, _: usize, len: usize) -> Result<(), SinkAbort> {
        self.depth -= 1;
        self.arrays += 1;
        self.max_array_length = self.max_array_length.max(len as u64);
        Ok(())
    }

    fn end_object(&mut self, _: usize, pairs: usize) -> Result<(), SinkAbort> {
        self.depth -= 1;
        self.objects += 1;
        self.max_object_entries = self.max_object_entries.max(pairs as u64);
        Ok(())
    }
}

/// Assemble the result Hash. Sub-hashes group related counters; the
/// histogram is sorted by count (descending), ties by key, so
/// `.first(10)` reads as a top-10.
fn stats_to_hash(ruby: &Ruby, s: &StatsSink, byte_size: usize) -> Result<Value, Error> {
    let set = |h: &RHash, name: &str, v: u64| h.aset(ruby.to_symbol(name), v);

    let values = ruby.hash_new();
    set(&values, "total", s.total_values())?;
    set(&values, "objects", s.objects)?;
    set(&values, "arrays", s.arrays)?;
    set(&values, "strings", s.strings)?;
    set(&values, "integers", s.integers)?;
    set(&values, "floats", s.floats)?;
    set(&values, "booleans", s.booleans)?;
    set(&values, "nulls", s.nulls)?;

    let keys = ruby.hash_new();
    set(&keys, "total", s.keys)?;
    set(&keys, "unique", s.histogram.len() as u64)?;

    let containers = ruby.hash_new();
    set(&containers, "max_object_entries", s.max_object_entries)?;
    set(&containers, "max_array_length", s.max_array_length)?;

    let strings = ruby.hash_new();
    set(&strings, "bytes", s.string_bytes)?;
    set(&strings, "max_bytes", s.max_string_bytes)?;

    let mut sorted: Vec<(&str, u64)> = s.histogram.iter().map(|(k, &n)| (&**k, n)).collect();
    sorted.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let histogram = ruby.hash_new_capa(sorted.len());
    for (key, count) in sorted {
        histogram.aset(ruby.str_new(key), count)?;
    }

    let out = ruby.hash_new();
    set(&out, "byte_size", byte_size as u64)?;
    let root = s.root.unwrap_or_default();
    out.aset(ruby.to_symbol("root"), ruby.to_symbol(root.name()))?;
    set(&out, "max_depth", s.max_depth as u64)?;
    out.aset(ruby.to_symbol("values"), values)?;
    out.aset(ruby.to_symbol("keys"), keys)?;
    out.aset(ruby.to_symbol("key_histogram"), histogram)?;
    out.aset(ruby.to_symbol("containers"), containers)?;
    out.aset(ruby.to_symbol("strings"), strings)?;
    Ok(out.as_value())
}

/// Stats' options: `max_nesting`, which here defaults to UNLIMITED (a
/// deep blob is exactly what a diagnostic should describe, not refuse),
/// and the grammar extensions. Decoded before the source is borrowed:
/// a hash lookup can run a key's own `hash`/`eql?`.
fn stats_opts(ruby: &Ruby, opts: Value) -> Result<ParseNativeOpts, Error> {
    let mut o = ParseNativeOpts {
        max_nesting: usize::MAX,
        ..ParseNativeOpts::default()
    };
    let Some(h) = options_hash(ruby, opts)? else {
        return Ok(o);
    };
    let mut r = OptReader::new(ruby, h);
    if let Some(v) = r.get(Opt::MaxNesting) {
        o.max_nesting = max_nesting_of(v);
    }
    o.popts.allow_nan = r.truthy(Opt::AllowNan);
    o.popts.allow_trailing_comma = r.truthy(Opt::AllowTrailingComma);
    r.finish()?;
    Ok(o)
}

/// Run the counting pass over already-UTF-8-vouched bytes and build
/// the result.
fn stats_over(ruby: &Ruby, input: &[u8], o: &ParseNativeOpts) -> Result<Value, Error> {
    let mut sink = StatsSink {
        max_nesting: o.max_nesting,
        ..StatsSink::default()
    };
    let result = with_pull_state(|state| {
        // Safety: callers verified UTF-8 (coderange or a full scan).
        unsafe { nosj::parse_utf8_unchecked_with(input, &mut state.bufs, &mut sink, o.popts) }
    });
    result.map_err(|failure| drive_error(ruby, failure, o, input, (0, input.len())))?;
    stats_to_hash(ruby, &sink, input.len())
}

/// `NOSJ.stats(source, opts)`: document statistics from one null-sink
/// parser pass.
pub fn stats_native(
    ruby: &Ruby,
    _rb_self: Value,
    data: RString,
    opts: Value,
) -> Result<Value, Error> {
    let o = stats_opts(ruby, opts)?;
    let input = utf8_input(ruby, &data)?;
    stats_over(ruby, input, &o)
}

/// `NOSJ.stats_file(path, opts)`: `NOSJ.stats` against a memory-mapped
/// file; `byte_size` is the file's size.
pub fn stats_file_native(
    ruby: &Ruby,
    _rb_self: Value,
    path: RString,
    opts: Value,
) -> Result<Value, Error> {
    let o = stats_opts(ruby, opts)?;
    let p = path.to_string()?;
    with_mapped_file(ruby, &p, |map| stats_over(ruby, &map, &o))
}

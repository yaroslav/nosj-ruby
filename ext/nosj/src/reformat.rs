//! Reformat without building values: `NOSJ.minify` / `NOSJ.reformat`
//! drive the full parser straight into the crate's `Writer`, a pure
//! event-to-bytes pipe. No Ruby object is allocated for the document,
//! only the result String; between the SIMD scan on the way in and the
//! SIMD escape kernels on the way out there is nothing else.
//!
//! Output is exactly what `NOSJ.generate(NOSJ.parse(json), opts)`
//! would produce, and the pipe accepts exactly what parse accepts:
//! duplicate keys raise unless `allow_duplicate_key` (then they pass
//! through: a reformatter must not silently drop data the way parse's
//! last-key-wins materialization does), and lone surrogates raise.
//! Numbers come out in the gem's canonical spelling (`1.50` becomes
//! `1.5`), and string escapes are normalized by the emission kernels.

use std::cell::Cell;

use magnus::{Error, RString, Ruby, Value};
use nosj::{FloatFormat, WriteOptions, Writer};

use crate::errors::{nesting_error, nosj_exception, parser_error, parser_error_at};
use crate::files::with_mapped_file;
use crate::gen::opts::{read_gen_opts, GenConfig, DEFAULT_CONFIG};
use crate::opt_reader::OptReader;
use crate::parse::{
    duplicate_key_error, lone_surrogate_error, options_hash, read_parse_opts, utf8_input,
    ParseNativeOpts,
};
use crate::patch::finish_string;
use crate::sink::{DupKeys, SinkAbort};
use crate::state::{with_pull_state, with_taken};

thread_local! {
    /// Pooled output buffer: capacity survives across calls (see
    /// `state::with_taken`).
    static PIPE_BUF: Cell<Vec<u8>> = const { Cell::new(Vec::new()) };
}

/// The event-to-Writer pipe. Structure events forward to the Writer's
/// grammar state (separators, layout, indentation); scalar events
/// forward to its emission kernels.
struct PipeSink<'a> {
    w: Writer<'a>,
    depth: usize,
    max_nesting: usize,
    /// Non-finite floats pass through as literals only when the
    /// generate side allows them; see [`PipeSink::float`].
    allow_nan: bool,
    dup_keys: DupKeys<'a>,
}

impl PipeSink<'_> {
    fn enter(&mut self) -> Result<(), SinkAbort> {
        self.depth += 1;
        if self.depth > self.max_nesting {
            return Err(SinkAbort::TooDeep);
        }
        Ok(())
    }
}

impl nosj::Sink for PipeSink<'_> {
    type Error = SinkAbort;

    fn null(&mut self) -> Result<(), SinkAbort> {
        self.w.null();
        Ok(())
    }

    fn boolean(&mut self, value: bool) -> Result<(), SinkAbort> {
        self.w.boolean(value);
        Ok(())
    }

    fn int(&mut self, value: i64) -> Result<(), SinkAbort> {
        self.w.int(value);
        Ok(())
    }

    fn float(&mut self, value: f64) -> Result<(), SinkAbort> {
        if value.is_finite() {
            self.w.float(value);
            return Ok(());
        }
        let spelling = if value.is_nan() {
            "NaN"
        } else if value > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        };
        // Not just the allow_nan keywords: a huge-exponent literal
        // (1e999) parses to Infinity in strict mode too, and generate
        // refuses to emit it. Gem parity either way. The parser only
        // delivers the f64, never the original digits, so passing the
        // source spelling through is not an option. One asymmetry
        // follows: the pipe streams duplicate-key entries that
        // parse's last-key-wins would discard, so a document with an
        // overflowing literal shadowed by a duplicate key raises here
        // even though generate(parse(x)) would succeed.
        if !self.allow_nan {
            return Err(SinkAbort::NonFiniteFloat(spelling));
        }
        self.w.value_raw(spelling.as_bytes());
        Ok(())
    }

    fn big_int(&mut self, digits: &str) -> Result<(), SinkAbort> {
        // Verbatim digit passthrough: no bignum is ever built.
        self.w.value_raw(digits.as_bytes());
        Ok(())
    }

    fn str(&mut self, value: &str) -> Result<(), SinkAbort> {
        self.w.str(value);
        Ok(())
    }

    fn str_bytes(&mut self, _: &[u8]) -> Result<(), SinkAbort> {
        Err(SinkAbort::LoneSurrogate)
    }

    fn key(&mut self, key: &str) -> Result<(), SinkAbort> {
        self.dup_keys.key(key.as_bytes());
        self.w.key(key);
        Ok(())
    }

    fn key_bytes(&mut self, _: &[u8]) -> Result<(), SinkAbort> {
        Err(SinkAbort::LoneSurrogate)
    }

    fn begin_array(&mut self) -> Result<(), SinkAbort> {
        self.enter()?;
        self.w.begin_array();
        Ok(())
    }

    fn begin_object(&mut self) -> Result<(), SinkAbort> {
        self.enter()?;
        self.w.begin_object();
        Ok(())
    }

    fn mark(&self) -> usize {
        self.dup_keys.mark()
    }

    fn end_array(&mut self, _: usize, _: usize) -> Result<(), SinkAbort> {
        self.depth -= 1;
        self.w.end_array();
        Ok(())
    }

    fn end_object(&mut self, mark: usize, _: usize) -> Result<(), SinkAbort> {
        self.depth -= 1;
        self.dup_keys.close(mark)?;
        self.w.end_object();
        Ok(())
    }
}

/// Map a GenConfig (the gem's generate-option decoding) onto the
/// crate's WriteOptions.
fn write_options(cfg: &GenConfig) -> WriteOptions {
    let mut w = WriteOptions::COMPACT;
    w.indent = cfg.indent.clone();
    w.space = cfg.space.clone();
    w.space_before = cfg.space_before.clone();
    w.object_nl = cfg.object_nl.clone();
    w.array_nl = cfg.array_nl.clone();
    w.escape = cfg.mode;
    w.float = FloatFormat::Fpconv;
    w
}

/// Decoded reformat options: parse acceptance plus generate formatting.
/// Decoded before the source is borrowed, since decoding can run Ruby
/// (an option value's `to_int`; see `parse::utf8_input`).
struct ReformatOpts {
    parse: ParseNativeOpts,
    generate: Option<GenConfig>,
}

impl ReformatOpts {
    /// One reader over both option sets, so a key either reads is known.
    fn decode(ruby: &Ruby, opts: Value) -> Result<Self, Error> {
        let Some(h) = options_hash(ruby, opts)? else {
            return Ok(Self {
                parse: ParseNativeOpts::default(),
                generate: None,
            });
        };
        let mut reader = OptReader::new(ruby, h);
        let parse = read_parse_opts(&mut reader)?;
        let (generate, _) = read_gen_opts(&mut reader)?;
        reader.finish()?;
        Ok(Self {
            parse,
            generate: Some(generate),
        })
    }
}

/// Run the pipe over already-UTF-8-vouched bytes.
fn reformat_over(ruby: &Ruby, input: &[u8], opts: &ReformatOpts) -> Result<RString, Error> {
    let po = &opts.parse;
    let gcfg = opts.generate.as_ref().unwrap_or(&DEFAULT_CONFIG);
    let wopts = write_options(gcfg);

    with_taken(&PIPE_BUF, |buf| {
        let pipe = |buf: &mut Vec<u8>, check_dups: bool| {
            buf.clear();
            // The output is at least input-sized for minify-shaped runs.
            buf.reserve(input.len());
            with_pull_state(|state| {
                let mut sink = PipeSink {
                    w: Writer::new(buf, &wopts),
                    depth: 0,
                    max_nesting: po.max_nesting,
                    allow_nan: gcfg.allow_nan,
                    dup_keys: DupKeys::new(&mut state.fingerprints, &mut state.seen, check_dups),
                };
                // Safety: callers verified UTF-8 (coderange or full scan).
                unsafe {
                    nosj::parse_utf8_unchecked_with(input, &mut state.bufs, &mut sink, po.popts)
                }
            })
        };
        let mut result = pipe(buf, !po.allow_duplicate_key);
        if let Err(nosj::DriveError::Sink(SinkAbort::DuplicateKey)) = result {
            if crate::locate::duplicate_key(input, po.popts).is_some() {
                return Err(duplicate_key_error(ruby, input, 0, input.len(), po.popts));
            }
            // A fingerprint collision, not a repeat: redo without the check.
            result = pipe(buf, false);
        }
        match result {
            Ok(()) => finish_string(buf),
            Err(nosj::DriveError::Sink(SinkAbort::TooDeep)) => Err(nesting_error(
                ruby,
                format!(
                    "nesting of {} is too deep",
                    po.max_nesting.saturating_add(1)
                ),
            )),
            Err(nosj::DriveError::Sink(SinkAbort::LoneSurrogate)) => {
                Err(lone_surrogate_error(ruby, input, 0, input.len(), po.popts))
            }
            Err(nosj::DriveError::Sink(SinkAbort::NonFiniteFloat(spelling))) => Err(Error::new(
                nosj_exception(ruby, "GeneratorError"),
                format!("{spelling} not allowed in JSON"),
            )),
            Err(nosj::DriveError::Sink(_)) => {
                Err(parser_error(ruby, "reformat pass aborted".into()))
            }
            Err(nosj::DriveError::Parse(e)) => {
                Err(parser_error_at(ruby, input, e.offset, e.to_string()))
            }
        }
    })
}

/// `NOSJ.reformat_native(source, opts)`: minify and reformat share
/// this entry; the formatting defaults are compact.
pub fn reformat_native(
    ruby: &Ruby,
    _rb_self: Value,
    data: RString,
    opts: Value,
) -> Result<RString, Error> {
    let opts = ReformatOpts::decode(ruby, opts)?;
    let input = utf8_input(ruby, &data)?;
    reformat_over(ruby, input, &opts)
}

/// `NOSJ.reformat_file_native(path, opts)`: the pipe over a read-only
/// memory map; the input document never becomes a Ruby String.
pub fn reformat_file_native(
    ruby: &Ruby,
    _rb_self: Value,
    path: RString,
    opts: Value,
) -> Result<RString, Error> {
    let p = path.to_string()?;
    let opts = ReformatOpts::decode(ruby, opts)?;
    // Mapping a zero-length file fails with EINVAL on Linux; route an
    // empty file to the parser's own "unexpected end of input" so the
    // error class is deterministic across platforms. Metadata failures
    // fall through for the mapper's Errno.
    if std::fs::metadata(&p).is_ok_and(|m| m.len() == 0) {
        return reformat_over(ruby, &[], &opts);
    }
    with_mapped_file(ruby, &p, |map| reformat_over(ruby, &map, &opts))
}

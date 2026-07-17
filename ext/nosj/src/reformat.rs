//! Reformat without building values: `NOSJ.minify` / `NOSJ.reformat`
//! drive the full parser straight into the crate's `Writer`, a pure
//! event-to-bytes pipe. No Ruby object is allocated for the document,
//! only the result String; between the SIMD scan on the way in and the
//! SIMD escape kernels on the way out there is nothing else.
//!
//! Output is exactly what `NOSJ.generate(NOSJ.parse(json), opts)`
//! would produce, with two deliberate differences: duplicate object
//! keys pass through (a reformatter must not silently drop data the
//! way parse's last-key-wins materialization does), and lone-surrogate
//! string values re-escape as `\uXXXX` instead of raising (the output
//! must reparse; raw WTF-8 would not). Numbers come out in the gem's
//! canonical spelling (`1.50` becomes `1.5`), and string escapes are
//! normalized by the emission kernels.

use std::cell::RefCell;

use magnus::value::ReprValue;
use magnus::{Error, RString, Ruby, Value};
use nosj::emit::EscapeMode;
use nosj::{FloatFormat, WriteOptions, Writer};

use crate::errors::{nesting_error, nosj_exception, parser_error, parser_error_at};
use crate::files::with_mapped_file;
use crate::gen::opts::{parse_gen_opts, GenConfig, DEFAULT_CONFIG};
use crate::parse::{parse_native_opts, utf8_input};
use crate::patch::finish_string;
use crate::sink::SinkAbort;
use crate::state::PULL_STATE;

thread_local! {
    /// Pooled output buffer: capacity survives across calls. The pipe
    /// never calls back into Ruby, so the borrow spans the whole drive
    /// without any reentrancy concern.
    static PIPE_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// The event-to-Writer pipe. Structure events forward to the Writer's
/// grammar state (separators, layout, indentation); scalar events
/// forward to its emission kernels.
struct PipeSink<'a> {
    w: Writer<'a>,
    depth: usize,
    max_nesting: usize,
    /// For re-escaping WTF-8 string content (see [`quote_wtf8`]).
    mode: EscapeMode,
}

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// WTF-8 lone surrogates arrive as one 3-byte sequence with this lead
/// byte (the only ill-formed runs the parser ever emits).
const WTF8_SURROGATE_LEAD: u8 = 0xED;
const WTF8_SURROGATE_LEN: usize = 3;
/// Payload bits of a UTF-8 lead / continuation byte.
const UTF8_LEAD3_BITS: u32 = 0x0F;
const UTF8_CONT_BITS: u32 = 0x3F;

/// Quote and escape WTF-8 content: valid UTF-8 runs go through the
/// configured escape kernel, and lone-surrogate sequences re-escape as
/// `\uXXXX`, so the output reparses to the identical string in every
/// mode (raw WTF-8 bytes would not: the parser requires UTF-8 input).
/// This deliberately diverges from `generate`, which refuses
/// broken-coderange strings: a reformatter must accept everything the
/// parser accepts.
fn quote_wtf8(out: &mut Vec<u8>, bytes: &[u8], mode: EscapeMode) {
    out.push(b'"');
    let mut rest = bytes;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                nosj::emit::escape_into(out, s.as_bytes(), mode);
                break;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                nosj::emit::escape_into(out, &rest[..valid], mode);
                let sur = &rest[valid..];
                debug_assert!(
                    sur.len() >= WTF8_SURROGATE_LEN && sur[0] == WTF8_SURROGATE_LEAD,
                    "parser only emits lone-surrogate WTF-8"
                );
                // Standard 3-byte UTF-8 decode of the surrogate
                // codepoint (U+D800..U+DFFF), re-emitted as \uXXXX.
                let cp = ((u32::from(sur[0]) & UTF8_LEAD3_BITS) << 12)
                    | ((u32::from(sur[1]) & UTF8_CONT_BITS) << 6)
                    | (u32::from(sur[2]) & UTF8_CONT_BITS);
                out.extend_from_slice(b"\\u");
                for shift in [12, 8, 4, 0] {
                    out.push(HEX_DIGITS[((cp >> shift) & 0xF) as usize]);
                }
                rest = &sur[WTF8_SURROGATE_LEN..];
            }
        }
    }
    out.push(b'"');
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
        } else {
            // Reached only when the parse accepted them (allow_nan);
            // the gem's literals go straight through.
            self.w.value_raw(if value.is_nan() {
                b"NaN"
            } else if value > 0.0 {
                b"Infinity"
            } else {
                b"-Infinity"
            });
        }
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

    fn str_bytes(&mut self, value: &[u8]) -> Result<(), SinkAbort> {
        // Rare path (lone-surrogate content); a per-call buffer is fine.
        let mut quoted = Vec::with_capacity(value.len() + 8);
        quote_wtf8(&mut quoted, value, self.mode);
        self.w.value_raw(&quoted);
        Ok(())
    }

    fn key(&mut self, key: &str) -> Result<(), SinkAbort> {
        self.w.key(key);
        Ok(())
    }

    fn key_bytes(&mut self, _key: &[u8]) -> Result<(), SinkAbort> {
        // A lone-surrogate KEY has no pre-serialized escape hatch in
        // the Writer (values have value_raw; a key_raw is on the crate
        // wishlist), so this pathological case keeps generate's
        // refusal semantics.
        Err(SinkAbort::BrokenUtf8Output)
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
        0
    }

    fn end_array(&mut self, _: usize, _: usize) -> Result<(), SinkAbort> {
        self.depth -= 1;
        self.w.end_array();
        Ok(())
    }

    fn end_object(&mut self, _: usize, _: usize) -> Result<(), SinkAbort> {
        self.depth -= 1;
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

/// Run the pipe over already-UTF-8-vouched bytes.
fn reformat_over(ruby: &Ruby, input: &[u8], opts: Value) -> Result<RString, Error> {
    let po = parse_native_opts(ruby, opts)?;
    let built;
    let gcfg: &GenConfig = if opts.is_nil() {
        &DEFAULT_CONFIG
    } else {
        built = parse_gen_opts(ruby, opts)?.0;
        &built
    };
    let wopts = write_options(gcfg);

    PIPE_BUF.with(|cell| {
        let mut buf = cell.borrow_mut();
        buf.clear();
        // The output is at least input-sized for minify-shaped runs.
        buf.reserve(input.len());
        let mut sink = PipeSink {
            w: Writer::new(&mut buf, &wopts),
            depth: 0,
            max_nesting: po.max_nesting,
            mode: gcfg.mode,
        };
        let result = PULL_STATE.with(|state_cell| {
            let mut state = state_cell.borrow_mut();
            // Safety: callers verified UTF-8 (coderange or full scan).
            unsafe { nosj::parse_utf8_unchecked_with(input, &mut state.bufs, &mut sink, po.popts) }
        });
        match result {
            Ok(()) => finish_string(&buf),
            Err(nosj::DriveError::Sink(SinkAbort::TooDeep)) => Err(nesting_error(
                ruby,
                format!(
                    "nesting of {} is too deep",
                    po.max_nesting.saturating_add(1)
                ),
            )),
            Err(nosj::DriveError::Sink(SinkAbort::BrokenUtf8Output)) => Err(Error::new(
                // Gem parity: generate raises GeneratorError for a
                // string ascii_only cannot represent.
                nosj_exception(ruby, "GeneratorError"),
                "source sequence is illegal/malformed utf-8",
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
    let input = utf8_input(ruby, &data)?;
    reformat_over(ruby, input, opts)
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
    // Mapping a zero-length file fails with EINVAL on Linux; route an
    // empty file to the parser's own "unexpected end of input" so the
    // error class is deterministic across platforms. Metadata failures
    // fall through for the mapper's Errno.
    if std::fs::metadata(&p).is_ok_and(|m| m.len() == 0) {
        return reformat_over(ruby, &[], opts);
    }
    with_mapped_file(ruby, &p, |map| reformat_over(ruby, &map, opts))
}

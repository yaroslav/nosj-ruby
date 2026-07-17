//! JSON generation: walks a Ruby object graph and emits JSON bytes through
//! nosj's SIMD emission kernels (escape scanning, Ruby-format floats).
//! Matches the JSON gem's `generate` semantics: same output bytes, same
//! error classes and messages, same options.
//!
//! - `opts.rs`: option-hash decoding into [`opts::GenConfig`].
//! - `walker.rs`: the recursive emitter (compact/pretty const-generic).
//! - `keys.rs`: the pre-escaped, VALUE-identity key cache.
//! - `ruby.rs`: `rb_protect` shims and inline RBasic flag readers.
//! - `errors.rs`: failure carriers mapped onto the gem's exceptions.
//!
//! Tried and rejected (2026-07-15, measured on Zen 4 AND Apple
//! Silicon): emitting through the crate's `EmitBuf` trait straight
//! into the returned Ruby String (both a pure heap-String buffer and a
//! stack-prefix hybrid with last-output-size spill hints). It removes
//! the final `rb_utf8_str_new` copy, and twitter generation won one
//! roll on Zen 4 (0.96), but the per-call multi-MB String allocation
//! loses to this warm pooled Vec everywhere else: ARM gsoc generation
//! regressed a deterministic 1.42x (13/13 wins fell to 10/13), x86
//! tiny documents up to 1.29x. The final copy was never the expensive
//! part; the warm buffer is.

mod errors;
mod hash_iter;
mod keys;
mod opts;
mod ruby;
mod walker;

use magnus::rb_sys::{AsRawValue, FromRawValue};
use magnus::{Error, RString, Ruby, Value};
use std::cell::RefCell;

use errors::raise_fail;
use keys::GenKeyCache;
use walker::Gen;

/// Per-thread generate scratch: the pooled output buffer (capacity
/// survives across calls) and the pre-escaped key cache. One
/// thread_local, borrowed ONCE for the whole call: in a shared library
/// every thread_local access is a `__tls_get_addr` call, and the old
/// take/put-back shape paid two of them plus struct moves per call
/// (measurable on 45-byte documents). A recursive generate via a user
/// `to_json` hits the failed `try_borrow_mut` arm and runs on a fresh
/// scratch instead of panicking the RefCell.
struct GenScratch {
    buf: Vec<u8>,
    keys: GenKeyCache,
    /// Keys pre-escaped under HtmlSafe (the Rails encoder): cached
    /// bytes bake in the escape mode, so each cacheable mode owns a
    /// cache (see walker::mode_cacheable).
    html_keys: GenKeyCache,
}

thread_local! {
    static GEN_SCRATCH: RefCell<GenScratch> = RefCell::new(GenScratch {
        buf: Vec::new(),
        keys: GenKeyCache::with_capacity(256),
        html_keys: GenKeyCache::with_capacity(64),
    });
}

/// `NOSJ.generate(obj, opts = nil)`, registered as a variadic native
/// method: no Ruby forwarder frame, and the nil-options path borrows
/// the shared [`opts::DEFAULT_CONFIG`] instead of building a config.
pub fn generate_entry(ruby: &Ruby, _rb_self: Value, args: &[Value]) -> Result<RString, Error> {
    use magnus::value::ReprValue;
    let (obj, opts) = match *args {
        [obj] => (obj, None),
        [obj, opts] if opts.is_nil() => (obj, None),
        [obj, opts] => (obj, Some(opts)),
        _ => {
            return Err(Error::new(
                ruby.exception_arg_error(),
                format!(
                    "wrong number of arguments (given {}, expected 1..2)",
                    args.len()
                ),
            ));
        }
    };
    let built;
    let (cfg, cap_hint): (&opts::GenConfig, usize) = match opts {
        None => (&opts::DEFAULT_CONFIG, 0),
        Some(o) => {
            let (c, hint) = opts::parse_gen_opts(ruby, o)?;
            built = c;
            (&built, hint)
        }
    };
    generate_scratched(ruby, obj, cfg, cap_hint)
}

/// `NOSJ.generate_rails_native(obj, escape_html, escape_js)`: compact
/// generation under ActiveSupport walk semantics (non-native values
/// recurse through as_json, non-finite floats emit null). Every escape
/// flag combination maps to a crate escape mode, so HTML/JS-safety
/// escaping is always fused into the emission kernels: one pass, no
/// post-processing. The Rails encoder installed by
/// `require "nosj/rails"` is the only caller.
pub fn generate_rails_native(
    ruby: &Ruby,
    _rb_self: Value,
    obj: Value,
    escape_html: bool,
    escape_js: bool,
) -> Result<RString, Error> {
    let cfg = match (escape_html, escape_js) {
        (true, true) => &opts::RAILS_HTML_SAFE_CONFIG,
        (true, false) => &opts::RAILS_HTML_ENTITIES_CONFIG,
        (false, true) => &opts::RAILS_JS_SEPARATORS_CONFIG,
        (false, false) => &opts::RAILS_CONFIG,
    };
    generate_scratched(ruby, obj, cfg, 0)
}

/// `NOSJ.generate_native(obj, opts)`: the fixed-arity entry kept for
/// the Ruby-level wrappers (pretty_generate merges options first).
pub fn generate_native(
    ruby: &Ruby,
    _rb_self: Value,
    obj: Value,
    opts: Value,
) -> Result<RString, Error> {
    use magnus::value::ReprValue;
    if opts.is_nil() {
        return generate_scratched(ruby, obj, &opts::DEFAULT_CONFIG, 0);
    }
    let (cfg, cap_hint) = opts::parse_gen_opts(ruby, opts)?;
    generate_scratched(ruby, obj, &cfg, cap_hint)
}

/// Generate `obj` under a Ruby options hash (or nil) and hand the
/// finished bytes to `finish` instead of building a Ruby String: the
/// entry `NOSJ.write_file` uses to stream straight to disk.
pub(crate) fn generate_bytes_into<R>(
    ruby: &Ruby,
    obj: Value,
    opts: Value,
    finish: impl FnOnce(&Ruby, &[u8]) -> Result<R, Error>,
) -> Result<R, Error> {
    use magnus::value::ReprValue;
    if opts.is_nil() {
        return generate_scratched_into(ruby, obj, &opts::DEFAULT_CONFIG, 0, finish);
    }
    let (cfg, cap_hint) = opts::parse_gen_opts(ruby, opts)?;
    generate_scratched_into(ruby, obj, &cfg, cap_hint, finish)
}

fn generate_scratched(
    ruby: &Ruby,
    obj: Value,
    cfg: &opts::GenConfig,
    cap_hint: usize,
) -> Result<RString, Error> {
    generate_scratched_into(ruby, obj, cfg, cap_hint, finish_rstring)
}

/// The default finisher: the generated bytes as a Ruby String.
fn finish_rstring(_ruby: &Ruby, out: &[u8]) -> Result<RString, Error> {
    Ok(unsafe {
        RString::from_value(Value::from_raw(rb_sys::rb_utf8_str_new(
            out.as_ptr().cast(),
            out.len() as std::os::raw::c_long,
        )))
        .expect("rb_utf8_str_new returns a String")
    })
}

fn generate_scratched_into<R>(
    ruby: &Ruby,
    obj: Value,
    cfg: &opts::GenConfig,
    cap_hint: usize,
    finish: impl FnOnce(&Ruby, &[u8]) -> Result<R, Error>,
) -> Result<R, Error> {
    GEN_SCRATCH.with(|cell| match cell.try_borrow_mut() {
        Ok(mut scratch) => {
            let scratch = &mut *scratch;
            let GenScratch {
                buf,
                keys,
                html_keys,
                ..
            } = scratch;
            let keys = if cfg.mode == nosj::emit::EscapeMode::HtmlSafe {
                html_keys
            } else {
                keys
            };
            generate_with(ruby, obj, cfg, cap_hint, buf, keys, finish)
        }
        Err(_) => {
            let mut buf = Vec::new();
            let mut keys = GenKeyCache::default();
            generate_with(ruby, obj, cfg, cap_hint, &mut buf, &mut keys, finish)
        }
    })
}

fn generate_with<R>(
    ruby: &Ruby,
    obj: Value,
    cfg: &opts::GenConfig,
    cap_hint: usize,
    out: &mut Vec<u8>,
    keys: &mut GenKeyCache,
    finish: impl FnOnce(&Ruby, &[u8]) -> Result<R, Error>,
) -> Result<R, Error> {
    out.clear();
    if out.capacity() < cap_hint {
        out.reserve(cap_hint);
    }
    emit_one(ruby, obj, cfg, out, keys)?;
    finish(ruby, out)
}

/// Emit one value into `out` (appending, not clearing), mapping walker
/// failures onto the gem's exceptions. `inline(always)`: this is the
/// single-document hot path's only call layer.
#[inline(always)]
fn emit_one(
    ruby: &Ruby,
    obj: Value,
    cfg: &opts::GenConfig,
    out: &mut Vec<u8>,
    keys: &mut GenKeyCache,
) -> Result<(), Error> {
    let mut g = Gen {
        out,
        cfg,
        fail: None,
        keys,
    };
    let result = if cfg.pretty {
        g.emit_value::<true>(obj.as_raw(), cfg.start_depth)
    } else {
        g.emit_value::<false>(obj.as_raw(), cfg.start_depth)
    };

    let Gen { fail, .. } = g;
    match (result, fail) {
        (Ok(()), None) => Ok(()),
        (_, Some(fail)) => Err(raise_fail(ruby, fail)),
        (Err(()), None) => Err(Error::new(
            ruby.exception_runtime_error(),
            "generation failed without error detail",
        )),
    }
}

/// Formatting strings holding a newline (or carriage return) would
/// split one value across NDJSON lines; the lines entries refuse them.
fn breaks_line_framing(cfg: &opts::GenConfig) -> bool {
    [
        &cfg.indent,
        &cfg.space,
        &cfg.space_before,
        &cfg.object_nl,
        &cfg.array_nl,
    ]
    .into_iter()
    .any(|v| v.iter().any(|&b| b == b'\n' || b == b'\r'))
}

/// `NOSJ.generate_lines_native(values, opts)`: NDJSON generation, one
/// compact document per element into ONE pooled buffer, newline after
/// each (every line terminated, per the format).
pub fn generate_lines_native(
    ruby: &Ruby,
    _rb_self: Value,
    values: magnus::RArray,
    opts: Value,
) -> Result<RString, Error> {
    generate_lines_bytes_into(ruby, values, opts, finish_rstring)
}

/// The write-to-disk form of [`generate_lines_native`], used by
/// `NOSJ.write_lines`.
pub(crate) fn generate_lines_bytes_into<R>(
    ruby: &Ruby,
    values: magnus::RArray,
    opts: Value,
    finish: impl FnOnce(&Ruby, &[u8]) -> Result<R, Error>,
) -> Result<R, Error> {
    use magnus::value::ReprValue;
    let built;
    let (cfg, cap_hint): (&opts::GenConfig, usize) = if opts.is_nil() {
        (&opts::DEFAULT_CONFIG, 0)
    } else {
        let (c, hint) = opts::parse_gen_opts(ruby, opts)?;
        built = c;
        (&built, hint)
    };
    if breaks_line_framing(cfg) {
        return Err(Error::new(
            ruby.exception_arg_error(),
            "formatting options containing newlines would break JSON Lines framing",
        ));
    }
    GEN_SCRATCH.with(|cell| match cell.try_borrow_mut() {
        Ok(mut scratch) => {
            let scratch = &mut *scratch;
            let GenScratch { buf, keys, .. } = scratch;
            generate_lines_with(ruby, values, cfg, cap_hint, buf, keys, finish)
        }
        Err(_) => {
            let mut buf = Vec::new();
            let mut keys = GenKeyCache::default();
            generate_lines_with(ruby, values, cfg, cap_hint, &mut buf, &mut keys, finish)
        }
    })
}

fn generate_lines_with<R>(
    ruby: &Ruby,
    values: magnus::RArray,
    cfg: &opts::GenConfig,
    cap_hint: usize,
    out: &mut Vec<u8>,
    keys: &mut GenKeyCache,
    finish: impl FnOnce(&Ruby, &[u8]) -> Result<R, Error>,
) -> Result<R, Error> {
    out.clear();
    if out.capacity() < cap_hint {
        out.reserve(cap_hint);
    }
    for i in 0..values.len() {
        let obj: Value = values.entry(i as isize)?;
        emit_one(ruby, obj, cfg, out, keys)?;
        out.push(b'\n');
    }
    finish(ruby, out)
}

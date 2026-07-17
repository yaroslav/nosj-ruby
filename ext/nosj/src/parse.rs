//! Whole-document entry points: `NOSJ.parse` (fused cursor),
//! `NOSJ.valid?` (null sink), and the unregistered GVL-releasing
//! indexed parse, plus the shared option decoding, input gating, and
//! drive-result mapping they all use.

use magnus::rb_sys::{AsRawValue, FromRawValue};
use magnus::{Error, RString, Ruby, Value};

use crate::errors::{nesting_error, parser_error, parser_error_at};
use crate::sink::{NullSink, RubyValueSink, SinkAbort, MAX_NESTING};
use crate::state::{ensure_marked_shadow, PullState, PULL_STATE};

pub(crate) use crate::errors::parser_error as err;

/// Byte range of `sub` within `source`. `sub` must be a subslice of
/// `source` (it always is here: pointer resolution and lazy spans hand
/// out slices borrowed from the document they resolved against).
pub(crate) fn span_of(source: &[u8], sub: &[u8]) -> (usize, usize) {
    let start = sub.as_ptr() as usize - source.as_ptr() as usize;
    (start, start + sub.len())
}

/// Validate that `data` is UTF-8 (or US-ASCII) with intact coderange and
/// hand out its byte slice.
pub(crate) fn utf8_input<'a>(ruby: &Ruby, data: &'a RString) -> Result<&'a [u8], Error> {
    let raw = data.as_raw();
    unsafe {
        let enc = rb_sys::rb_enc_get_index(raw);
        if enc != rb_sys::rb_utf8_encindex() && enc != rb_sys::rb_usascii_encindex() {
            return Err(err(ruby, "input must be UTF-8 encoded".into()));
        }
        if rb_sys::rb_enc_str_coderange(raw)
            == rb_sys::ruby_coderange_type::RUBY_ENC_CODERANGE_BROKEN as std::os::raw::c_int
        {
            return Err(err(ruby, "input is not valid UTF-8".into()));
        }
        Ok(data.as_slice())
    }
}

/// JSON.parse-compatible options, decoded once from the Ruby hash and
/// shared by every entry point that materializes (or validates) values.
#[derive(Clone, Copy)]
pub(crate) struct ParseNativeOpts {
    pub(crate) symbolize: bool,
    pub(crate) freeze: bool,
    pub(crate) max_nesting: usize,
    pub(crate) popts: nosj::ParseOptions,
}

impl Default for ParseNativeOpts {
    fn default() -> Self {
        Self {
            symbolize: false,
            freeze: false,
            max_nesting: MAX_NESTING,
            popts: nosj::ParseOptions::default(),
        }
    }
}

/// Decode a JSON.parse-compatible options hash: symbolize_names, freeze,
/// max_nesting, allow_nan, allow_trailing_comma. Unsupported gem options
/// (object_class, array_class, decimal_class, create_additions) raise.
pub(crate) fn parse_native_opts(ruby: &Ruby, opts: Value) -> Result<ParseNativeOpts, Error> {
    use magnus::value::ReprValue;
    use magnus::RHash;

    let mut out = ParseNativeOpts::default();
    if opts.is_nil() {
        return Ok(out);
    }
    let h = RHash::from_value(opts)
        .ok_or_else(|| Error::new(ruby.exception_arg_error(), "options must be a Hash"))?;

    let truthy = |name: &str| -> bool {
        h.get(ruby.to_symbol(name))
            .is_some_and(|v: Value| v.to_bool())
    };

    out.symbolize = truthy("symbolize_names");
    out.freeze = truthy("freeze");
    out.popts.allow_nan = truthy("allow_nan");
    out.popts.allow_trailing_comma = truthy("allow_trailing_comma");

    if let Some(mn) = h.get(ruby.to_symbol("max_nesting")) {
        out.max_nesting =
            if mn.is_nil() || mn.to_bool() && magnus::Integer::from_value(mn).is_none() {
                MAX_NESTING // nil / true: gem default
            } else if !mn.to_bool() {
                usize::MAX // false: unlimited
            } else {
                magnus::Integer::from_value(mn)
                    .and_then(|i| i.to_u64().ok())
                    .map_or(MAX_NESTING, |n| n as usize)
            };
    }

    for unsupported in [
        "object_class",
        "array_class",
        "decimal_class",
        "create_additions",
    ] {
        if truthy(unsupported) {
            return Err(Error::new(
                ruby.exception_arg_error(),
                format!("NOSJ.parse does not support the {unsupported} option"),
            ));
        }
    }
    Ok(out)
}

/// Pop the root value off the sink stack, or map a drive failure onto
/// the gem's exceptions. Shared by every driver. `source`/`base` locate
/// the driven bytes within the full document, so ParserError positions
/// stay absolute when a subtree slice was parsed.
fn finish_drive(
    ruby: &Ruby,
    result: Result<(), nosj::DriveError<SinkAbort>>,
    stack: &mut Vec<rb_sys::VALUE>,
    max_nesting: usize,
    source: &[u8],
    base: usize,
) -> Result<Value, Error> {
    match result {
        Ok(()) => {
            let raw = stack
                .pop()
                .unwrap_or(rb_sys::special_consts::Qnil as rb_sys::VALUE);
            Ok(unsafe { Value::from_raw(raw) })
        }
        Err(nosj::DriveError::Sink(SinkAbort::Overflow)) => {
            Err(parser_error(ruby, "document too large".into()))
        }
        Err(nosj::DriveError::Sink(SinkAbort::BadBigint)) => {
            Err(parser_error(ruby, "invalid bignum".into()))
        }
        Err(nosj::DriveError::Sink(SinkAbort::TooDeep)) => Err(nesting_error(
            ruby,
            format!("nesting of {} is too deep", max_nesting.saturating_add(1)),
        )),
        Err(nosj::DriveError::Parse(e)) => Err(parser_error_at(
            ruby,
            source,
            base + e.offset,
            e.to_string(),
        )),
    }
}

/// Drive the fused cursor over the whole of `source`. See
/// [`materialize_at`].
pub(crate) fn materialize(ruby: &Ruby, source: &[u8], o: &ParseNativeOpts) -> Result<Value, Error> {
    materialize_at(ruby, source, 0, source.len(), o)
}

/// Drive the fused cursor over `source[start..end]`, building Ruby
/// values through the shared thread-local sink machinery. `source` must
/// be valid UTF-8 (see [`utf8_input`]); the full document is passed so
/// error positions come out absolute.
pub(crate) fn materialize_at(
    ruby: &Ruby,
    source: &[u8],
    start: usize,
    end: usize,
    o: &ParseNativeOpts,
) -> Result<Value, Error> {
    PULL_STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        ensure_marked_shadow(&mut state.vstack);
        ensure_marked_shadow(&mut state.key_shadow);

        let PullState {
            bufs,
            keys,
            sym_keys,
            vstack,
            key_shadow,
            ..
        } = &mut *state;
        let stack = &mut vstack.as_mut().unwrap().values;
        stack.clear();

        let mut sink = RubyValueSink {
            stack,
            keys: if o.symbolize { sym_keys } else { keys },
            key_shadow: key_shadow.as_deref_mut().unwrap(),
            depth: 0,
            symbolize: o.symbolize,
            freeze: o.freeze,
            max_nesting: o.max_nesting,
        };

        // Safety: callers verified UTF-8 (coderange or nosj slice).
        let result = unsafe {
            nosj::parse_utf8_unchecked_with(&source[start..end], bufs, &mut sink, o.popts)
        };
        finish_drive(ruby, result, sink.stack, o.max_nesting, source, start)
    })
}

/// JSON.parse-compatible entry (see [`parse_native_opts`] for options).
pub fn parse_native(
    ruby: &Ruby,
    _rb_self: Value,
    data: RString,
    opts: Value,
) -> Result<Value, Error> {
    let o = parse_native_opts(ruby, opts)?;
    let input = utf8_input(ruby, &data)?;
    materialize(ruby, input, &o)
}

/// `NOSJ.valid?(source, opts)` returns true iff `NOSJ.parse` would
/// succeed under the same options. Parse refusals (malformed JSON, bad
/// encoding, too-deep nesting) return false; option and argument-type
/// errors still raise exactly like `parse`.
pub fn valid_native(
    ruby: &Ruby,
    _rb_self: Value,
    data: RString,
    opts: Value,
) -> Result<bool, Error> {
    let o = parse_native_opts(ruby, opts)?;
    let Ok(input) = utf8_input(ruby, &data) else {
        return Ok(false);
    };
    let ok = PULL_STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        let mut sink = NullSink {
            depth: 0,
            max_nesting: o.max_nesting,
        };
        // Safety: coderange verified by utf8_input.
        unsafe { nosj::parse_utf8_unchecked_with(input, &mut state.bufs, &mut sink, o.popts) }
            .is_ok()
    });
    Ok(ok)
}

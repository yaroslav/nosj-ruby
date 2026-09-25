//! Whole-document entry points: `NOSJ.parse` (fused cursor),
//! `NOSJ.valid?` (null sink), and the unregistered GVL-releasing
//! indexed parse, plus the shared option decoding, input gating, and
//! drive-result mapping they all use.

use magnus::rb_sys::{AsRawValue, FromRawValue};
use magnus::{Error, RString, Ruby, Value};

use crate::errors::{nesting_error, nosj_exception, parser_error, parser_error_at};
use crate::locate::Repeat;
use crate::opt_reader::{Opt, OptReader};
use crate::sink::{DupKeys, NullSink, RubyValueSink, SinkAbort, MAX_NESTING};
use crate::state::{ensure_marked_shadow, with_pull_state, PullState};

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
///
/// The slice borrows the string's buffer, which Ruby may reallocate or
/// swap (even for a frozen string: see `lazy::DocBytes`), so it must
/// not be held across anything that can run Ruby code: user callbacks,
/// yields, or option decoding (`to_int` and friends). Decode options
/// first; re-borrow after callbacks.
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
    /// json 3 default: a repeated key raises.
    pub(crate) allow_duplicate_key: bool,
    pub(crate) popts: nosj::ParseOptions,
}

impl Default for ParseNativeOpts {
    fn default() -> Self {
        Self {
            symbolize: false,
            freeze: false,
            max_nesting: MAX_NESTING,
            allow_duplicate_key: false,
            popts: nosj::ParseOptions::default(),
        }
    }
}

/// Decode a JSON.parse-compatible options hash (see [`read_parse_opts`]);
/// keys it does not read raise ArgumentError, like json 3.
pub(crate) fn parse_native_opts(ruby: &Ruby, opts: Value) -> Result<ParseNativeOpts, Error> {
    let Some(h) = options_hash(ruby, opts)? else {
        return Ok(ParseNativeOpts::default());
    };
    let mut reader = OptReader::new(ruby, h);
    let out = read_parse_opts(&mut reader)?;
    reader.finish()?;
    Ok(out)
}

/// The non-empty options Hash in `opts`, or None for nil and `{}`
/// (json 3's keyword-only parse hands the drop-in an empty hash per
/// call).
pub(crate) fn options_hash(ruby: &Ruby, opts: Value) -> Result<Option<magnus::RHash>, Error> {
    use magnus::value::ReprValue;
    if opts.is_nil() {
        return Ok(None);
    }
    let h = magnus::RHash::from_value(opts)
        .ok_or_else(|| Error::new(ruby.exception_arg_error(), "options must be a Hash"))?;
    Ok((!h.is_empty()).then_some(h))
}

/// Read symbolize_names, freeze, max_nesting, allow_nan,
/// allow_trailing_comma and allow_duplicate_key. The json options NOSJ
/// does not implement (object_class, array_class, decimal_class,
/// on_load, create_additions, allow_comments, allow_control_characters,
/// allow_invalid_escape) raise unless falsy.
pub(crate) fn read_parse_opts(r: &mut OptReader) -> Result<ParseNativeOpts, Error> {
    let mut out = ParseNativeOpts {
        symbolize: r.truthy(Opt::SymbolizeNames),
        freeze: r.truthy(Opt::Freeze),
        allow_duplicate_key: r.truthy(Opt::AllowDuplicateKey),
        ..ParseNativeOpts::default()
    };
    out.popts.allow_nan = r.truthy(Opt::AllowNan);
    out.popts.allow_trailing_comma = r.truthy(Opt::AllowTrailingComma);

    if let Some(mn) = r.get(Opt::MaxNesting) {
        out.max_nesting = max_nesting_of(mn);
    }

    r.tolerate(&[
        Opt::ObjectClass,
        Opt::ArrayClass,
        Opt::DecimalClass,
        Opt::OnLoad,
        Opt::CreateAdditions,
        Opt::AllowComments,
        Opt::AllowControlCharacters,
        Opt::AllowInvalidEscape,
    ]);
    Ok(out)
}

/// A given `max_nesting` value as a limit: nil or true keep the gem's
/// default, false is unlimited, an Integer is the limit.
pub(crate) fn max_nesting_of(value: Value) -> usize {
    use magnus::value::ReprValue;
    if value.is_nil() || value.to_bool() && magnus::Integer::from_value(value).is_none() {
        MAX_NESTING
    } else if !value.to_bool() {
        usize::MAX
    } else {
        magnus::Integer::from_value(value)
            .and_then(|i| i.to_u64().ok())
            .map_or(MAX_NESTING, |n| n as usize)
    }
}

pub(crate) type DriveResult = Result<(), nosj::DriveError<SinkAbort>>;

/// A drive's failure as the gem's exception; shared by every driver.
/// `source[start..end]` is what was driven, within the full document,
/// so ParserError positions stay absolute when a subtree slice was
/// parsed. Sinks see no offsets, so their two document refusals are
/// positioned by a cold-path re-walk (`crate::locate`).
pub(crate) fn drive_error(
    ruby: &Ruby,
    failure: nosj::DriveError<SinkAbort>,
    o: &ParseNativeOpts,
    source: &[u8],
    (start, end): (usize, usize),
) -> Error {
    use magnus::value::ReprValue;
    use nosj::DriveError::{Parse, Sink};

    let driven = &source[start..end];
    match failure {
        Sink(SinkAbort::Overflow) => parser_error(ruby, "document too large".into()),
        Sink(SinkAbort::BadBigint) => parser_error(ruby, "invalid bignum".into()),
        Sink(SinkAbort::TooDeep) => nesting_error(
            ruby,
            format!("nesting of {} is too deep", o.max_nesting.saturating_add(1)),
        ),
        // Positioned like json 3's: at the `{` of the object repeating
        // the key.
        Sink(SinkAbort::DuplicateKey) => match crate::locate::duplicate_key(driven, o.popts) {
            Repeat::Found { at, key } => parser_error_at(
                ruby,
                source,
                start + at,
                format!(
                    "duplicate key {} at byte {at}",
                    ruby.str_new(&key).inspect()
                ),
            ),
            Repeat::Absent | Repeat::Undecided => parser_error(ruby, "duplicate key".into()),
        },
        Sink(SinkAbort::LoneSurrogate) => match crate::locate::first_walk_error(driven, o.popts) {
            Some(e) => parser_error_at(ruby, source, start + e.offset, e.to_string()),
            None => parser_error(ruby, "lone UTF-16 surrogate".into()),
        },
        // Only the reformat pipe raises it: generation refuses a
        // non-finite float, with the gem's GeneratorError.
        Sink(SinkAbort::NonFiniteFloat(spelling)) => Error::new(
            nosj_exception(ruby, "GeneratorError"),
            format!("{spelling} not allowed in JSON"),
        ),
        Parse(e) => parser_error_at(ruby, source, start + e.offset, e.to_string()),
    }
}

/// Drive a sink that builds no Hash (`valid?`, the reformat pipe) under
/// json 3's duplicate-key rule. `drive(check_dups)` runs the parse. With
/// the check on, `DupKeys` compares key fingerprints, so a refusal is
/// confirmed exactly, and a collision (no object really repeats a key)
/// drives again with the check off.
pub(crate) fn drive_hashless(
    input: &[u8],
    o: &ParseNativeOpts,
    mut drive: impl FnMut(bool) -> DriveResult,
) -> DriveResult {
    let result = drive(!o.allow_duplicate_key);
    if matches!(result, Err(nosj::DriveError::Sink(SinkAbort::DuplicateKey)))
        && matches!(crate::locate::duplicate_key(input, o.popts), Repeat::Absent)
    {
        return drive(false);
    }
    result
}

/// Pop the root value off the sink stack, or raise the drive's failure
/// (see [`drive_error`]).
fn finish_drive(
    ruby: &Ruby,
    result: DriveResult,
    stack: &mut Vec<rb_sys::VALUE>,
    o: &ParseNativeOpts,
    source: &[u8],
    span: (usize, usize),
) -> Result<Value, Error> {
    result.map_err(|failure| drive_error(ruby, failure, o, source, span))?;
    let raw = stack
        .pop()
        .unwrap_or(rb_sys::special_consts::Qnil as rb_sys::VALUE);
    Ok(unsafe { Value::from_raw(raw) })
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
    with_pull_state(|state| {
        ensure_marked_shadow(&mut state.vstack);
        ensure_marked_shadow(&mut state.key_shadow);

        let PullState {
            bufs,
            keys,
            sym_keys,
            vstack,
            key_shadow,
            ..
        } = state;
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
            allow_duplicate_key: o.allow_duplicate_key,
        };

        // Safety: callers verified UTF-8 (coderange or nosj slice).
        let result = unsafe {
            nosj::parse_utf8_unchecked_with(&source[start..end], bufs, &mut sink, o.popts)
        };
        finish_drive(ruby, result, sink.stack, o, source, (start, end))
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
    let result = drive_hashless(input, &o, |check_dups| {
        with_pull_state(|state| {
            let mut sink = NullSink {
                depth: 0,
                max_nesting: o.max_nesting,
                dup_keys: DupKeys::new(&mut state.dup, check_dups),
            };
            // Safety: coderange verified by utf8_input.
            unsafe { nosj::parse_utf8_unchecked_with(input, &mut state.bufs, &mut sink, o.popts) }
        })
    });
    Ok(result.is_ok())
}

//! Parse-side exception raising. Malformed documents raise
//! `NOSJ::ParserError` carrying the failure position (`#byte_offset`,
//! `#line`, `#column`) and a caret snippet (`#snippet`); too-deep
//! nesting raises `NOSJ::NestingError`, matching gem json's classes.
//!
//! Everything here runs only when a parse has already failed: the
//! position pass over the source and the exception construction cost
//! nothing on the happy path, and the accessors on the Ruby side are
//! plain ivar reads.

use magnus::value::ReprValue;
use magnus::{Class, Error, ExceptionClass, Module, Object, RModule, RObject, Ruby};

/// Look up an exception class under NOSJ, falling back to RuntimeError
/// if the Ruby layer has not defined it (only possible when the native
/// extension is loaded without lib/nosj.rb).
pub(crate) fn nosj_exception(ruby: &Ruby, name: &str) -> ExceptionClass {
    let lookup = || -> Result<ExceptionClass, Error> {
        let m: RModule = ruby.define_module("NOSJ")?;
        m.const_get(name)
    };
    lookup().unwrap_or_else(|_| ruby.exception_runtime_error())
}

/// NOSJ::ParserError without position info (encoding refusals and
/// other failures that have no meaningful offset).
#[cold]
pub(crate) fn parser_error(ruby: &Ruby, msg: String) -> Error {
    Error::new(nosj_exception(ruby, "ParserError"), msg)
}

/// NOSJ::NestingError (parse side: message parity with gem json, which
/// raises JSON::NestingError when max_nesting is exceeded).
#[cold]
pub(crate) fn nesting_error(ruby: &Ruby, msg: String) -> Error {
    Error::new(nosj_exception(ruby, "NestingError"), msg)
}

/// Plain RuntimeError, for failures that are not parse errors (the
/// unmappable-I/O fallback).
#[cold]
pub(crate) fn runtime_error(ruby: &Ruby, msg: String) -> Error {
    Error::new(ruby.exception_runtime_error(), msg)
}

/// NOSJ::ParserError enriched with the failure position: `offset` is a
/// byte offset into `source` (the full document, so positions stay
/// absolute even when the failing parse ran over a subtree slice).
#[cold]
pub(crate) fn parser_error_at(ruby: &Ruby, source: &[u8], offset: usize, msg: String) -> Error {
    let class = nosj_exception(ruby, "ParserError");
    let loc = locate(source, offset);
    let Ok(exc) = class.new_instance((msg.as_str(),)) else {
        return Error::new(class, msg);
    };
    // Exception instances are plain T_OBJECTs; magnus exposes ivar_set
    // through RObject, not through its Exception wrapper.
    if let Some(obj) = RObject::from_value(exc.as_value()) {
        let set = |name: &str, v: usize| {
            let _ = obj.ivar_set(name, v);
        };
        set("@byte_offset", offset.min(source.len()));
        set("@line", loc.line);
        set("@column", loc.column);
        if let Some(snippet) = &loc.snippet {
            let _ = obj.ivar_set("@snippet", snippet.as_str());
        }
    }
    Error::from(exc)
}

/// A resolved source position: 1-based line, 1-based character column
/// within that line, and a two-line caret snippet (`None` when the
/// offending line is empty or not valid UTF-8).
struct Location {
    line: usize,
    column: usize,
    snippet: Option<String>,
}

/// UTF-8 continuation bytes are 0b10xxxxxx; every other byte starts a
/// character, so counting non-continuation bytes counts characters.
const UTF8_CONTINUATION_MASK: u8 = 0b1100_0000;
const UTF8_CONTINUATION_BITS: u8 = 0b1000_0000;

fn count_chars(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .filter(|&&b| b & UTF8_CONTINUATION_MASK != UTF8_CONTINUATION_BITS)
        .count()
}

fn locate(source: &[u8], offset: usize) -> Location {
    let off = offset.min(source.len());
    let line = 1 + count_newlines(&source[..off]);
    let line_start = source[..off]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |p| p + 1);
    let mut line_end = source[off..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(source.len(), |p| off + p);
    if line_end > line_start && source[line_end - 1] == b'\r' {
        line_end -= 1;
    }
    // An offset sitting on the line terminator itself carets one past
    // the last character of the line's content.
    let caret = off.clamp(line_start, line_end);
    let column = 1 + count_chars(&source[line_start..caret]);
    let snippet = std::str::from_utf8(&source[line_start..line_end])
        .ok()
        .and_then(|l| build_snippet(l, caret - line_start));
    Location {
        line,
        column,
        snippet,
    }
}

fn count_newlines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| b == b'\n').count()
}

/// Characters kept around the caret. Minified JSON is routinely one
/// multi-kilobyte line, so the snippet shows a window, not the line.
const SNIPPET_CHARS_BEFORE: usize = 50;
const SNIPPET_CHARS_AFTER: usize = 30;
const SNIPPET_ELLIPSIS: char = '…';

/// Render the offending line plus a caret line underneath, windowed
/// around `caret_byte` (a byte offset into `line`).
fn build_snippet(line: &str, caret_byte: usize) -> Option<String> {
    if line.is_empty() {
        return None;
    }
    let mut caret_byte = caret_byte.min(line.len());
    while !line.is_char_boundary(caret_byte) {
        caret_byte -= 1;
    }
    let caret_chars = line[..caret_byte].chars().count();
    let total_chars = caret_chars + line[caret_byte..].chars().count();

    let window_first = caret_chars.saturating_sub(SNIPPET_CHARS_BEFORE);
    let window_last = (caret_chars + SNIPPET_CHARS_AFTER).min(total_chars);
    let mut caret_column = caret_chars - window_first;

    let mut out = String::new();
    if window_first > 0 {
        out.push(SNIPPET_ELLIPSIS);
        caret_column += 1;
    }
    for ch in line
        .chars()
        .skip(window_first)
        .take(window_last - window_first)
    {
        // Control characters (tabs included) render as one space so
        // the caret column stays aligned with what is printed.
        out.push(if ch.is_control() { ' ' } else { ch });
    }
    if window_last < total_chars {
        out.push(SNIPPET_ELLIPSIS);
    }
    out.push('\n');
    for _ in 0..caret_column {
        out.push(' ');
    }
    out.push('^');
    Some(out)
}

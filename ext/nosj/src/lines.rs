//! NDJSON / JSON Lines: `NOSJ.each_line` walks a newline-delimited
//! document yielding one parsed value per line, and the file form does
//! it over a read-only memory map. A raw newline can never occur
//! INSIDE a JSON value (control characters must be escaped), so line
//! splitting is exact framing, and one-value-per-line is what the
//! format requires: a second value on a line fails as trailing
//! garbage, with an absolute document position on the error.

use magnus::value::ReprValue;
use magnus::{Error, RString, Ruby, Value};

use crate::files::with_mapped_file;
use crate::parse::{materialize_at, parse_native_opts, utf8_input, ParseNativeOpts};

/// Blank-line whitespace (the newline itself is the separator). Lines
/// holding only this are skipped, per the NDJSON convention that
/// parsers ignore empty lines (trailing newlines at EOF are universal).
const LINE_WS: [u8; 3] = *b" \t\r";

fn blank(line: &[u8]) -> bool {
    line.iter().all(|b| LINE_WS.contains(b))
}

/// Yield one parsed value per non-blank line. Each line parses through
/// the shared sink machinery against the FULL source, so a malformed
/// line raises the rich ParserError whose `#line` is the physical
/// NDJSON line number. The thread state is borrowed per line, never
/// across a yield: the block is free to call back into NOSJ.
fn walk_lines(ruby: &Ruby, source: &[u8], o: &ParseNativeOpts) -> Result<(), Error> {
    let mut pos = 0;
    while pos < source.len() {
        let line_end = source[pos..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(source.len(), |p| pos + p);
        if !blank(&source[pos..line_end]) {
            let value = materialize_at(ruby, source, pos, line_end, o)?;
            let _: Value = ruby.yield_value(value)?;
        }
        pos = line_end + 1;
    }
    Ok(())
}

/// `NOSJ.each_line(source, opts) { |value| }`: the Ruby wrapper
/// guarantees a block (and builds the Enumerator otherwise).
pub fn each_line_native(
    ruby: &Ruby,
    _rb_self: Value,
    data: RString,
    opts: Value,
) -> Result<Value, Error> {
    let o = parse_native_opts(ruby, opts)?;
    let input = utf8_input(ruby, &data)?;
    if data.as_value().is_frozen() {
        // A frozen source cannot be mutated by the block, and `data`
        // lives on this frame, so the borrow stays valid across yields.
        walk_lines(ruby, input, &o)?;
    } else {
        // The block could mutate (or free the buffer of) an unfrozen
        // source mid-iteration; walk a private copy. Same policy as
        // NOSJ.lazy: pass a frozen string for zero-copy.
        let owned = input.to_vec();
        walk_lines(ruby, &owned, &o)?;
    }
    Ok(ruby.qnil().as_value())
}

/// `NOSJ.each_line_file(path, opts) { |value| }`: NDJSON over a
/// read-only memory map; the file never becomes a Ruby String.
pub fn each_line_file_native(
    ruby: &Ruby,
    _rb_self: Value,
    path: RString,
    opts: Value,
) -> Result<Value, Error> {
    let o = parse_native_opts(ruby, opts)?;
    let p = path.to_string()?;
    // An empty file is a valid, zero-event NDJSON stream, but mapping
    // a zero-length file fails with EINVAL on Linux; answer before the
    // mapper. Metadata failures (ENOENT, ...) fall through so the
    // mapper raises the same Errno an open would.
    if std::fs::metadata(&p).is_ok_and(|m| m.len() == 0) {
        return Ok(ruby.qnil().as_value());
    }
    with_mapped_file(ruby, &p, |map| {
        walk_lines(ruby, &map, &o)?;
        Ok(ruby.qnil().as_value())
    })
}

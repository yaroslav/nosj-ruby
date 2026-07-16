//! Partial parsing: `NOSJ.dig`, `NOSJ.at_pointer`, and their batch
//! forms. Pointers resolve against the raw document through the nosj
//! crate's cursor-mode skipper; only the matched subtree materializes,
//! through the same sink machinery as a full parse.

use magnus::{Error, RString, Ruby, Value};

use crate::parse::{err, materialize, parse_native_opts, utf8_input, ParseNativeOpts};
use crate::state::PULL_STATE;

/// Resolve one JSON Pointer against `data`, materializing the matched
/// subtree; `nil` when the pointer misses.
fn at_pointer_impl(
    ruby: &Ruby,
    data: RString,
    pointer: &str,
    o: &ParseNativeOpts,
) -> Result<Value, Error> {
    use magnus::value::ReprValue;
    let input = utf8_input(ruby, &data)?;
    // Resolve first (one PULL_STATE borrow), then materialize (a fresh
    // borrow); the resolved slice borrows `input`, not the buffers.
    let resolved = PULL_STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        // Safety: coderange verified above.
        unsafe { nosj::pointer_utf8_unchecked(input, pointer, &mut state.bufs) }
    });
    match resolved {
        Ok(None) => Ok(ruby.qnil().as_value()),
        Ok(Some(slice)) => materialize(ruby, slice.as_bytes(), o),
        Err(e) if matches!(e.kind, nosj::ErrorKind::InvalidPointer) => {
            Err(Error::new(ruby.exception_arg_error(), e.to_string()))
        }
        Err(e) => Err(err(ruby, e.to_string())),
    }
}

/// `NOSJ.at_pointer(source, "/a/b/0", opts)`: pointer lookup; the
/// matched subtree materializes under the same options as `parse`.
pub fn at_pointer_native(
    ruby: &Ruby,
    _rb_self: Value,
    data: RString,
    pointer: RString,
    opts: Value,
) -> Result<Value, Error> {
    let o = parse_native_opts(ruby, opts)?;
    let ptr = pointer.to_string()?;
    at_pointer_impl(ruby, data, &ptr, &o)
}

/// Convert one dig path (String/Symbol keys, Integer indices) into a
/// JSON Pointer, `~`/`/` escaped. `None` = a negative index: no pointer
/// equivalent, the path resolves to `nil` by definition.
fn path_to_pointer(ruby: &Ruby, path: magnus::RArray) -> Result<Option<String>, Error> {
    fn push_escaped_token(ptr: &mut String, key: &str) {
        ptr.push('/');
        for c in key.chars() {
            match c {
                '~' => ptr.push_str("~0"),
                '/' => ptr.push_str("~1"),
                c => ptr.push(c),
            }
        }
    }

    let mut ptr = String::new();
    for i in 0..path.len() {
        let item: Value = path.entry(i as isize)?;
        if let Some(int) = magnus::Integer::from_value(item) {
            let idx = int.to_i64()?;
            if idx < 0 {
                return Ok(None);
            }
            ptr.push('/');
            ptr.push_str(&idx.to_string());
        } else if let Some(s) = RString::from_value(item) {
            push_escaped_token(&mut ptr, &s.to_string()?);
        } else if let Some(sym) = magnus::Symbol::from_value(item) {
            push_escaped_token(&mut ptr, &sym.name()?);
        } else {
            return Err(Error::new(
                ruby.exception_arg_error(),
                "path elements must be Strings, Symbols, or Integers",
            ));
        }
    }
    Ok(Some(ptr))
}

/// `NOSJ.dig(source, *path)`: Hash#dig-shaped partial parsing.
/// Negative indices resolve to `nil` (JSON Pointer has no equivalent).
pub fn dig_native(
    ruby: &Ruby,
    _rb_self: Value,
    data: RString,
    path: magnus::RArray,
) -> Result<Value, Error> {
    use magnus::value::ReprValue;
    match path_to_pointer(ruby, path)? {
        Some(ptr) => at_pointer_impl(ruby, data, &ptr, &ParseNativeOpts::default()),
        None => Ok(ruby.qnil().as_value()),
    }
}

/// Resolve many pointers in ONE forward pass (`nosj::pointers`: the
/// walk descends only into subtrees some pointer still needs, so a batch
/// costs about its single deepest query), then materialize each hit.
/// `None` entries (negative dig indices) come back as `nil` without ever
/// reaching the resolver.
fn at_pointers_impl(
    ruby: &Ruby,
    data: RString,
    pointers: &[Option<String>],
    o: &ParseNativeOpts,
) -> Result<Value, Error> {
    use magnus::value::ReprValue;

    let input = utf8_input(ruby, &data)?;
    let live: Vec<&str> = pointers.iter().flatten().map(String::as_str).collect();

    // Resolve first (one PULL_STATE borrow); the resolved slices borrow
    // `input`, not the buffers, so materializing can re-borrow freely.
    let resolved = PULL_STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        // Safety: coderange verified by utf8_input.
        unsafe { nosj::pointers_utf8_unchecked(input, &live, &mut state.bufs) }
    });
    let mut hits = match resolved {
        Ok(hits) => hits.into_iter(),
        Err(e) if matches!(e.kind, nosj::ErrorKind::InvalidPointer) => {
            return Err(Error::new(ruby.exception_arg_error(), e.to_string()));
        }
        Err(e) => return Err(err(ruby, e.to_string())),
    };

    let out = ruby.ary_new_capa(pointers.len());
    for ptr in pointers {
        let hit = if ptr.is_some() {
            hits.next().flatten()
        } else {
            None
        };
        match hit {
            Some(slice) => out.push(materialize(ruby, slice.as_bytes(), o)?)?,
            None => out.push(ruby.qnil())?,
        }
    }
    Ok(out.as_value())
}

/// `NOSJ.at_pointers(source, pointers, opts)`: batch pointer lookup,
/// positionally aligned results.
pub fn at_pointers_native(
    ruby: &Ruby,
    _rb_self: Value,
    data: RString,
    pointers: magnus::RArray,
    opts: Value,
) -> Result<Value, Error> {
    let o = parse_native_opts(ruby, opts)?;
    let mut ptrs: Vec<Option<String>> = Vec::with_capacity(pointers.len());
    for i in 0..pointers.len() {
        let item: Value = pointers.entry(i as isize)?;
        let s = RString::from_value(item)
            .ok_or_else(|| Error::new(ruby.exception_arg_error(), "pointers must be Strings"))?;
        ptrs.push(Some(s.to_string()?));
    }
    at_pointers_impl(ruby, data, &ptrs, &o)
}

/// `NOSJ.dig_many(source, paths, opts)`: batch dig, one resolver
/// pass for all paths.
pub fn dig_many_native(
    ruby: &Ruby,
    _rb_self: Value,
    data: RString,
    paths: magnus::RArray,
    opts: Value,
) -> Result<Value, Error> {
    let o = parse_native_opts(ruby, opts)?;
    let mut ptrs: Vec<Option<String>> = Vec::with_capacity(paths.len());
    for i in 0..paths.len() {
        let item: Value = paths.entry(i as isize)?;
        let path = magnus::RArray::from_value(item)
            .ok_or_else(|| Error::new(ruby.exception_arg_error(), "each path must be an Array"))?;
        ptrs.push(path_to_pointer(ruby, path)?);
    }
    at_pointers_impl(ruby, data, &ptrs, &o)
}

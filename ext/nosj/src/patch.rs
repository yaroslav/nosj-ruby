//! Byte-splicing edits and RFC 6902 JSON Patch over raw documents.
//!
//! `NOSJ.splice` resolves every target pointer in ONE forward pass
//! (the batch resolver), then rebuilds the string copying the
//! untouched bytes around freshly generated values: no parse of the
//! rest, no re-generation, formatting outside the targets preserved
//! byte-for-byte. `NOSJ.patch` applies RFC 6902 operations
//! sequentially the same way; structural ops (add/remove) locate
//! entries by walking ONLY the parent container's span with the pull
//! Reader. Values are emitted through the shared gen machinery, so
//! bytes match `NOSJ.generate` exactly.

use magnus::value::ReprValue;
use magnus::{Error, ExceptionClass, RArray, RHash, RString, Ruby, Value};

use crate::errors::{nosj_exception, parser_error_at};
use crate::gen::{self, opts::GenConfig};
use crate::parse::{materialize_at, span_of, utf8_input, ParseNativeOpts};
use crate::state::PULL_STATE;

const WS: [u8; 4] = *b" \t\n\r";

fn patch_error(ruby: &Ruby, msg: String) -> Error {
    Error::new(nosj_exception(ruby, "PatchError"), msg)
}

fn arg_error(ruby: &Ruby, msg: String) -> Error {
    Error::new(ruby.exception_arg_error(), msg)
}

/// Decode generate options into a borrowed config: the shared default
/// for nil, else a config built into `slot`.
fn gen_config<'a>(
    ruby: &Ruby,
    opts: Value,
    slot: &'a mut Option<GenConfig>,
) -> Result<&'a GenConfig, Error> {
    if opts.is_nil() {
        Ok(&gen::opts::DEFAULT_CONFIG)
    } else {
        Ok(slot.insert(gen::opts::parse_gen_opts(ruby, opts)?.0))
    }
}

/// Resolve `pointer` over `doc`, returning the value's byte span.
/// Parse failures raise the rich ParserError (absolute positions);
/// pointer syntax errors raise ArgumentError, like `at_pointer`.
fn span_at(ruby: &Ruby, doc: &[u8], pointer: &str) -> Result<Option<(usize, usize)>, Error> {
    let resolved = PULL_STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        // SAFETY: every entry validated the document bytes as UTF-8.
        unsafe { nosj::pointer_utf8_unchecked(doc, pointer, &mut state.bufs) }
    });
    match resolved {
        Ok(None) => Ok(None),
        Ok(Some(slice)) => Ok(Some(span_of(doc, slice.as_bytes()))),
        Err(e) if matches!(e.kind, nosj::ErrorKind::InvalidPointer) => {
            Err(arg_error(ruby, e.to_string()))
        }
        Err(e) => Err(parser_error_at(ruby, doc, e.offset, e.to_string())),
    }
}

/// One direct child of a container: decoded key (objects only) and
/// the value's byte span, doc-absolute.
struct ChildSpan {
    key: Option<String>,
    start: usize,
    end: usize,
}

const KIND_OBJECT: u8 = b'{';

/// Walk a container span with the pull Reader, collecting every direct
/// child's span (and decoded key). Mirrors the lazy-document children
/// walk; errors report doc-absolute positions.
fn container_children(
    ruby: &Ruby,
    doc: &[u8],
    start: usize,
    end: usize,
) -> Result<(u8, Vec<ChildSpan>), Error> {
    let span = &doc[start..end];
    let kind = span.first().copied().unwrap_or(0);
    let walk: Result<Vec<ChildSpan>, nosj::ParseError> = PULL_STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        // SAFETY: doc validated UTF-8 by the entry; spans lie on token
        // edges.
        let mut r = unsafe { nosj::Reader::from_utf8_unchecked(span, &mut state.bufs) };
        r.next_node()?;
        let mut out = Vec::new();
        if kind == KIND_OBJECT {
            let mut key = r.object_first_key()?.map(String::from);
            while let Some(k) = key {
                let sub = r.skip_value()?;
                let (s, e) = span_of(span, sub.as_bytes());
                out.push(ChildSpan {
                    key: Some(k),
                    start: start + s,
                    end: start + e,
                });
                key = r.object_next_key()?.map(String::from);
            }
        } else {
            let mut has = r.array_first()?;
            while has {
                let sub = r.skip_value()?;
                let (s, e) = span_of(span, sub.as_bytes());
                out.push(ChildSpan {
                    key: None,
                    start: start + s,
                    end: start + e,
                });
                has = r.array_next()?;
            }
        }
        Ok(out)
    });
    let children = walk.map_err(|e| parser_error_at(ruby, doc, start + e.offset, e.to_string()))?;
    Ok((kind, children))
}

/// Split an RFC 6901 pointer into (parent, unescaped last token).
/// Returns None for the root pointer "".
fn split_pointer(pointer: &str) -> Option<(&str, String)> {
    let cut = pointer.rfind('/')?;
    let token = pointer[cut + 1..].replace("~1", "/").replace("~0", "~");
    Some((&pointer[..cut], token))
}

/// RFC 6902 array index token: digits only, no leading zeros.
fn parse_index(token: &str) -> Option<usize> {
    if token.is_empty() || (token.len() > 1 && token.starts_with('0')) {
        return None;
    }
    if !token.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    token.parse().ok()
}

fn skip_ws(doc: &[u8], mut pos: usize) -> usize {
    while pos < doc.len() && WS.contains(&doc[pos]) {
        pos += 1;
    }
    pos
}

/// Start of entry `i`'s bytes (the key quote for objects, the value
/// for arrays): first content after the opening bracket for entry 0,
/// after the separating comma otherwise. Pure byte scanning over a
/// region the Reader walk just validated.
fn entry_start(doc: &[u8], container_start: usize, children: &[ChildSpan], i: usize) -> usize {
    if i == 0 {
        skip_ws(doc, container_start + 1)
    } else {
        let after_prev = skip_ws(doc, children[i - 1].end);
        // after_prev sits on the ',' between entries.
        skip_ws(doc, after_prev + 1)
    }
}

/// What goes into the document at a splice point.
enum Insert {
    /// Generate this Ruby value through the gen machinery.
    Value(Value),
    /// Pre-rendered bytes: move/copy splice the source span verbatim,
    /// and entry insertions carry their key and comma.
    Owned(Vec<u8>),
    Nothing,
}

/// Rebuild `doc` with `[start, end)` replaced by `insert`.
fn apply_edit(
    ruby: &Ruby,
    doc: &[u8],
    start: usize,
    end: usize,
    insert: &Insert,
    cfg: &GenConfig,
) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(doc.len() + 64);
    out.extend_from_slice(&doc[..start]);
    match insert {
        Insert::Value(v) => gen::emit_into(ruby, *v, cfg, &mut out)?,
        Insert::Owned(bytes) => out.extend_from_slice(bytes),
        Insert::Nothing => {}
    }
    out.extend_from_slice(&doc[end..]);
    Ok(out)
}

/// Render `"key":` plus the value bytes for an object entry insertion.
fn render_entry(ruby: &Ruby, key: &str, value: &Insert, cfg: &GenConfig) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::with_capacity(key.len() + 16);
    let key_str: Value = ruby.str_new(key).as_value();
    gen::emit_into(ruby, key_str, cfg, &mut bytes)?;
    bytes.push(b':');
    match value {
        Insert::Value(v) => gen::emit_into(ruby, *v, cfg, &mut bytes)?,
        Insert::Owned(raw) => bytes.extend_from_slice(raw),
        Insert::Nothing => unreachable!("entry insertions always carry a value"),
    }
    Ok(bytes)
}

/// RFC 6902 `add`: object members insert-or-replace, array indices
/// insert before (with `-` appending), the root replaces the document.
fn op_add(
    ruby: &Ruby,
    doc: &[u8],
    path: &str,
    value: &Insert,
    cfg: &GenConfig,
) -> Result<Vec<u8>, Error> {
    let Some((parent, token)) = split_pointer(path) else {
        // Root: the value becomes the entire document.
        return apply_edit(ruby, doc, 0, doc.len(), value, cfg);
    };
    let Some((pstart, pend)) = span_at(ruby, doc, parent)? else {
        return Err(patch_error(
            ruby,
            format!("add target parent {parent:?} does not exist"),
        ));
    };
    let (kind, children) = container_children(ruby, doc, pstart, pend)?;
    if kind == KIND_OBJECT {
        if let Some(i) = children
            .iter()
            .position(|c| c.key.as_deref() == Some(&*token))
        {
            // Existing member: add replaces its value.
            return apply_edit(ruby, doc, children[i].start, children[i].end, value, cfg);
        }
        let mut entry = render_entry(ruby, &token, value, cfg)?;
        if let Some(last) = children.last() {
            let mut bytes = Vec::with_capacity(entry.len() + 1);
            bytes.push(b',');
            bytes.append(&mut entry);
            apply_edit(ruby, doc, last.end, last.end, &Insert::Owned(bytes), cfg)
        } else {
            // Empty object: insert just before the closing brace.
            apply_edit(ruby, doc, pend - 1, pend - 1, &Insert::Owned(entry), cfg)
        }
    } else {
        let index = if token == "-" {
            children.len()
        } else {
            parse_index(&token).ok_or_else(|| {
                patch_error(ruby, format!("invalid array index {token:?} in {path:?}"))
            })?
        };
        if index > children.len() {
            return Err(patch_error(
                ruby,
                format!("array index {index} out of range in {path:?}"),
            ));
        }
        if index == children.len() {
            // Append (also the empty-array case).
            if let Some(last) = children.last() {
                let mut bytes = vec![b','];
                extend_insert(ruby, &mut bytes, value, cfg)?;
                apply_edit(ruby, doc, last.end, last.end, &Insert::Owned(bytes), cfg)
            } else {
                let mut bytes = Vec::new();
                extend_insert(ruby, &mut bytes, value, cfg)?;
                apply_edit(ruby, doc, pend - 1, pend - 1, &Insert::Owned(bytes), cfg)
            }
        } else {
            // Insert before element `index`.
            let at = children[index].start;
            let mut bytes = Vec::new();
            extend_insert(ruby, &mut bytes, value, cfg)?;
            bytes.push(b',');
            apply_edit(ruby, doc, at, at, &Insert::Owned(bytes), cfg)
        }
    }
}

fn extend_insert(
    ruby: &Ruby,
    bytes: &mut Vec<u8>,
    value: &Insert,
    cfg: &GenConfig,
) -> Result<(), Error> {
    match value {
        Insert::Value(v) => gen::emit_into(ruby, *v, cfg, bytes),
        Insert::Owned(raw) => {
            bytes.extend_from_slice(raw);
            Ok(())
        }
        Insert::Nothing => unreachable!("insertions always carry a value"),
    }
}

/// RFC 6902 `remove`: the entry disappears along with its key and one
/// separating comma; surrounding formatting stays.
fn op_remove(ruby: &Ruby, doc: &[u8], path: &str) -> Result<Vec<u8>, Error> {
    let Some((parent, token)) = split_pointer(path) else {
        return Err(patch_error(ruby, "cannot remove the root".into()));
    };
    let Some((pstart, pend)) = span_at(ruby, doc, parent)? else {
        return Err(patch_error(
            ruby,
            format!("remove target parent {parent:?} does not exist"),
        ));
    };
    let (kind, children) = container_children(ruby, doc, pstart, pend)?;
    let i = if kind == KIND_OBJECT {
        children
            .iter()
            .position(|c| c.key.as_deref() == Some(&*token))
    } else {
        parse_index(&token).filter(|&i| i < children.len())
    };
    let Some(i) = i else {
        return Err(patch_error(
            ruby,
            format!("remove target {path:?} does not exist"),
        ));
    };
    let (rs, re) = if children.len() == 1 {
        (pstart + 1, pend - 1)
    } else if i + 1 < children.len() {
        (
            entry_start(doc, pstart, &children, i),
            entry_start(doc, pstart, &children, i + 1),
        )
    } else {
        (children[i - 1].end, children[i].end)
    };
    apply_edit(
        ruby,
        doc,
        rs,
        re,
        &Insert::Nothing,
        &gen::opts::DEFAULT_CONFIG,
    )
}

/// Deep JSON equality for `test`: the target span materializes and
/// compares with Ruby `==` (JSON types map onto Ruby ones 1:1).
fn op_test(ruby: &Ruby, doc: &[u8], path: &str, expected: Value) -> Result<(), Error> {
    let Some((start, end)) = span_at(ruby, doc, path)? else {
        return Err(patch_error(
            ruby,
            format!("test target {path:?} does not exist"),
        ));
    };
    let actual = materialize_at(ruby, doc, start, end, &ParseNativeOpts::default())?;
    let equal: bool = actual.funcall("==", (expected,))?;
    if !equal {
        return Err(patch_error(ruby, format!("test failed at {path:?}")));
    }
    Ok(())
}

/// `NOSJ.splice(json, edits, opts)`: batch pointer replacement. All
/// targets resolve in ONE forward pass; the output is built in one
/// sweep copying every byte outside the target spans untouched.
pub fn splice_native(
    ruby: &Ruby,
    _rb_self: Value,
    data: RString,
    edits: RHash,
    opts: Value,
) -> Result<RString, Error> {
    let mut slot = None;
    let cfg = gen_config(ruby, opts, &mut slot)?;
    let input = utf8_input(ruby, &data)?;

    let mut pointers: Vec<String> = Vec::with_capacity(edits.len());
    let mut values: Vec<Value> = Vec::with_capacity(edits.len());
    edits.foreach(|k: Value, v: Value| {
        let ptr = RString::from_value(k)
            .ok_or_else(|| arg_error(ruby, "splice pointers must be Strings".into()))?;
        pointers.push(ptr.to_string()?);
        values.push(v);
        Ok(magnus::r_hash::ForEach::Continue)
    })?;

    let refs: Vec<&str> = pointers.iter().map(String::as_str).collect();
    let resolved = PULL_STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        // SAFETY: coderange verified by utf8_input.
        unsafe { nosj::pointers_utf8_unchecked(input, &refs, &mut state.bufs) }
    });
    let hits = match resolved {
        Ok(hits) => hits,
        Err(e) if matches!(e.kind, nosj::ErrorKind::InvalidPointer) => {
            return Err(arg_error(ruby, e.to_string()));
        }
        Err(e) => return Err(parser_error_at(ruby, input, e.offset, e.to_string())),
    };

    let mut spans: Vec<(usize, usize, Value)> = Vec::with_capacity(pointers.len());
    for (i, hit) in hits.into_iter().enumerate() {
        let Some(slice) = hit else {
            let exc: ExceptionClass = ruby.exception_key_error();
            return Err(Error::new(
                exc,
                format!("pointer {:?} does not resolve", pointers[i]),
            ));
        };
        let (s, e) = span_of(input, slice.as_bytes());
        spans.push((s, e, values[i]));
    }
    spans.sort_unstable_by_key(|&(s, _, _)| s);
    for pair in spans.windows(2) {
        if pair[1].0 < pair[0].1 {
            return Err(arg_error(
                ruby,
                "splice targets overlap (one pointer addresses a value inside another)".into(),
            ));
        }
    }

    let mut out = Vec::with_capacity(input.len() + 64);
    let mut pos = 0;
    for &(s, e, v) in &spans {
        out.extend_from_slice(&input[pos..s]);
        gen::emit_into(ruby, v, cfg, &mut out)?;
        pos = e;
    }
    out.extend_from_slice(&input[pos..]);
    finish_string(&out)
}

fn finish_string(bytes: &[u8]) -> Result<RString, Error> {
    use magnus::rb_sys::FromRawValue;
    Ok(unsafe {
        RString::from_value(Value::from_raw(rb_sys::rb_utf8_str_new(
            bytes.as_ptr().cast(),
            bytes.len() as std::os::raw::c_long,
        )))
        .expect("rb_utf8_str_new returns a String")
    })
}

/// Fetch a patch-operation field by String or Symbol key. An absent
/// key is None; an explicit null stays Some (RFC 6902 allows
/// `"value": null`).
fn op_field(ruby: &Ruby, op: RHash, name: &str) -> Option<Value> {
    op.get(name).or_else(|| op.get(ruby.to_symbol(name)))
}

fn op_str_field(ruby: &Ruby, op: RHash, name: &str) -> Result<Option<String>, Error> {
    match op_field(ruby, op, name) {
        None => Ok(None),
        Some(v) => {
            let s = RString::from_value(v)
                .ok_or_else(|| arg_error(ruby, format!("patch op {name:?} must be a String")))?;
            Ok(Some(s.to_string()?))
        }
    }
}

/// `NOSJ.patch(json, ops, opts)`: RFC 6902, applied sequentially over
/// the evolving raw document.
pub fn patch_native(
    ruby: &Ruby,
    _rb_self: Value,
    data: RString,
    ops: RArray,
    opts: Value,
) -> Result<RString, Error> {
    let mut slot = None;
    let cfg = gen_config(ruby, opts, &mut slot)?;
    let input = utf8_input(ruby, &data)?;
    let mut doc: Vec<u8> = input.to_vec();

    for i in 0..ops.len() {
        let entry: Value = ops.entry(i as isize)?;
        let op = RHash::from_value(entry)
            .ok_or_else(|| arg_error(ruby, format!("patch op {i} is not a Hash")))?;
        let name = op_str_field(ruby, op, "op")?
            .ok_or_else(|| arg_error(ruby, format!("patch op {i} is missing \"op\"")))?;
        let path = op_str_field(ruby, op, "path")?
            .ok_or_else(|| arg_error(ruby, format!("patch op {i} is missing \"path\"")))?;
        let value = || {
            op_field(ruby, op, "value").ok_or_else(|| {
                arg_error(ruby, format!("patch op {i} ({name}) is missing \"value\""))
            })
        };
        let from = || {
            op_str_field(ruby, op, "from")?.ok_or_else(|| {
                arg_error(ruby, format!("patch op {i} ({name}) is missing \"from\""))
            })
        };

        doc = match name.as_str() {
            "add" => op_add(ruby, &doc, &path, &Insert::Value(value()?), cfg)?,
            "replace" => {
                let Some((s, e)) = span_at(ruby, &doc, &path)? else {
                    return Err(patch_error(
                        ruby,
                        format!("replace target {path:?} does not exist"),
                    ));
                };
                apply_edit(ruby, &doc, s, e, &Insert::Value(value()?), cfg)?
            }
            "remove" => op_remove(ruby, &doc, &path)?,
            "move" => {
                let from = from()?;
                if path == from {
                    continue;
                }
                if path.starts_with(&format!("{from}/")) {
                    return Err(patch_error(
                        ruby,
                        format!("cannot move {from:?} into its own child {path:?}"),
                    ));
                }
                let Some((s, e)) = span_at(ruby, &doc, &from)? else {
                    return Err(patch_error(
                        ruby,
                        format!("move source {from:?} does not exist"),
                    ));
                };
                let raw = doc[s..e].to_vec();
                let removed = op_remove(ruby, &doc, &from)?;
                op_add(ruby, &removed, &path, &Insert::Owned(raw), cfg)?
            }
            "copy" => {
                let from = from()?;
                let Some((s, e)) = span_at(ruby, &doc, &from)? else {
                    return Err(patch_error(
                        ruby,
                        format!("copy source {from:?} does not exist"),
                    ));
                };
                let raw = doc[s..e].to_vec();
                op_add(ruby, &doc, &path, &Insert::Owned(raw), cfg)?
            }
            "test" => {
                op_test(ruby, &doc, &path, value()?)?;
                doc
            }
            other => {
                return Err(arg_error(ruby, format!("unknown patch op {other:?}")));
            }
        };
    }
    finish_string(&doc)
}

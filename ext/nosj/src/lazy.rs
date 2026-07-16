//! Lazy documents: `NOSJ.lazy` wraps a JSON document and resolves
//! access on demand through the crate's pointer skipper. Every node is
//! a byte span into a shared, immutable copy of the document; container
//! children come back as further lazy nodes, scalars materialize
//! immediately through the same sink machinery as a full parse. Nothing
//! outside the touched path is ever parsed, so pulling a few fields out
//! of a large document costs microseconds, not a full parse.
//!
//! Validation is as-you-go (the crate's skipper checks bracket balance
//! over skipped content and fully validates resolved targets), so a
//! malformed region raises when an access first walks it, not at
//! `NOSJ.lazy` time.

use std::sync::Arc;

use magnus::typed_data::Obj;
use magnus::value::ReprValue;
use magnus::{DataTypeFunctions, Error, RArray, RString, Ruby, TypedData, Value};

use crate::parse::{err, materialize, parse_native_opts, utf8_input, ParseNativeOpts};
use crate::pointer::{path_to_pointer, push_escaped_token};
use crate::state::PULL_STATE;

/// The document bytes behind a node tree. A frozen Ruby source is
/// borrowed zero-copy: freezing rules out mutation, and every node
/// GC-marks the string with `rb_gc_mark` semantics, which both keeps it
/// alive and pins it against compaction, so the captured pointer stays
/// valid for as long as any node exists. Anything else is copied once.
enum DocBytes {
    Owned(Vec<u8>),
    Frozen {
        source: rb_sys::VALUE,
        ptr: *const u8,
        len: usize,
    },
}

/// The shared document: stable bytes plus the parse options every
/// materialization from this document uses.
struct DocInner {
    bytes: DocBytes,
    opts: ParseNativeOpts,
}

// SAFETY: the bytes are immutable for the document's whole life (an
// owned Vec, or a frozen Ruby string pinned and kept alive by every
// node's GC mark), so cross-thread reads are plain shared reads; the
// raw VALUE is only dereferenced by the GC mark, which runs at
// safepoints (the ShadowHandle contract in state.rs). There is no
// interior mutability anywhere in the type.
unsafe impl Send for DocInner {}
unsafe impl Sync for DocInner {}

impl DocInner {
    fn bytes(&self) -> &[u8] {
        match &self.bytes {
            DocBytes::Owned(v) => v,
            // SAFETY: the source string is frozen (no mutation, no
            // buffer reallocation) and pinned+kept alive by every
            // node's GC mark; see DocBytes.
            DocBytes::Frozen { ptr, len, .. } => unsafe { std::slice::from_raw_parts(*ptr, *len) },
        }
    }
}

const KIND_OBJECT: u8 = b'{';
const KIND_ARRAY: u8 = b'[';

/// One lazy container node: a byte span into its document. Spans always
/// come from the crate's resolver (token edges within the doc bytes),
/// and never cross the Ruby boundary, so they cannot be forged from
/// Ruby.
#[derive(TypedData)]
#[magnus(class = "NOSJ::Lazy", mark)]
pub struct LazyNode {
    doc: Arc<DocInner>,
    start: usize,
    end: usize,
    kind: u8,
}

impl DataTypeFunctions for LazyNode {
    fn mark(&self, marker: &magnus::gc::Marker) {
        if let DocBytes::Frozen { source, .. } = self.doc.bytes {
            use magnus::rb_sys::FromRawValue;
            // SAFETY: the VALUE was a live, frozen string at node
            // creation and this mark is what keeps it that way.
            marker.mark(unsafe { Value::from_raw(source) });
        }
    }
}

impl LazyNode {
    fn span(&self) -> &[u8] {
        &self.doc.bytes()[self.start..self.end]
    }
}

/// Wrap a resolved raw-value slice: containers become new lazy nodes,
/// scalars materialize now. `sub` must be a subslice of `doc.bytes`.
fn resolved_to_value(ruby: &Ruby, doc: &Arc<DocInner>, sub: &[u8]) -> Result<Value, Error> {
    match sub.first().copied() {
        Some(k) if k == KIND_OBJECT || k == KIND_ARRAY => {
            let base = doc.bytes().as_ptr() as usize;
            let start = sub.as_ptr() as usize - base;
            let node = LazyNode {
                doc: Arc::clone(doc),
                start,
                end: start + sub.len(),
                kind: k,
            };
            let obj: Obj<LazyNode> = ruby.obj_wrap(node);
            Ok(obj.as_value())
        }
        _ => materialize(ruby, sub, &doc.opts),
    }
}

/// Resolve `pointer` within `node`'s span. Shared by `__get` and
/// `__at_pointer`; both misses and negative-index paths return nil.
fn resolve_in_span(ruby: &Ruby, node: &LazyNode, pointer: &str) -> Result<Value, Error> {
    // Resolve first (one PULL_STATE borrow, slice borrows the doc, not
    // the buffers), then materialize (which re-borrows internally).
    let resolved = PULL_STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        // SAFETY: doc bytes were coderange-gated at NOSJ.lazy creation,
        // and spans lie on token edges, so the span is valid UTF-8.
        unsafe { nosj::pointer_utf8_unchecked(node.span(), pointer, &mut state.bufs) }
    });
    match resolved {
        Ok(None) => Ok(ruby.qnil().as_value()),
        Ok(Some(sub)) => resolved_to_value(ruby, &node.doc, sub.as_bytes()),
        Err(e) if matches!(e.kind, nosj::ErrorKind::InvalidPointer) => {
            Err(Error::new(ruby.exception_arg_error(), e.to_string()))
        }
        Err(e) => Err(err(ruby, e.to_string())),
    }
}

/// `NOSJ.lazy(source, opts)`: gate the encoding, copy the bytes, locate
/// the root value. Container roots wrap as lazy nodes; a scalar root has
/// nothing to defer and materializes immediately.
///
/// The root span is the input trimmed of surrounding whitespace; no
/// byte of content is walked here (resolving via the "" pointer would
/// bracket-skip the whole document just to find the same end), so
/// malformation anywhere, including root-level trailing garbage,
/// surfaces on first access, per the module's lazy-validation contract.
pub fn lazy_native(
    ruby: &Ruby,
    _rb_self: Value,
    data: RString,
    opts: Value,
) -> Result<Value, Error> {
    const WS: [u8; 4] = *b" \t\n\r";
    let o = parse_native_opts(ruby, opts)?;
    let input = utf8_input(ruby, &data)?;

    let Some(start) = input.iter().position(|b| !WS.contains(b)) else {
        return Err(err(ruby, "unexpected end of input".into()));
    };
    let end = input.iter().rposition(|b| !WS.contains(b)).unwrap() + 1;

    // Frozen sources are borrowed zero-copy (see DocBytes); `data` is
    // on the caller's machine stack, so it stays pinned through this
    // call, and the node's mark takes over from the first GC on.
    let bytes = if data.as_value().is_frozen() {
        use magnus::rb_sys::AsRawValue;
        DocBytes::Frozen {
            source: data.as_raw(),
            ptr: input.as_ptr(),
            len: input.len(),
        }
    } else {
        DocBytes::Owned(input.to_vec())
    };
    let doc = Arc::new(DocInner { bytes, opts: o });
    let sub = &doc.bytes()[start..end];
    resolved_to_value(ruby, &doc, sub)
}

/// `__get(token)`: one path step. Integer tokens are JSON Pointer
/// indices (negative ones resolve to nil, as in NOSJ.dig); String and
/// Symbol tokens are keys, `~`/`/`-escaped.
pub fn lazy_get(ruby: &Ruby, rb_self: Obj<LazyNode>, token: Value) -> Result<Value, Error> {
    let mut ptr = String::new();
    if let Some(int) = magnus::Integer::from_value(token) {
        let idx = int.to_i64()?;
        if idx < 0 {
            return Ok(ruby.qnil().as_value());
        }
        ptr.push('/');
        ptr.push_str(&idx.to_string());
    } else if let Some(s) = RString::from_value(token) {
        push_escaped_token(&mut ptr, &s.to_string()?);
    } else if let Some(sym) = magnus::Symbol::from_value(token) {
        push_escaped_token(&mut ptr, &sym.name()?);
    } else {
        return Err(Error::new(
            ruby.exception_arg_error(),
            "keys must be Strings, Symbols, or Integers",
        ));
    }
    resolve_in_span(ruby, &rb_self, &ptr)
}

/// `__dig(path)`: the whole dig path fused into ONE pointer resolution
/// within this node's span, instead of one resolve (and one cached
/// node) per step. Semantics match `NOSJ.dig`: negative indices and
/// steps into scalars resolve to nil.
pub fn lazy_dig(ruby: &Ruby, rb_self: Obj<LazyNode>, path: RArray) -> Result<Value, Error> {
    match path_to_pointer(ruby, path)? {
        Some(ptr) => resolve_in_span(ruby, &rb_self, &ptr),
        None => Ok(ruby.qnil().as_value()),
    }
}

/// `__at_pointer(pointer)`: a full RFC 6901 pointer, resolved within
/// this node's subtree.
pub fn lazy_at_pointer(
    ruby: &Ruby,
    rb_self: Obj<LazyNode>,
    pointer: RString,
) -> Result<Value, Error> {
    let ptr = pointer.to_string()?;
    resolve_in_span(ruby, &rb_self, &ptr)
}

/// `__materialize`: the whole span as plain Ruby values, under the
/// document's parse options.
pub fn lazy_materialize(ruby: &Ruby, rb_self: Obj<LazyNode>) -> Result<Value, Error> {
    materialize(ruby, rb_self.span(), &rb_self.doc.opts)
}

/// `__kind`: `:object` or `:array`.
pub fn lazy_kind(ruby: &Ruby, rb_self: Obj<LazyNode>) -> magnus::Symbol {
    match rb_self.kind {
        KIND_OBJECT => ruby.to_symbol("object"),
        _ => ruby.to_symbol("array"),
    }
}

/// `__byte_size`: span length in bytes (cheap; used by #inspect).
pub fn lazy_byte_size(_ruby: &Ruby, rb_self: Obj<LazyNode>) -> usize {
    rb_self.end - rb_self.start
}

fn reader_err(ruby: &Ruby, e: nosj::ParseError) -> Error {
    err(ruby, e.to_string())
}

/// `__keys`: the object's decoded keys, one Reader walk, values skipped.
pub fn lazy_keys(ruby: &Ruby, rb_self: Obj<LazyNode>) -> Result<RArray, Error> {
    if rb_self.kind != KIND_OBJECT {
        return Err(Error::new(
            ruby.exception_type_error(),
            "keys on a JSON array",
        ));
    }
    let out = ruby.ary_new();
    PULL_STATE.with(|cell| -> Result<(), Error> {
        let mut state = cell.borrow_mut();
        // SAFETY: spans are valid UTF-8 (see resolve_in_span).
        let mut r = unsafe { nosj::Reader::from_utf8_unchecked(rb_self.span(), &mut state.bufs) };
        r.next_node().map_err(|e| reader_err(ruby, e))?;
        let mut has = match r.object_first_key().map_err(|e| reader_err(ruby, e))? {
            Some(k) => {
                out.push(ruby.str_new(k))?;
                true
            }
            None => false,
        };
        while has {
            r.skip_value().map_err(|e| reader_err(ruby, e))?;
            has = match r.object_next_key().map_err(|e| reader_err(ruby, e))? {
                Some(k) => {
                    out.push(ruby.str_new(k))?;
                    true
                }
                None => false,
            };
        }
        Ok(())
    })?;
    Ok(out)
}

/// `__size`: entry count (object pairs or array elements), one walk,
/// nothing materialized.
pub fn lazy_size(ruby: &Ruby, rb_self: Obj<LazyNode>) -> Result<usize, Error> {
    PULL_STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        // SAFETY: spans are valid UTF-8 (see resolve_in_span).
        let mut r = unsafe { nosj::Reader::from_utf8_unchecked(rb_self.span(), &mut state.bufs) };
        r.next_node().map_err(|e| reader_err(ruby, e))?;
        let mut n = 0usize;
        if rb_self.kind == KIND_OBJECT {
            let mut has = r
                .object_first_key()
                .map_err(|e| reader_err(ruby, e))?
                .is_some();
            while has {
                n += 1;
                r.skip_value().map_err(|e| reader_err(ruby, e))?;
                has = r
                    .object_next_key()
                    .map_err(|e| reader_err(ruby, e))?
                    .is_some();
            }
        } else {
            let mut has = r.array_first().map_err(|e| reader_err(ruby, e))?;
            while has {
                n += 1;
                r.skip_value().map_err(|e| reader_err(ruby, e))?;
                has = r.array_next().map_err(|e| reader_err(ruby, e))?;
            }
        }
        Ok(n)
    })
}

/// A child discovered during a container walk: a doc-relative span,
/// plus the owned key for object entries (decoded keys borrow the
/// reader's scratch, so they are copied out before phase two).
struct ChildDesc {
    key: Option<String>,
    start: usize,
    end: usize,
}

/// `__children`: every direct child in ONE walk. Objects yield
/// `[key, child]` pairs, arrays yield children; containers wrap lazily,
/// scalars materialize. Two phases so the Reader's buffer borrow ends
/// before materialization re-borrows the thread state.
pub fn lazy_children(ruby: &Ruby, rb_self: Obj<LazyNode>) -> Result<RArray, Error> {
    let base = rb_self.doc.bytes().as_ptr() as usize;
    let descs: Result<Vec<ChildDesc>, nosj::ParseError> = PULL_STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        // SAFETY: spans are valid UTF-8 (see resolve_in_span).
        let mut r = unsafe { nosj::Reader::from_utf8_unchecked(rb_self.span(), &mut state.bufs) };
        r.next_node()?;
        let mut out = Vec::new();
        if rb_self.kind == KIND_OBJECT {
            let mut key = r.object_first_key()?.map(String::from);
            while let Some(k) = key {
                let sub = r.skip_value()?;
                let start = sub.as_ptr() as usize - base;
                out.push(ChildDesc {
                    key: Some(k),
                    start,
                    end: start + sub.len(),
                });
                key = r.object_next_key()?.map(String::from);
            }
        } else {
            let mut has = r.array_first()?;
            while has {
                let sub = r.skip_value()?;
                let start = sub.as_ptr() as usize - base;
                out.push(ChildDesc {
                    key: None,
                    start,
                    end: start + sub.len(),
                });
                has = r.array_next()?;
            }
        }
        Ok(out)
    });
    let descs = descs.map_err(|e| reader_err(ruby, e))?;

    let out = ruby.ary_new_capa(descs.len());
    for d in descs {
        let child = resolved_to_value(ruby, &rb_self.doc, &rb_self.doc.bytes()[d.start..d.end])?;
        match d.key {
            Some(k) => {
                let pair = ruby.ary_new_capa(2);
                pair.push(ruby.str_new(&k))?;
                pair.push(child)?;
                out.push(pair)?;
            }
            None => out.push(child)?,
        }
    }
    Ok(out)
}

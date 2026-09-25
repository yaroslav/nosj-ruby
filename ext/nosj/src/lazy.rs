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

use crate::errors::parser_error_at;
use crate::parse::{materialize_at, parse_native_opts, span_of, utf8_input, ParseNativeOpts};
use crate::pointer::{path_to_pointer, push_escaped_token};
use crate::state::with_pull_state;

/// The document bytes behind a node tree. A frozen Ruby source is
/// borrowed zero-copy: freezing rules out any change to its CONTENT,
/// and every node GC-marks the string with `rb_gc_mark` semantics
/// (alive, and pinned against compaction). Freezing does NOT pin the
/// buffer, though: deduplicating a frozen String subclass or a string
/// carrying ivars (`-str`) swaps in an identical shared buffer and
/// frees the old one. So the bytes are re-read from the string on every
/// access; same content means every span stays valid. Anything else is
/// copied once.
pub(crate) enum DocBytes {
    Owned(Vec<u8>),
    Frozen(rb_sys::VALUE),
    /// A read-only file mapping (`NOSJ.load_lazy_file`): pages never
    /// touched are never read off disk. Concurrent modification of the
    /// mapped file by another process is documented as unsupported
    /// (the standard mmap caveat).
    Mmap(memmap2::Mmap),
}

/// The shared document: stable bytes plus the parse options every
/// materialization from this document uses.
struct DocInner {
    bytes: DocBytes,
    opts: ParseNativeOpts,
}

// SAFETY: the content is immutable for the document's whole life (an
// owned Vec, or a frozen Ruby string pinned and kept alive by every
// node's GC mark), so cross-thread reads are plain shared reads of it.
// There is no interior mutability anywhere in the type.
unsafe impl Send for DocInner {}
unsafe impl Sync for DocInner {}

impl DocInner {
    /// The document bytes. For a frozen source the slice is valid only
    /// until the next Ruby call (which could swap the string's buffer;
    /// see DocBytes): callers finish with it, or call this again,
    /// before running Ruby code.
    fn bytes(&self) -> &[u8] {
        match &self.bytes {
            DocBytes::Owned(v) => v,
            // SAFETY: a live T_STRING (kept alive and pinned by every
            // node's GC mark), read fresh on each call; see DocBytes.
            DocBytes::Frozen(source) => unsafe {
                std::slice::from_raw_parts(
                    rb_sys::macros::RSTRING_PTR(*source).cast::<u8>(),
                    rb_sys::macros::RSTRING_LEN(*source) as usize,
                )
            },
            DocBytes::Mmap(m) => m,
        }
    }
}

const KIND_OBJECT: u8 = b'{';
const KIND_ARRAY: u8 = b'[';

/// One lazy container node: a byte span into its document. Spans always
/// come from the crate's resolver (token edges within the doc bytes),
/// and never cross the Ruby boundary, so they cannot be forged from
/// Ruby.
///
/// `wb_protected`: a node's only Ruby reference (a frozen source, in
/// the shared `DocInner`) is set before the root node is wrapped and
/// never written again, so there is no write to put a barrier on, and
/// nodes promote to the old generation instead of being rescanned by
/// every minor GC (children cache Ruby-side, in barrier-protected
/// ivars). `free_immediately`: dropping a node calls no Ruby API (it
/// releases a Vec, an mmap, or nothing).
#[derive(TypedData)]
#[magnus(class = "NOSJ::Lazy", free_immediately, mark, wb_protected)]
pub struct LazyNode {
    doc: Arc<DocInner>,
    start: usize,
    end: usize,
    kind: u8,
}

impl DataTypeFunctions for LazyNode {
    fn mark(&self, marker: &magnus::gc::Marker) {
        if let DocBytes::Frozen(source) = self.doc.bytes {
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

    /// A Reader over this node's span, walking the grammar the document
    /// was opened with (trailing commas, NaN keywords).
    fn reader<'a, 'b>(&'a self, bufs: &'b mut nosj::Buffers) -> nosj::Reader<'a, 'b> {
        // SAFETY: spans are valid UTF-8 (see resolve_in_span).
        unsafe { nosj::Reader::from_utf8_unchecked_with(self.span(), bufs, self.doc.opts.popts) }
    }
}

/// Wrap a resolved raw-value slice: containers become new lazy nodes,
/// scalars materialize now. `sub` must be a subslice of `doc.bytes`.
fn resolved_to_value(ruby: &Ruby, doc: &Arc<DocInner>, sub: &[u8]) -> Result<Value, Error> {
    let (start, end) = span_of(doc.bytes(), sub);
    match sub.first().copied() {
        Some(k) if k == KIND_OBJECT || k == KIND_ARRAY => {
            let node = LazyNode {
                doc: Arc::clone(doc),
                start,
                end,
                kind: k,
            };
            let obj: Obj<LazyNode> = ruby.obj_wrap(node);
            Ok(obj.as_value())
        }
        _ => materialize_at(ruby, doc.bytes(), start, end, &doc.opts),
    }
}

/// Resolve `pointer` within `node`'s span. Shared by `__get` and
/// `__at_pointer`; both misses and negative-index paths return nil.
fn resolve_in_span(ruby: &Ruby, node: &LazyNode, pointer: &str) -> Result<Value, Error> {
    // Resolve, then materialize, as two separate uses of the parse
    // state; the resolved slice borrows the doc, not the state.
    let resolved = with_pull_state(|state| {
        // SAFETY: doc bytes were coderange-gated at NOSJ.lazy creation,
        // and spans lie on token edges, so the span is valid UTF-8.
        unsafe {
            nosj::pointer_utf8_unchecked_with(
                node.span(),
                pointer,
                &mut state.bufs,
                node.doc.opts.popts,
            )
        }
    });
    match resolved {
        Ok(None) => Ok(ruby.qnil().as_value()),
        Ok(Some(sub)) => resolved_to_value(ruby, &node.doc, sub.as_bytes()),
        Err(e) if matches!(e.kind, nosj::ErrorKind::InvalidPointer) => {
            Err(Error::new(ruby.exception_arg_error(), e.to_string()))
        }
        Err(e) => Err(reader_err(ruby, node, e)),
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
    let o = parse_native_opts(ruby, opts)?;
    let input = utf8_input(ruby, &data)?;

    // Frozen sources are borrowed zero-copy (see DocBytes); `data` is
    // on the caller's machine stack, so it stays pinned through this
    // call, and the node's mark takes over from the first GC on.
    let bytes = if data.as_value().is_frozen() {
        use magnus::rb_sys::AsRawValue;
        DocBytes::Frozen(data.as_raw())
    } else {
        DocBytes::Owned(input.to_vec())
    };
    wrap_root(ruby, bytes, o)
}

/// Wrap document bytes as the root lazy value. Shared by `NOSJ.lazy`
/// and `NOSJ.load_lazy_file`; the bytes must already be known UTF-8.
pub(crate) fn wrap_root(
    ruby: &Ruby,
    bytes: DocBytes,
    opts: ParseNativeOpts,
) -> Result<Value, Error> {
    const WS: [u8; 4] = *b" \t\n\r";
    let doc = Arc::new(DocInner { bytes, opts });
    let input = doc.bytes();
    let Some(start) = input.iter().position(|b| !WS.contains(b)) else {
        return Err(parser_error_at(
            ruby,
            input,
            input.len(),
            "unexpected end of input".into(),
        ));
    };
    let end = input.iter().rposition(|b| !WS.contains(b)).unwrap() + 1;
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
    materialize_at(
        ruby,
        rb_self.doc.bytes(),
        rb_self.start,
        rb_self.end,
        &rb_self.doc.opts,
    )
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

/// A walk failure inside `node`'s span: the reader's offset is
/// span-relative, so shift by the node's start to report an absolute
/// document position.
fn reader_err(ruby: &Ruby, node: &LazyNode, e: nosj::ParseError) -> Error {
    parser_error_at(ruby, node.doc.bytes(), node.start + e.offset, e.to_string())
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
    with_pull_state(|state| -> Result<(), Error> {
        let mut r = rb_self.reader(&mut state.bufs);
        r.next_node().map_err(|e| reader_err(ruby, &rb_self, e))?;
        let mut has = match r
            .object_first_key()
            .map_err(|e| reader_err(ruby, &rb_self, e))?
        {
            Some(k) => {
                out.push(ruby.str_new(k))?;
                true
            }
            None => false,
        };
        while has {
            r.skip_value().map_err(|e| reader_err(ruby, &rb_self, e))?;
            has = match r
                .object_next_key()
                .map_err(|e| reader_err(ruby, &rb_self, e))?
            {
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
    with_pull_state(|state| {
        let mut r = rb_self.reader(&mut state.bufs);
        r.next_node().map_err(|e| reader_err(ruby, &rb_self, e))?;
        let mut n = 0usize;
        if rb_self.kind == KIND_OBJECT {
            let mut has = r
                .object_first_key()
                .map_err(|e| reader_err(ruby, &rb_self, e))?
                .is_some();
            while has {
                n += 1;
                r.skip_value().map_err(|e| reader_err(ruby, &rb_self, e))?;
                has = r
                    .object_next_key()
                    .map_err(|e| reader_err(ruby, &rb_self, e))?
                    .is_some();
            }
        } else {
            let mut has = r.array_first().map_err(|e| reader_err(ruby, &rb_self, e))?;
            while has {
                n += 1;
                r.skip_value().map_err(|e| reader_err(ruby, &rb_self, e))?;
                has = r.array_next().map_err(|e| reader_err(ruby, &rb_self, e))?;
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
/// scalars materialize. Two phases so the walk's use of the parse state
/// ends before materialization needs it (nested, it would start fresh).
pub fn lazy_children(ruby: &Ruby, rb_self: Obj<LazyNode>) -> Result<RArray, Error> {
    let base = rb_self.doc.bytes().as_ptr() as usize;
    let descs: Result<Vec<ChildDesc>, nosj::ParseError> = with_pull_state(|state| {
        let mut r = rb_self.reader(&mut state.bufs);
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
    let descs = descs.map_err(|e| reader_err(ruby, &rb_self, e))?;

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

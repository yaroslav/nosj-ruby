//! Low-level Ruby helpers for generation: protected wrappers over the
//! user-reachable raising calls (a Ruby raise longjmping through Rust
//! frames is UB, so each goes through magnus's closure-based
//! `rb_sys::protect`; magnus owns the FFI trampoline) and inline
//! RBasic flag readers.

use magnus::Error;
use rb_sys::{ruby_special_consts, VALUE};
use std::os::raw::c_int;
use std::sync::OnceLock;

pub(super) const QNIL: VALUE = ruby_special_consts::RUBY_Qnil as VALUE;
pub(super) const QTRUE: VALUE = ruby_special_consts::RUBY_Qtrue as VALUE;
pub(super) const QFALSE: VALUE = ruby_special_consts::RUBY_Qfalse as VALUE;

/// `v.to_s`, protected.
pub(super) fn protected_to_s(v: VALUE) -> Result<VALUE, Error> {
    magnus::rb_sys::protect(|| unsafe { rb_sys::rb_obj_as_string(v) })
}

/// `v.inspect`, protected (user code for any element's `inspect`).
pub(super) fn protected_inspect(v: VALUE) -> Result<VALUE, Error> {
    magnus::rb_sys::protect(|| unsafe { rb_sys::rb_inspect(v) })
}

/// `v.to_json`, protected.
pub(super) fn protected_to_json(v: VALUE) -> Result<VALUE, Error> {
    magnus::rb_sys::protect(|| unsafe { rb_sys::rb_funcall(v, to_json_id(), 0) })
}

/// `v.to_json` if `v.respond_to?(:to_json)`, else `None`, under ONE
/// protect: `rb_respond_to` dispatches to a user-defined `respond_to?` /
/// `respond_to_missing?`, which may raise just like `to_json` itself.
pub(super) fn protected_to_json_if_responds(v: VALUE) -> Result<Option<VALUE>, Error> {
    const QUNDEF: VALUE = ruby_special_consts::RUBY_Qundef as VALUE;
    let json = magnus::rb_sys::protect(|| unsafe {
        if rb_sys::rb_respond_to(v, to_json_id()) != 0 {
            rb_sys::rb_funcall(v, to_json_id(), 0)
        } else {
            QUNDEF
        }
    })?;
    Ok((json != QUNDEF).then_some(json))
}

/// `v.as_json`, protected. Argument-less on purpose: ActiveSupport's
/// JSONGemEncoder#jsonify recursion also calls as_json without
/// options (only the top-level value receives them).
pub(super) fn protected_as_json(v: VALUE) -> Result<VALUE, Error> {
    magnus::rb_sys::protect(|| unsafe { rb_sys::rb_funcall(v, as_json_id(), 0) })
}

/// Interned `as_json` method ID, resolved once per process.
fn as_json_id() -> rb_sys::ID {
    static AS_JSON: OnceLock<usize> = OnceLock::new();
    *AS_JSON.get_or_init(|| unsafe { rb_sys::rb_intern(c"as_json".as_ptr()) } as usize)
        as rb_sys::ID
}

/// Resolve the `OnceLock`s here now (init, main Ractor): an initializer
/// that enters the VM must never race between Ractors (see lib.rs), and
/// an initialized lock never blocks again.
pub(crate) fn warm_up() {
    as_json_id();
    to_json_id();
    utf8_encindexes();
}

/// Whether `v` is a `JSON::Fragment` (pre-rendered JSON to splice
/// verbatim: the gem accepts fragments even under `strict`, and
/// ActiveSupport's encoder passes them through). The class is resolved
/// lazily and cached only on success, so a json gem loaded after the
/// first generate is still found; a fragment instance existing implies
/// its class does. The cached VALUE is a constant of the JSON module,
/// so it can never be collected.
pub(super) fn is_json_fragment(v: VALUE) -> Result<bool, Error> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static FRAGMENT: AtomicUsize = AtomicUsize::new(0);
    let mut cls = FRAGMENT.load(Ordering::Relaxed) as VALUE;
    if cls == 0 {
        cls = resolve_json_fragment()?;
        if cls == 0 {
            return Ok(false);
        }
        FRAGMENT.store(cls as usize, Ordering::Relaxed);
    }
    Ok(unsafe { rb_sys::rb_obj_is_kind_of(v, cls) != QFALSE })
}

/// `JSON::Fragment`, or 0 while undefined. Only the `rb_const_get`s are
/// protected: fetching a constant can run its pending autoload (user
/// code, whose raise propagates as any constant reference's would),
/// while `rb_const_defined` never loads anything.
fn resolve_json_fragment() -> Result<VALUE, Error> {
    unsafe {
        let object = rb_sys::rb_cObject;
        let json_id = rb_sys::rb_intern(c"JSON".as_ptr());
        if rb_sys::rb_const_defined(object, json_id) == 0 {
            return Ok(0);
        }
        let json = magnus::rb_sys::protect(|| rb_sys::rb_const_get(object, json_id))?;
        let fragment_id = rb_sys::rb_intern(c"Fragment".as_ptr());
        if rb_sys::rb_const_defined(json, fragment_id) == 0 {
            return Ok(0);
        }
        magnus::rb_sys::protect(|| rb_sys::rb_const_get(json, fragment_id))
    }
}

/// Encode `v` to UTF-8, protected. `rb_str_encode` raises on
/// undefined/invalid conversions, matching the gem, which wraps that
/// exception as GeneratorError (`rb_str_export_to_enc` is lenient and
/// silently passes bad bytes through).
pub(super) fn protected_encode_utf8(v: VALUE) -> Result<VALUE, Error> {
    magnus::rb_sys::protect(|| unsafe {
        let utf8 = rb_sys::rb_enc_from_encoding(rb_sys::rb_utf8_encoding());
        rb_sys::rb_str_encode(v, utf8, 0, QNIL)
    })
}

/// Interned `to_json` method ID, resolved once per process.
fn to_json_id() -> rb_sys::ID {
    static TO_JSON: OnceLock<usize> = OnceLock::new();
    *TO_JSON.get_or_init(|| unsafe { rb_sys::rb_intern(c"to_json".as_ptr()) } as usize)
        as rb_sys::ID
}

pub(super) fn utf8_encindexes() -> (c_int, c_int) {
    static IDX: OnceLock<(c_int, c_int)> = OnceLock::new();
    *IDX.get_or_init(|| unsafe { (rb_sys::rb_utf8_encindex(), rb_sys::rb_usascii_encindex()) })
}

// Coderange and encoding index live in RBasic flags (public ABI); reading
// them inline instead of calling rb_enc_str_coderange / rb_enc_get_index is
// how the gem avoids two C calls per string (RB_ENC_CODERANGE,
// RB_ENCODING_GET_INLINED). The bit layout comes from rb-sys's bindings,
// generated from the headers of the Ruby being built against, so it
// follows any layout change instead of silently misreading flags.
const CR_MASK: u64 = rb_sys::ruby_coderange_type::RUBY_ENC_CODERANGE_MASK as u64;
pub(super) const CR_7BIT: u64 = rb_sys::ruby_coderange_type::RUBY_ENC_CODERANGE_7BIT as u64;
pub(super) const CR_VALID: u64 = rb_sys::ruby_coderange_type::RUBY_ENC_CODERANGE_VALID as u64;
const ENC_SHIFT: u64 = rb_sys::ruby_encoding_consts::RUBY_ENCODING_SHIFT as u64;
const ENC_MASK: u64 = rb_sys::ruby_encoding_consts::RUBY_ENCODING_MASK as u64;
/// Inline encoding-index sentinel: the real index is stored out of line.
const ENC_INLINE_MAX: c_int = rb_sys::ruby_encoding_consts::RUBY_ENCODING_INLINE_MAX as c_int;

#[inline(always)]
pub(super) fn str_coderange(s: VALUE) -> u64 {
    let flags = unsafe { (*(s as *const rb_sys::RBasic)).flags };
    let cr = flags & CR_MASK;
    if cr != 0 {
        cr
    } else {
        (unsafe { rb_sys::rb_enc_str_coderange(s) } as u64) & CR_MASK
    }
}

#[inline(always)]
pub(super) fn str_enc_index(s: VALUE) -> c_int {
    let flags = unsafe { (*(s as *const rb_sys::RBasic)).flags };
    let idx = ((flags & ENC_MASK) >> ENC_SHIFT) as c_int;
    if idx == ENC_INLINE_MAX {
        unsafe { rb_sys::rb_enc_get_index(s) }
    } else {
        idx
    }
}

/// `RB_SPECIAL_CONST_P` (immediates plus Qnil/Qfalse), through rb-sys's
/// inline versioned stable API rather than a hand-copied bit test.
#[inline(always)]
pub(super) fn is_special_const(v: VALUE) -> bool {
    rb_sys::macros::SPECIAL_CONST_P(v)
}

/// Borrow a Ruby String's bytes.
///
/// # Safety
/// `s` must be a `T_STRING` VALUE. The slice borrows the Ruby heap: it is
/// valid only until the next call that could mutate, reallocate, or free
/// the string, so callers must copy the bytes out before any Ruby call.
#[inline(always)]
pub(super) unsafe fn rstring_bytes<'a>(s: VALUE) -> &'a [u8] {
    std::slice::from_raw_parts(
        rb_sys::macros::RSTRING_PTR(s).cast::<u8>(),
        rb_sys::macros::RSTRING_LEN(s) as usize,
    )
}

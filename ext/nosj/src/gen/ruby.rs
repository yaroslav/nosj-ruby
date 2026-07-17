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

/// `v.to_json`, protected.
pub(super) fn protected_to_json(v: VALUE) -> Result<VALUE, Error> {
    magnus::rb_sys::protect(|| unsafe { rb_sys::rb_funcall(v, to_json_id(), 0) })
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

/// Whether `v` is a `JSON::Fragment` (pre-rendered JSON to splice
/// verbatim: the gem accepts fragments even under `strict`, and
/// ActiveSupport's encoder passes them through). The class is resolved
/// lazily and cached only on success, so a json gem loaded after the
/// first generate is still found; a fragment instance existing implies
/// its class does. The cached VALUE is a constant of the JSON module,
/// so it can never be collected.
pub(super) fn is_json_fragment(v: VALUE) -> bool {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static FRAGMENT: AtomicUsize = AtomicUsize::new(0);
    let mut cls = FRAGMENT.load(Ordering::Relaxed);
    if cls == 0 {
        cls = resolve_json_fragment();
        if cls == 0 {
            return false;
        }
        FRAGMENT.store(cls, Ordering::Relaxed);
    }
    unsafe { rb_sys::rb_obj_is_kind_of(v, cls as VALUE) != QFALSE }
}

fn resolve_json_fragment() -> usize {
    unsafe {
        let object = rb_sys::rb_cObject;
        let json_id = rb_sys::rb_intern(c"JSON".as_ptr());
        if rb_sys::rb_const_defined(object, json_id) == 0 {
            return 0;
        }
        let json = rb_sys::rb_const_get(object, json_id);
        let fragment_id = rb_sys::rb_intern(c"Fragment".as_ptr());
        if rb_sys::rb_const_defined(json, fragment_id) == 0 {
            return 0;
        }
        rb_sys::rb_const_get(json, fragment_id) as usize
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
pub(super) fn to_json_id() -> rb_sys::ID {
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
// RB_ENCODING_GET_INLINED).
const CR_MASK: u64 = 3 << 20;
pub(super) const CR_7BIT: u64 = 1 << 20;
pub(super) const CR_VALID: u64 = 2 << 20;
const ENC_SHIFT: u64 = 22;
const ENC_MASK: u64 = 127 << 22;

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
    if idx == 127 {
        // RUBY_ENCODING_INLINE_MAX sentinel: index stored out of line.
        unsafe { rb_sys::rb_enc_get_index(s) }
    } else {
        idx
    }
}

#[inline(always)]
pub(super) fn is_special_const(v: VALUE) -> bool {
    // RB_SPECIAL_CONST_P: immediates plus Qnil/Qfalse.
    (v & (ruby_special_consts::RUBY_IMMEDIATE_MASK as VALUE)) != 0 || v == QNIL || v == QFALSE
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

//! The one hand-written C-ABI callback in this extension.
//!
//! Ruby's only allocation-free hash iteration is `rb_hash_foreach`,
//! which takes a C function pointer. magnus's closure wrapper for it
//! (`RHash::foreach`) costs a measured 3-8% on object-heavy
//! generation (per-pair `Value` wrapping, Ruby-handle construction,
//! and result plumbing), so this module packs the same closure shape
//! over the raw API: the closure is monomorphized straight into the
//! trampoline and a pair costs exactly the callback dispatch.
//! Everything else in the extension stays on magnus.

use rb_sys::VALUE;
use std::os::raw::c_int;

/// Continue/stop signal from the per-pair closure, mirroring
/// `st_retval`'s two values the callback may return here.
pub(super) enum Step {
    Continue,
    Stop,
}

/// Iterate `hash`'s pairs, passing raw VALUEs to `f`.
///
/// The closure must not let a Ruby exception cross this frame (route
/// raising calls through magnus's protect and report via [`Step`]);
/// panics abort the process (the extension builds with panic=abort),
/// never unwind into C.
///
/// # Safety
///
/// `hash` must be a live `T_HASH` VALUE.
pub(super) unsafe fn foreach_raw<F>(hash: VALUE, mut f: F)
where
    F: FnMut(VALUE, VALUE) -> Step,
{
    unsafe extern "C" fn trampoline<F>(k: VALUE, v: VALUE, arg: VALUE) -> c_int
    where
        F: FnMut(VALUE, VALUE) -> Step,
    {
        let f = unsafe { &mut *(arg as *mut F) };
        match f(k, v) {
            Step::Continue => 0,
            Step::Stop => 1,
        }
    }
    unsafe {
        rb_sys::rb_hash_foreach(
            hash,
            Some(trampoline::<F>),
            std::ptr::from_mut(&mut f) as VALUE,
        );
    }
}

//! Per-thread parser state and the GC-visible value-stack shadow.
//!
//! Pending VALUEs live on heap-allocated stacks that Ruby's GC can see
//! through a wrapped handle object whose magnus-driven mark walks the
//! live entries (the JSON gem's rvalue_stack design): precise marking
//! of only `values.len()` slots, growing on demand so huge flat arrays
//! (mesh.json) never overflow.

use ahash::AHashMap;
use magnus::typed_data::Obj;
use magnus::{DataTypeFunctions, TypedData};
use nosj::Buffers;
use std::cell::RefCell;

/// Everything a parse touches, allocated once per thread and reused:
/// nosj's scratch buffers, the interned-key caches, and the GC-marked
/// stacks.
pub(crate) struct PullState {
    pub(crate) bufs: Buffers,
    pub(crate) keys: AHashMap<Box<str>, rb_sys::VALUE>,
    /// Separate cache for symbolize_names mode: symbol and string VALUEs
    /// must never share a map.
    pub(crate) sym_keys: AHashMap<Box<str>, rb_sys::VALUE>,
    /// Leaked once per thread; kept alive + GC-marked via the wrapped
    /// handle.
    pub(crate) vstack: Option<&'static mut VStackShadow>,
    /// Marked shadow holding the cached key VALUEs; keys are kept alive by
    /// this (collectable on epoch clear), NOT by per-key eternal GC pins.
    pub(crate) key_shadow: Option<&'static mut VStackShadow>,
}

thread_local! {
    pub(crate) static PULL_STATE: RefCell<PullState> = RefCell::new(PullState {
        bufs: Buffers::new(),
        keys: AHashMap::with_capacity(256),
        sym_keys: AHashMap::new(),
        vstack: None,
        key_shadow: None,
    });
}

/// GC-marked holder for pending VALUEs.
pub(crate) struct VStackShadow {
    pub(crate) values: Vec<rb_sys::VALUE>,
}

/// Ruby-side handle over a leaked shadow: magnus drives the GC mark
/// through [`DataTypeFunctions::mark`] (its trampoline, not ours),
/// pinning every pending VALUE with `rb_gc_mark` semantics. The class
/// is defined (and made a private constant) at init.
#[derive(TypedData)]
#[magnus(class = "NOSJ::ValueStackShadow", mark)]
pub(crate) struct ShadowHandle(*const VStackShadow);

// SAFETY: the pointee is leaked for the process lifetime and each
// handle stays with the thread that created it; the only cross-thread
// access is the GC mark read, which runs at safepoints while the
// owning thread is parked (the same contract the previous
// rb_data_type_t dmark relied on).
unsafe impl Send for ShadowHandle {}

impl DataTypeFunctions for ShadowHandle {
    fn mark(&self, marker: &magnus::gc::Marker) {
        use magnus::rb_sys::FromRawValue;
        // SAFETY: the shadow is leaked for the process lifetime, and
        // GC marks at safepoints while the owning thread is not
        // mutating the stack (the same contract the previous
        // hand-written dmark relied on). Entries are live VALUEs
        // pushed by the sinks.
        let shadow = unsafe { &*self.0 };
        for &v in &shadow.values {
            marker.mark(unsafe { magnus::Value::from_raw(v) });
        }
    }
}

/// Create (once per thread) a leaked, GC-marked VStackShadow.
pub(crate) fn ensure_marked_shadow(slot: &mut Option<&'static mut VStackShadow>) {
    if slot.is_none() {
        let ruby = magnus::Ruby::get().expect("called on a Ruby thread");
        let shadow: &'static mut VStackShadow = Box::leak(Box::new(VStackShadow {
            values: Vec::with_capacity(1024),
        }));
        let ptr = std::ptr::from_mut::<VStackShadow>(shadow).cast_const();
        let handle: Obj<ShadowHandle> = ruby.obj_wrap(ShadowHandle(ptr));
        magnus::gc::register_mark_object(handle);
        *slot = Some(shadow);
    }
}

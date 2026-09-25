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
use std::cell::Cell;

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

impl PullState {
    fn fresh() -> Box<Self> {
        Box::new(PullState {
            bufs: Buffers::new(),
            keys: AHashMap::with_capacity(256),
            sym_keys: AHashMap::new(),
            vstack: None,
            key_shadow: None,
        })
    }
}

thread_local! {
    static PULL_STATE: Cell<Option<Box<PullState>>> = const { Cell::new(None) };
}

/// Run `f` on this thread's parse state, taken OUT of the thread-local
/// for the call and stored back afterwards (the generate scratch's
/// pattern, gen/mod.rs). Parses allocate Ruby objects, and a failed
/// allocation raises NoMemoryError, which longjmps over these frames: a
/// RefCell borrow held across it stayed borrowed for good, and the next
/// parse on the thread panicked, aborting the process under
/// panic=abort. Here a lost state is merely leaked (its shadows keep
/// their last VALUEs alive) and the next call starts a fresh one; an
/// empty cell likewise serves a nested call.
pub(crate) fn with_pull_state<R>(f: impl FnOnce(&mut PullState) -> R) -> R {
    let mut state = PULL_STATE.with(Cell::take).unwrap_or_else(PullState::fresh);
    let result = f(&mut state);
    PULL_STATE.with(|cell| cell.set(Some(state)));
    result
}

/// GC-marked holder for pending VALUEs.
pub(crate) struct VStackShadow {
    pub(crate) values: Vec<rb_sys::VALUE>,
}

/// Ruby-side handle over a leaked shadow: magnus drives the GC mark
/// through [`DataTypeFunctions::mark`] (its trampoline, not ours),
/// pinning every pending VALUE with `rb_gc_mark` semantics. The class
/// is defined (and made a private constant) at init.
///
/// Deliberately NOT `wb_protected`: parses push VALUEs into the shadow
/// with plain stores, no write barriers, so an old protected handle
/// would let the GC miss young values it holds (a use-after-free).
/// Staying write-barrier-unprotected makes every GC rescan the handle,
/// which costs nothing measurable: there are one to three per thread.
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
        ruby.gc_register_mark_object(handle);
        *slot = Some(shadow);
    }
}

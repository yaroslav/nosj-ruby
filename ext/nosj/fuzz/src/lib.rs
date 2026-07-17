//! Shared harness for the extension fuzz targets: boot one embedded
//! Ruby VM per process, register the extension exactly as `require
//! "nosj/nosj"` would, and load the Ruby-side differential checks.
//!
//! The targets exercise the gem-side byte-manipulation layers the
//! parser crate's own fuzzers never see (reformat, NDJSON framing,
//! splice/patch span arithmetic). The checking logic lives in
//! prelude.rb; a raised Ruby exception surfaces here as an Err and
//! aborts the run.

use std::sync::Once;

use magnus::prelude::*;
use magnus::{RModule, Ruby, Value};

static VM: Once = Once::new();

const PRELUDE: &str = include_str!("prelude.rb");

/// Run one fuzz iteration with the VM up and the checks defined.
/// libFuzzer drives every input on the same thread, which is the
/// thread the VM was initialized on.
pub fn with_vm<F: FnOnce(&Ruby, RModule)>(f: F) {
    VM.call_once(|| {
        let cluster = unsafe { magnus::embed::init() };
        // The VM must outlive every future iteration.
        std::mem::forget(cluster);
        let ruby = Ruby::get().expect("VM just initialized on this thread");
        unsafe { nosj_ext::Init_nosj() };
        ruby.eval::<Value>(PRELUDE).expect("fuzz prelude must load");
    });
    let ruby = Ruby::get().expect("fuzz iterations run on the VM thread");
    let checks = ruby
        .define_module("NOSJFuzz")
        .expect("prelude defines NOSJFuzz");
    f(&ruby, checks);
}

/// Feed raw bytes to a single-argument check; any raised exception
/// (an assertion failure or an unexpected error class) aborts.
pub fn drive(check: &str, data: &[u8]) {
    with_vm(|ruby, checks| {
        let input = ruby.str_from_slice(data);
        if let Err(e) = checks.funcall::<_, _, Value>(check, (input,)) {
            panic!("{check}: {e}");
        }
    });
}

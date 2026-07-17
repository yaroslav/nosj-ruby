//! Native extension of the nosj Ruby gem: Ruby bindings over the
//! first-party `nosj` SIMD JSON crate.
//!
//! - `parse.rs`: whole-document entry points (parse, valid?, the
//!   GVL-releasing indexed parse) plus shared option decoding and
//!   input gating.
//! - `pointer.rs`: partial parsing (dig, at_pointer, batch forms).
//! - `lazy.rs`: lazy documents (NOSJ.lazy nodes resolving access on
//!   demand over shared document bytes).
//! - `files.rs`: file entry points (load_file, write_file, and the
//!   mmap-backed load_lazy_file / at_pointer_file / dig_file).
//! - `sink.rs`: the VALUE-building and validation sinks with their
//!   interned-key caches.
//! - `state.rs`: per-thread reusable state and the GC-marked value
//!   stacks.
//! - `gen/`: generation (JSON.generate-compatible walker, options,
//!   key cache, protect shims, error mapping).

pub mod files;
pub mod gen;
pub mod lazy;
pub mod parse;
pub mod pointer;
pub mod sink;
pub mod state;

use magnus::{method, prelude::*, Error, Ruby};

// Init_nosj must match the required bundle's basename (nosj.bundle,
// "nosj/nosj"), not the package name nosj_native (see Cargo.toml).
#[magnus::init(name = "nosj")]
fn init(ruby: &Ruby) -> Result<(), Error> {
    compile_info();

    let module = ruby.define_module("NOSJ")?;
    module.define_class("ValueStackShadow", ruby.class_object())?;
    let _: magnus::Value =
        module.funcall("private_constant", (ruby.to_symbol("ValueStackShadow"),))?;
    module.define_singleton_method("parse_native", method!(parse::parse_native, 2))?;
    module.define_singleton_method("valid_native", method!(parse::valid_native, 2))?;
    module.define_singleton_method("dig_native", method!(pointer::dig_native, 2))?;
    module.define_singleton_method("dig_many_native", method!(pointer::dig_many_native, 3))?;
    module.define_singleton_method("at_pointer_native", method!(pointer::at_pointer_native, 3))?;
    module.define_singleton_method(
        "at_pointers_native",
        method!(pointer::at_pointers_native, 3),
    )?;
    module.define_singleton_method("lazy_native", method!(lazy::lazy_native, 2))?;
    module.define_singleton_method("load_file_native", method!(files::load_file_native, 2))?;
    module.define_singleton_method("write_file_native", method!(files::write_file_native, 3))?;
    module.define_singleton_method(
        "load_lazy_file_native",
        method!(files::load_lazy_file_native, 2),
    )?;
    module.define_singleton_method(
        "at_pointer_file_native",
        method!(files::at_pointer_file_native, 3),
    )?;
    module.define_singleton_method("dig_file_native", method!(files::dig_file_native, 2))?;
    let lazy_class = module.define_class("Lazy", ruby.class_object())?;
    // Nodes are only born from NOSJ.lazy / lazy resolution.
    lazy_class.undef_default_alloc_func();
    lazy_class.define_method("__get", method!(lazy::lazy_get, 1))?;
    lazy_class.define_method("__dig", method!(lazy::lazy_dig, 1))?;
    lazy_class.define_method("__at_pointer", method!(lazy::lazy_at_pointer, 1))?;
    lazy_class.define_method("__materialize", method!(lazy::lazy_materialize, 0))?;
    lazy_class.define_method("__kind", method!(lazy::lazy_kind, 0))?;
    lazy_class.define_method("__byte_size", method!(lazy::lazy_byte_size, 0))?;
    lazy_class.define_method("__keys", method!(lazy::lazy_keys, 0))?;
    lazy_class.define_method("__size", method!(lazy::lazy_size, 0))?;
    lazy_class.define_method("__children", method!(lazy::lazy_children, 0))?;
    module.define_singleton_method("generate_native", method!(gen::generate_native, 2))?;
    module.define_singleton_method(
        "generate_rails_native",
        method!(gen::generate_rails_native, 3),
    )?;
    // `generate` itself is native and variadic: the json gem routes
    // its `generate` through a Ruby frame into C, so skipping our own
    // forwarder frame is a straight per-call win on small documents.
    module.define_singleton_method("generate", method!(gen::generate_entry, -1))?;
    Ok(())
}

/// Debug builds announce themselves so a stray unoptimized bundle is
/// never benchmarked by accident; release builds load silently.
#[inline]
fn compile_info() {
    #[cfg(debug_assertions)]
    println!("nosj: running in DEBUG mode — do not benchmark this build");
}

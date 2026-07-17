//! Byte-splicing edits and RFC 6902 patch vs a pure-Ruby tree
//! reference. Input is "document \0 spec": an object spec drives
//! splice, an array spec drives patch.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    nosj_ruby_fuzz::drive("patch_case", data);
});

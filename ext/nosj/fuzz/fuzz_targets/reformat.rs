//! Reformat pipe vs whole-document parse: acceptance must agree
//! (valid?/stats included), minify must round-trip and be idempotent,
//! pretty must round-trip.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    nosj_ruby_fuzz::drive("reformat_case", data);
});

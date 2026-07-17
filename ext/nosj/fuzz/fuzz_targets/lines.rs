//! NDJSON framing vs one parse per line, plus a generate_lines
//! round-trip over whatever the streaming side yielded.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    nosj_ruby_fuzz::drive("lines_case", data);
});

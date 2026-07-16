## [0.1.0] - 2026-07-15

Initial release.

- `NOSJ.parse`, `NOSJ.generate`, and `NOSJ.pretty_generate`: `json`-gem-compatible parsing and generation—same output bytes, same option names, same error classes and messages—built on the first-party SIMD [nosj](https://crates.io/crates/nosj) crate (NEON on Apple Silicon; SSE2/AVX2 on x86-64, selected at runtime). Faster than the `json` gem and the third-party parsers (Oj, RapidJSON, FastJsonparser, Yajl) across the benchmark corpus, in both directions.
- Partial parsing: `NOSJ.dig` and `NOSJ.at_pointer` resolve a JSON Pointer and materialize only the matched subtree; `NOSJ.dig_many` and `NOSJ.at_pointers` resolve whole batches of paths in a single pass over the document.
- `NOSJ.valid?`: full-strictness validation that allocates no Ruby objects.
- Drop-in acceleration: `require "nosj/json"` reroutes `JSON.parse`, `JSON.generate`, `JSON.pretty_generate`, and `JSON.dump` through nosj, falling back to the original implementation for unsupported options; `require "nosj/multi_json"` adds a MultiJson adapter.
- Precompiled platform gems, each built natively with profile-guided optimization: Linux x86-64 and arm64 (glibc and musl), macOS (Apple Silicon), Windows (x64), for Ruby 3.3 through 4.0. Other platforms compile the source gem.
- RBS signatures and full YARD documentation.

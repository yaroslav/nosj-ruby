## [Unreleased]

- File APIs. `NOSJ.load_file(path, opts)` parses a file directly
  (~1.3× faster than `parse(File.read(path))`—no file-sized Ruby
  String is created), and `NOSJ.write_file(path, obj, opts)` generates
  straight to disk, returning the byte count like `File.write`.
  `NOSJ.load_lazy_file(path, opts)` wraps a file as a lazy document
  over a read-only memory map, and `NOSJ.at_pointer_file` /
  `NOSJ.dig_file` pull single values out of a file without reading the
  rest into Ruby. Missing files raise the usual `Errno` exceptions.
- `NOSJ.lazy`: lazy documents. Wrap a document once, then read only
  what you need: `doc["users"][3]["name"]` parses just that path, `#dig`
  and `#at_pointer` resolve whole paths, and `#keys`, `#size`, and
  `#each` inspect a node without parsing its values. Containers come
  back lazy, scalars come back as plain Ruby values, and repeated
  reads are cached. `#value` (also `#to_h` / `#to_a`) materializes a
  subtree under the usual parse options (`symbolize_names`, `freeze`,
  ...). Pass a frozen string and creating the view is practically
  free, even on megabyte documents. Malformed content raises on first
  read, not at wrap time.

## [0.1.0] - 2026-07-16

Initial release.

- `NOSJ.parse`, `NOSJ.generate`, and `NOSJ.pretty_generate`: `json`-gem-compatible parsing and generation—same output bytes, same option names, same error classes and messages—built on the first-party SIMD [nosj](https://crates.io/crates/nosj) crate (NEON on Apple Silicon; SSE2/AVX2 on x86-64, selected at runtime). Faster than the `json` gem and the third-party parsers (Oj, RapidJSON, FastJsonparser, Yajl) across the benchmark corpus, in both directions.
- Partial parsing: `NOSJ.dig` and `NOSJ.at_pointer` resolve a JSON Pointer and materialize only the matched subtree; `NOSJ.dig_many` and `NOSJ.at_pointers` resolve whole batches of paths in a single pass over the document.
- `NOSJ.valid?`: full-strictness validation that allocates no Ruby objects.
- Drop-in acceleration: `require "nosj/json"` reroutes `JSON.parse`, `JSON.generate`, `JSON.pretty_generate`, and `JSON.dump` through nosj, falling back to the original implementation for unsupported options; `require "nosj/multi_json"` adds a MultiJson adapter.
- Precompiled platform gems, each built natively with profile-guided optimization: Linux x86-64 and arm64 (glibc and musl), macOS (Apple Silicon), Windows (x64), for Ruby 3.3 through 4.0. Other platforms compile the source gem.
- RBS signatures and full YARD documentation.

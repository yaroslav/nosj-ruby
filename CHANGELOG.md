## [Unreleased]

**json 3.0 compatibility.** nosj now matches the behavior of the json
gem 3.0, and stays compatible with both the json 2.x and 3.0
interfaces: `NOSJ.parse`, `NOSJ.generate`, and the rest of nosj's own
API follow json 3.0 semantics, while the `nosj/json` drop-in follows
whichever json gem your application has installed, 2.x or 3.0, down to
its calling conventions and the documents it accepts. Upgrading json
(or not) is your choice; nosj works with either.

Behavior changes in nosj's own API, for input that was accepted
before:

- Duplicate object keys raise `NOSJ::ParserError` (positioned at the
  object repeating the key, like json 3) in `parse`, `load_file`,
  `valid?` (returns false), `minify`/`reformat`, and everything that
  materializes values. `allow_duplicate_key: true` restores the old
  behavior: the last value wins (in `minify`, repeated keys pass
  through).
- Lone UTF-16 surrogates such as `"\udc00"` raise `NOSJ::ParserError`
  everywhere, trailing ones included (they used to decode to raw
  WTF-8 bytes, and `minify` re-escaped them).
- `generate` raises `NOSJ::GeneratorError` for keys that render alike
  (`{"a" => 1, :a => 2}`, `{1 => 1, "1" => 2}`), with json 3's exact
  message; `allow_duplicate_key: true` emits them as before. Hashes
  whose keys are all of one kind are never checked. The Rails encoder
  is unchanged.
- `stats` still describes such documents rather than refusing them.
- Unknown options raise ArgumentError with json 3's message
  (`unknown keyword: foo`) in every entry point, instead of being
  ignored. json options nosj does not implement (`object_class`,
  `array_class`, `decimal_class`, `on_load`, `create_additions`,
  `allow_comments`, `allow_control_characters`,
  `allow_invalid_escape`, `sort_keys`, `as_json`) raise unless falsy;
  `on_load` and the newer ones used to be silently ignored.
  `escape_slash` is gone, as in json 3: use `script_safe`.
  `quirks_mode` is no longer accepted. `stats` takes only the options
  it documents (`max_nesting`, `allow_nan`, `allow_trailing_comma`).

The `nosj/json` drop-in follows the installed json gem:

- With json 3.0, `JSON.parse` takes keyword options only, `JSON.dump`
  uses json 3's defaults (nesting capped at 100), and whatever json 3
  refuses (`quirks_mode`, `escape_slash`, `create_additions`, unknown
  options, positional option hashes) raises exactly as json 3 raises.
- With json 2.x, everything behaves as before, including json 2's
  acceptance of duplicate keys, lone surrogates, and comments, and
  its handling of keys that render alike.
- Whenever the fast path refuses a call, the installed gem runs it
  again and decides: exceptions are now the gem's own (message,
  `json_path`, `invalid_object`) rather than nosj's messages re-raised
  as JSON classes. The second pass happens on failures only, and a
  `generate` run twice this way calls `to_json` again on the objects
  visited before the refusal.
- Fixed: `JSON.dump` raised NameError (`_dump_default_options`) with
  json older than 2.11, which includes the json bundled with Ruby 3.3
  and 3.4.

## [0.4.1] - 2026-09-25

- Fixed a crash in `NOSJ.generate`: an object whose `to_json` or
  `to_s` shrank the array being generated (for example with
  `Array#clear`) made the generator read freed memory, usually a
  segfault. The array length is now re-read for every element, like
  the json gem, so elements a callback appends are emitted too.
- Fixed memory corruption in `NOSJ.splice`: a replacement value whose
  `to_json` modified the source string, deduplicated a frozen String
  subclass (`-str`), or removed entries from the edits hash could make
  splice read freed memory, copying unrelated heap bytes into the
  result or crashing. All values are now generated before the source
  is read.
- Fixed `NOSJ.lazy` and `NOSJ.each_line` reading freed memory when the
  source is a frozen String subclass (such as
  `ActiveSupport::SafeBuffer`) or a frozen string carrying instance
  variables, and it is deduplicated with `-str` while the lazy
  document is alive or between lines: Ruby swaps such a string's
  buffer, and the old one kept being read (wrong values, or a crash).
- Fixed a memory leak in `NOSJ.generate` and `NOSJ.write_file`: an
  exception raised by user code the generator calls bypassed its
  cleanup and leaked its output buffer (megabytes per call after large
  documents). Affected: a raising `respond_to?` or
  `respond_to_missing?`, a raising `to_s` on an encoding-conversion
  error, a raising autoload of `JSON::Fragment` (strict and Rails
  modes), and a raising `Errno` constructor for a failed write. The
  exception still propagates unchanged.
- Fixed: after a `NoMemoryError` in the middle of a parse (or
  `minify`/`reformat`), the next call on that thread aborted the whole
  process. Per-thread parser state is now recovered instead.
- `NOSJ::Lazy` nodes now take part in generational GC: holding many
  nodes no longer slows down every minor GC (200,000 live nodes: 2.9 ms
  per minor GC before, 0.1 ms now), and they are freed immediately
  when collected.
- Fixed: lazy documents opened with `allow_trailing_comma: true` or
  `allow_nan: true` could not be walked. `size`, `keys`, `each`, and
  any lookup that missed or stepped over a trailing comma or a `NaN`
  raised `NOSJ::ParserError`, even though `value` worked.
- `NOSJ.at_pointer`, `NOSJ.at_pointers`, and `NOSJ.at_pointer_file`
  now honor `allow_nan` and `allow_trailing_comma` while resolving the
  pointer, not only when materializing the matched value.
- Updated the nosj crate to 0.2.2.
- Updated dependencies, Magnus bumped to 0.9.0.

## [0.4.0] - 2026-09-05

- Ractors: on Ruby 4.0+, `NOSJ.parse`, `NOSJ.generate`, and every
  other entry point (lazy documents, partial parsing, the file APIs,
  NDJSON, patches, reformatting, statistics) can be called from inside
  a Ractor. Before this release each of them raised
  `Ractor::UnsafeError` outside the main Ractor. Values parsed with
  `freeze: true` are Ractor-shareable, so a document parsed in one
  Ractor can be handed to another without copying.
- Updated Rust dependencies.

## [0.3.2] - 2026-07-19

- **IMPORTANT**—Fixed: the precompiled platform gems linked `libruby`
  into the extension. The arm64-darwin binaries recorded the build runner's
  absolute Ruby path, so `require "nosj"` failed with `Library not
  loaded: /Users/runner/hostedtoolcache/...` on any machine with Ruby
  installed elsewhere (#2), and the Linux binaries carried a
  `libruby.so` runtime dependency that a statically built Ruby (the
  ruby-build default) cannot satisfy. The cause was magnus's `embed`
  feature—needed only by the fuzz harness, which now enables it
  itself—pulling `rb-sys/link-ruby` into the gem build. Extension
  symbols now resolve from the host process at load time, and the
  release build refuses to stage a binary that links libruby.

## [0.3.1] - 2026-07-17

- Fixed: `NOSJ.minify` / `NOSJ.reformat` produced unparseable output
  for a float that overflows to Infinity—a huge-exponent literal like
  `1e999`, or a ~300-digit integer with an exponent (such literals
  parse to Infinity even in strict mode, matching the `json` gem).
  The pipe emitted a bare `Infinity` token; it now raises the same
  `GeneratorError` that `generate` would
  (`"Infinity not allowed in JSON"`). With `allow_nan: true` the
  literal still passes through as `Infinity`. Found by fuzzing.
- Fixed (second manifestation, also found by fuzzing): the same
  unparseable output could appear with the overflowing literal hidden
  behind a duplicate object key. Because the reformat pipe
  deliberately preserves duplicate-key entries, it now refuses such
  documents with the same `GeneratorError` even though
  `generate(parse(x))` succeeds there (last-key-wins parsing discards
  the shadowed value)—a documented divergence.
- Differential fuzzing for the native extension (`ext/nosj/fuzz`):
  three cargo-fuzz targets—reformat, NDJSON framing, and
  byte-splicing/JSON Patch—each drive the real entry points on an
  embedded Ruby VM and compare every input against pure-Ruby
  reference implementations, with committed seed corpora and a weekly
  CI workflow (`fuzz.yml`).

## [0.3.0] - 2026-07-17

- Reformat without parsing. `NOSJ.minify(json, opts)` and
  `NOSJ.reformat(json, opts)` (plus `NOSJ.reformat_file`) pipe the
  parser's events straight into the emission kernels: zero Ruby
  objects are allocated for the document, and output is exactly
  `generate(parse(json))`—canonical numbers, normalized escapes, the
  full set of `generate` formatting and escape options, with
  `pretty: true` as a `pretty_generate` shorthand—except duplicate
  object keys pass through and lone-surrogate string values re-escape
  as `\uXXXX` instead of raising (the output must always reparse).
  Acceptance options apply per `parse` (`allow_trailing_comma`
  normalizes the commas away). Measured on the 631 KB twitter.json:
  409µs, 3.4× faster than `NOSJ.generate(NOSJ.parse(x))`, 3.9× faster
  than gem json's cycle, 5.3× faster than Oj's, and 1.4× the cost of
  `NOSJ.valid?`.
- Byte-splicing edits and JSON Patch. `NOSJ.splice(json, pointer =>
  value, ...)` replaces values directly in the text: every target
  resolves in one forward pass and the result is rebuilt copying all
  bytes outside the target spans untouched (formatting, key order, and
  number spellings elsewhere survive exactly). Measured on
  twitter.json: 10× faster than parse-mutate-generate for a late
  field, 51× for an early one. Missing targets raise KeyError,
  overlapping targets ArgumentError. `NOSJ.patch(json, ops)` applies
  RFC 6902 JSON Patch (add/remove/replace/move/copy/test, String or
  Symbol op keys) to the raw string the same way, with structural ops
  walking only the parent container's span; application failures raise
  the new `NOSJ::PatchError`, malformed patch documents ArgumentError.
  `NOSJ.merge_patch(json, patch)` applies RFC 7386 JSON Merge Patch
  (semantic form). Inserted values are byte-identical to
  `NOSJ.generate` and accept its options; the RFC 6902 appendix-A
  suite and the full RFC 7386 test table are in the specs.
- NDJSON / JSON Lines. `NOSJ.each_line(source, opts)` yields one
  parsed value per line (Enumerator without a block, so
  `.first(10)`/`.lazy` walk only what they consume), skipping blank
  lines and enforcing one value per line; a malformed line raises the
  rich `ParserError` whose `#line` is the physical line number in the
  stream. `NOSJ.generate_lines(values, opts)` emits one compact
  newline-terminated document per element in a single buffer pass
  (measured 4.1× faster than the map-generate-join idiom on twitter
  statuses; `each_line` is 1.6× faster than line-split +
  `JSON.parse`), rejecting formatting options that would break line
  framing. File forms: `NOSJ.each_line_file` streams over a read-only
  memory map, `NOSJ.write_lines` generates straight to disk and
  returns the byte count. Parse options apply per line; pass a frozen
  string to `each_line` for zero-copy iteration.
- `NOSJ.stats(source, opts)` / `NOSJ.stats_file(path, opts)`: document
  statistics from one counting pass through the null-sink machinery,
  answering "what is this 40 MB blob" without building any Ruby values
  for the document (measured ~1.3× faster than a full parse). Reports
  `byte_size`, `root` kind, `max_depth`, value counts by type, key
  totals, a key histogram sorted by count, largest container sizes,
  and string byte totals. Nesting is unlimited by default (pass
  `max_nesting` to enforce a limit); `allow_nan` and
  `allow_trailing_comma` are honored; malformed documents raise the
  rich `ParserError`. The file form memory-maps, so the document never
  enters Ruby at all.
- Rich parse errors. Parse failures now raise `NOSJ::ParserError`
  (previously a bare `RuntimeError`) carrying the failure position,
  computed only when a parse fails: `#byte_offset`, 1-based `#line`,
  character-based `#column`, and a caret `#snippet` showing the
  offending line (windowed when the line is long, as minified JSON
  usually is). Positions are absolute within the document you passed,
  including through partial parsing (`dig`, `at_pointer`, batches),
  lazy documents, and the file APIs. `#detailed_message` appends the
  snippet, so unrescued errors print it. Failures with no position
  (encoding refusals) leave the accessors nil. Exceeding `max_nesting`
  during parsing now raises `NOSJ::NestingError`, matching the gem's
  class (generation already did); rescues of the old `RuntimeError`
  need updating to `NOSJ::ParserError`/`NOSJ::Error`.
- Rails mode: `require "nosj/rails"` accelerates a Rails application
  in both directions. It installs a nosj-backed ActiveSupport JSON
  encoder, so `obj.to_json`, `render json:`, and `ActiveSupport::JSON.encode` walk the object tree natively—values recurse through `as_json` exactly
  like ActiveSupport's own encoder. It also loads the `nosj/json` drop-in, so
  `ActiveSupport::JSON.decode` and JSON request-body parsing take the
  fast path (including on Rails 7.x, whose `quirks_mode` option the
  drop-in now accepts; the drop-in also accepts valid-UTF-8 BINARY
  strings now, which is what Rack delivers request bodies as). The
  HTML-safety escaping is fused into the SIMD string-emission kernels,
  so escaped output costs the same single pass as unescaped. Measured
  against stock ActiveSupport encoding: ×1.7 on small documents up to
  ×5.2 on large trees and ×14 on HTML-heavy content
  (`rake bench:rails`). In a Rails Gemfile:
  `gem "nosj", require: "nosj/rails"`.
- `JSON::Fragment` values now splice their pre-rendered JSON
  everywhere the `json` gem does: in default mode, under `strict:
  true`, and through the Rails encoder.

## [0.2.0] - 2026-07-16

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

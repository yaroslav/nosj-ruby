# gem nosj

**gem nosj** is an **very fast JSON parser and generator for Ruby**, written in Rust on the first-party [nosj](https://github.com/yaroslav/nosj) crate and **SIMD-accelerated** on every platform (NEON on Apple Silicon, SSE2/AVX2 on x86-64).

> gem nosj is the powerful evil twin of the json gem.

[![GitHub Release](https://img.shields.io/github/v/release/yaroslav/nosj-ruby)](https://github.com/yaroslav/nosj-ruby/releases)
[![Docs](https://img.shields.io/badge/yard-docs-blue.svg)](https://rubydoc.info/gems/nosj)

- It is **faster** than gem json and every
third-party parser, including Oj, RapidJSON, FastJsonparser, Yajl. 1.0–1.8× faster than the bundled json gem, 1.3–11× faster than Oj, and up to 17×
faster than Yajl—[see Benchmarks](#benchmarks).
- It has **lazy documents**: `NOSJ.lazy` wraps a document and parses a value only when you touch it—repeated access costs nanoseconds, and everything you never read is never parsed.
- It has a **partial parsing mode**: JSON Pointer lookups that pull single values out of big documents in microseconds, skipping everything else.
- It has **file APIs**: parse, generate, dig, and lazy-wrap files directly—no throwaway file-sized Ruby String, and the partial modes memory-map the file so unread pages never even leave the disk.
- It is **great to debug with**: parse errors carry line, column, and a caret snippet pointing at the break, and `NOSJ.stats` X-rays a mystery blob (depth, value counts, key histogram) faster than parsing it.
- It accelerates a **Rails** application in both encoding and decoding.
- It comes **precompiled** (platform gems built with per-platform optimizations,
nothing to compile on install).
- Otherwise, same API and option names as gem json.

**And there's more**: validate documents without building a single Ruby object, resolve whole batches of paths in one pass, and accelerate an entire application with a one-line drop-in.

- [Requirements](#requirements)
- [Getting started](#getting-started)
- [What's in the box](#whats-in-the-box)
- [Benchmarks](#benchmarks)
- [Switching from the json gem](#switching-from-the-json-gem)
- [How it works](#how-it-works)
- [Development](#development)
- [License](#license)

## Requirements

- **Ruby 3.3 or newer** (CRuby; tested on 3.3, 3.4, and 4.0).
- **Linux** (x86-64 and arm64, glibc and musl), **macOS** (Apple
  Silicon), or **Windows** (x64): these platforms install precompiled,
  per-platform-optimized gems with nothing to build.

## Getting started

```bash
bundle add nosj
```

```ruby
require "nosj"

NOSJ.parse('{"a":[1,true]}')       #=> {"a" => [1, true]}
NOSJ.generate({"a" => [1, true]})  #=> '{"a":[1,true]}'
```

That's it—if you know the `json` gem, you already know `nosj`.

### gem json compatibility

Want the speedup without touching your code? One line reroutes
`JSON.parse`, `JSON.generate`, `JSON.pretty_generate`, and `JSON.dump`
through nosj:

```ruby
require "nosj/json"
```

In a Bundler app (Rails included) that can live entirely in the
Gemfile you can do this:

```ruby
gem "nosj", require: "nosj/json"
```

A MultiJson adapter ships too: 

```ruby
require "nosj/multi_json"
```

And then `MultiJson.use NOSJ::MultiJsonAdapter`.

### Ruby on Rails

In a Rails app, use this for "Rails mode":

```ruby
gem "nosj", require: "nosj/rails"
```

That installs a nosj-backed ActiveSupport JSON encoder, so `obj.to_json`, `render json:`, and `ActiveSupport::JSON.encode` walk the object tree natively:
values that aren't JSON-native recurse through `as_json` exactly like
ActiveSupport's own encoder, non-finite floats encode as `null`, and
HTML-safety escaping (`escape_html_entities_in_json`) behaves
identically—verified differentially against ActiveSupport's encoder.
It loads the drop-in too, so `ActiveSupport::JSON.decode` and JSON
request-body parsing ride the fast path. 

Measured against ActiveSupport's own encoder: ×1.9 on small documents
up to ×5.2 on large trees and ×14 on HTML-heavy content—see
[Benchmarks → Rails mode](#rails-mode).

## What's in the box

**The `json` gem API**, on the `NOSJ` module:

```ruby
NOSJ.parse(src, symbolize_names: true)   # also: freeze, max_nesting,
                                         # allow_nan, allow_trailing_comma
NOSJ.generate(obj)                       # indent, space, object_nl, ...,
NOSJ.pretty_generate(obj)                # ascii_only, script_safe, strict
```

**Lazy documents.** `NOSJ.lazy` wraps a document in a lazy view: read
a field and only that path is parsed—containers stay lazy, scalars
arrive as plain Ruby values, and repeated reads are cached:

```ruby
doc = NOSJ.lazy(json)
doc["users"][3]["name"]   # parses only this path
doc.dig("meta", "count")  # a whole path in one fused resolution
doc["users"].size         # counted without materializing anything
doc["users"][3].value     # materialize one subtree (parse options apply)
```

Pass a frozen string and creating the view is practically free—
nanoseconds, even on a megabyte document. Malformed content raises
when it is first read, not at wrap time.


**Partial parsing.** Pull values out of a document without
materializing the rest—skipped content is stepped over at SIMD block
speed, so a lookup costs what it skips, not what the document weighs:

```ruby
NOSJ.dig(json, "users", 3, "name")        # Hash#dig-shaped
NOSJ.at_pointer(json, "/users/3/name")    # JSON Pointer

# Many lookups in one pass. A batch costs about as much as its
# single deepest member:
NOSJ.at_pointers(json, ["/users/3/name", "/meta/count"])
NOSJ.dig_many(json, [["users", 3, "name"], ["meta", "count"]])
```

Example: an early field resolves in ~0.35µs where `JSON.parse(json).dig(...)`
costs ~980µs on the same document—three orders of magnitude. A field
at the far end of a 570 KB document costs ~71µs, still 13× faster
than parse-then-dig. Misses return nil; matched subtrees materialize
with the same options as `parse` (`symbolize_names:`, `freeze:`).

**Files.** Every mode has a file-native form, so a document never
round-trips through a throwaway Ruby String:

```ruby
NOSJ.load_file("config.json")                 # 1.3× File.read + parse
NOSJ.write_file("out.json", obj)              # generate straight to disk
NOSJ.dig_file("huge.json", "users", 3, "name")   # never reads the rest
NOSJ.at_pointer_file("huge.json", "/meta/count")
doc = NOSJ.load_lazy_file("huge.json")        # lazy view over a memory map
```

The partial and lazy forms memory-map the file, so pages you never
read are never loaded from disk. Missing files raise the usual
`Errno` exceptions. Measured numbers live in
[Benchmarks → File APIs](#file-apis).

**Validation without parsing.** `NOSJ.valid?` runs the full
parser—tokenizers, string decode, number validation—into a null sink
and allocates no Ruby objects at all. It is 2-4× faster than
`NOSJ.parse`, which already leads every parser above:

```ruby
NOSJ.valid?('{"a":1}')                #=> true
NOSJ.valid?('{"a":}')                 #=> false
NOSJ.valid?(src, max_nesting: false)  # same options as parse
```

**Document statistics.** `NOSJ.stats` answers "what is this 40 MB
blob": one counting pass through the same null-sink machinery—no Ruby
value is built for the document, and it costs *less* than a parse
(~1.3× faster on twitter.json):

```ruby
s = NOSJ.stats(blob)          # or NOSJ.stats_file("huge.json")
s[:byte_size]                 #=> 631514
s[:root]                      #=> :object
s[:max_depth]                 #=> 10
s[:values]                    #=> {total: 13914, objects: 1264, arrays: 1050,
                              #    strings: 4754, integers: 2108, ...}
s[:keys]                      #=> {total: 13345, unique: 94}
s[:key_histogram].first(3)    #=> [["id", 447], ["id_str", 447], ["urls", 364]]
s[:containers]                #=> {max_object_entries: 40, max_array_length: 100}
s[:strings]                   #=> {bytes: 200716, max_bytes: 463}
```

Nesting is unlimited by default (a deep blob is exactly what a
diagnostic should describe); malformed documents raise the usual rich
`ParserError`.

**Rich parse errors.** A failed parse raises `NOSJ::ParserError`
carrying where the document broke—`#byte_offset`, `#line`, `#column`,
and a caret `#snippet`—computed only when a parse fails, so success
pays nothing. Positions stay absolute through partial parsing, lazy
documents, and the file APIs; unrescued errors print the snippet:

```ruby
NOSJ.parse(%({\n  "a": 1,\n  "b": }))
# NOSJ::ParserError: unexpected character at byte 19
#   e.line     #=> 3
#   e.column   #=> 8
#   e.snippet  #=>   "b": }
#                         ^
```

## Benchmarks

Every installed JSON gem, benchmark-ips: AWS EC2 c7a.2xlarge (AMD EPYC 9R14, Zen 4), Ruby 4.0.6 + YJIT, json 2.21.1, Oj 3.17.4, RapidJSON 0.4.0, FastJsonparser 0.6.0, Yajl 1.4.3, PGO build, 2026-07-16. `×N` = times slower than nosj. 

### Parse

| file | nosj (i/s) | json | Oj | FastJsonparser | RapidJSON | Yajl |
|---|---:|---:|---:|---:|---:|---:|
| activitypub | **12.4k** | ×1.18 | ×1.53 | ×1.89 | ×1.99 | ×4.57 |
| canada | **248** | ×1.10 | ×8.28 | ×1.45 | ×1.51 | ×4.96 |
| citm_catalog | **504** | ×1.03 | ×1.97 | ×2.07 | ×1.83 | ×4.93 |
| gsoc-2018 | **397** | ×1.30 | ×1.47 | ×1.81 | ×1.80 | ×4.78 |
| homebrew-formula | **15.2** | ×1.13 | ×1.54 | ×2.42 | ×2.02 | ×4.30 |
| homebrew-llvm | **49.3k** | ×1.60 | ×1.48 | ×1.50 | ×1.85 | ×5.11 |
| mesh | **1.1k** | ×1.30 | ×3.93 | ×1.82 | ×1.95 | ×6.96 |
| numbers | **5.9k** | ×1.19 | ×4.64 | ×1.43 | ×1.75 | ×7.04 |
| ohai | **14.4k** | ×1.47 | ×1.61 | ×2.11 | ×1.97 | ×4.71 |
| simple | **977k** | ×1.31 | ×1.77 | ×2.08 | ×1.63 | ×5.15 |
| small_mixed | **3.2M** | ×1.48 | ×2.85 | ×2.44 | ×1.82 | ×6.75 |
| tolstoy | **8.9k** | ×1.79 | ×1.96 | ×2.29 | ×2.10 | ×17.31 |
| twitter | **1.1k** | ×1.09 | ×1.83 | ×2.25 | ×2.61 | ×5.40 |

### Generate

| file | nosj (i/s) | json | Oj | RapidJSON | Yajl |
|---|---:|---:|---:|---:|---:|
| activitypub | **34.8k** | ×1.20 | ×1.78 | ×2.41 | ×5.84 |
| canada | **150** | ×0.98\* | ×10.98 | ×11.20 | ×10.87 |
| citm_catalog | **1.3k** | ×1.04 | ×1.44 | ×1.55 | ×2.85 |
| gsoc-2018 | **1.1k** | ×1.28 | ×2.64 | ×3.64 | ×11.00 |
| homebrew-formula | **20.9** | ×1.07 | ×1.25 | ×1.62 | ×3.03 |
| homebrew-llvm | **71.5k** | ×1.23 | ×2.22 | ×3.04 | ×5.80 |
| mesh | **613** | ×1.08 | ×9.24 | ×9.40 | ×9.26 |
| numbers | **2.1k** | ×1.03 | ×10.63 | ×11.00 | ×10.69 |
| ohai | **39.4k** | ×1.07 | ×1.29 | ×1.50 | ×3.37 |
| simple | **2.2M** | ×1.03 | ×1.58 | ×1.62 | ×4.36 |
| small_mixed | **5.6M** | ×1.09 | ×2.26 | ×1.83 | ×7.16 |
| tolstoy | **8.3k** | ×1.30 | ×4.15 | ×6.50 | ×14.54 |
| twitter | **2.9k** | ×1.09 | ×1.46 | ×2.01 | ×4.05 |

\* canada-generate is a statistical tie with the json gem (within
measurement error).

### File APIs

twitter.json (570 KB) from a warm page cache, medians of 7 alternating
rounds against the plain-Ruby composition on the same parser (Apple
Silicon dev box, Ruby 4.0.6 + YJIT, PGO build, 2026-07-16):

| operation | µs/op | vs the Ruby way |
|---|---:|---|
| `NOSJ.load_file` (parse the whole file) | 948 | ×1.33 vs `NOSJ.parse(File.read(path))` |
| `NOSJ.dig_file` (one deep field) | 246 | ×5.2 vs read + parse + dig |
| `NOSJ.load_lazy_file` + one field | 257 | ×5.0 vs read + parse + dig |

`NOSJ.write_file` measures at parity with
`File.write(NOSJ.generate(obj))` on this box—file-write timings swing
too much for an honest multiplier; what it saves is the intermediate
file-sized Ruby String.

### Rails mode

`ActiveSupport::JSON.encode` with the nosj encoder installed, against
stock ActiveSupport (Apple Silicon dev box, Ruby 4.0.6 + YJIT,
activesupport 8.1, medians of 5 interleaved per-process rounds,
outputs verified byte-identical first, 2026-07-17;
`rake bench:rails`):

| workload | nosj (i/s) | vs ActiveSupport |
|---|---:|---|
| twitter tree (570 KB) | 4.5k | ×5.2 |
| 100-record index (with timestamps) | 33.7k | ×1.7 |
| HTML-heavy user content | 177.7k | ×14.2 |
| Time/Date/BigDecimal hash | 1.1M | ×3.0 |
| small API hash | 4.2M | ×1.9 |
| small hash `to_json` | 4.1M | ×1.9 |

The HTML-safety escaping that dominates stock encodes of
user-generated content is fused into the SIMD string-emission kernels
here: escaped output costs the same single pass as unescaped.

Reproduce with `rake bench` (the parity-gated comparison, after a PGO retrain—the shipping configuration) or `rake bench:ips` (the multi-gem shoot-out).

## Switching from the json gem

You mostly don't have to do anything. Some differences:

- The legacy object-deserialization options (`create_additions`,
  `object_class`, `array_class`, `decimal_class`) raise ArgumentError;
  the `nosj/json` drop-in falls back to the original gem for them.
- Behaviors the `json` gem itself deprecates (JS comments, raw invalid
  UTF-8) follow the strict semantics instead.
- Unlike `Array#dig`, negative indices in `NOSJ.dig` return nil (JSON
  Pointer has no equivalent).
- Parse errors raise `NOSJ::ParserError` (`NOSJ::NestingError` past
  `max_nesting`, like the gem); messages use byte offsets rather than
  the gem's phrasing, and the exception carries `#line`, `#column`,
  and a caret `#snippet`.

Everything else—including the gem's exact float formatting, which is
not the shortest-round-trip form most libraries emit—matches
byte-for-byte and is verified continuously against the full corpus.

## How it works

Most fast parsers build their own tree first and convert it into Ruby
objects second, paying for every string and container twice. nosj is
built on the [nosj](https://github.com/yaroslav/nosj) Rust crate, an
event parser with no tree of its own:

- **No intermediate tree.** The crate parses with NEON/SSE2/AVX2 SIMD
  kernels and emits *events*; the extension builds interned hash keys,
  strings, and containers directly on Ruby's heap during the parse,
  with GC-safe value stacks and epoch-evicted key caches. Generation
  walks Ruby objects once, streaming through fused scan-and-store
  escape kernels.
- **Byte-exact floats.** Output reproduces the json gem's fpconv
  (Grisu2) float format digit for digit—round-tripping is verified,
  not assumed.
- **PGO everywhere.** Local builds, CI, and every precompiled platform
  gem train on the benchmark corpus before the shipping compile; the
  precompiled binaries use portable codegen with SIMD tiers detected at
  runtime.

## Development

```bash
bundle exec rake compile                # build the extension (applies a PGO profile if present)
bundle exec rake spec                   # the gem-parity suite
bundle exec rake bench                  # PGO retrain + the parity-gated sweep vs the json gem
bundle exec rake bench:fast             # the sweep without retraining
bundle exec rake "bench:ips[twitter]"   # multi-gem shoot-out (benchmark-ips); no args = full corpus
```

## License

MIT. The underlying Rust crate is `MIT AND BSL-1.0 AND Apache-2.0`; its
NOTICE file itemizes the derived components.

# Extension fuzzing

Differential fuzzing for the gem-side byte-manipulation layers the
[nosj](https://github.com/yaroslav/nosj) crate's own fuzzers never
see. Each target boots one embedded Ruby VM per process, registers the
extension exactly as `require "nosj/nosj"` would, and checks every
input against a pure-Ruby reference check (`src/prelude.rb`); any
divergence, unexpected exception class, or panic aborts with an
artifact.

| Target | What it checks |
|--------|----------------|
| `reformat` | minify/reformat acceptance agrees with parse (`valid?` and `stats` piggyback); minify output reparses to the identical value, minify is idempotent, pretty output reparses. |
| `lines` | `each_line` framing agrees with one `parse` per `\n`-split line, including the yielded prefix before an error; `generate_lines` round-trips what was yielded. |
| `patch` | Input is `document \0 spec`. An object spec drives `splice`, an array spec RFC 6902 `patch`; results and acceptance are compared against a tree-editing reference implementation. |

Run locally (nightly toolchain plus `cargo install cargo-fuzz`; on
macOS the extra link flag lets the extension's cdylib artifact accept
the instrumentation symbols that only exist inside the fuzz binary —
Linux does not need it):

```sh
cd ext/nosj
RUSTFLAGS="-Clink-arg=-Wl,-undefined,dynamic_lookup" \
  cargo +nightly fuzz run reformat fuzz/corpus/reformat fuzz/seeds/reformat \
  -s none -- -max_total_time=300
```

The fuzz binaries embed a Ruby VM and therefore link `libruby` (the
one place that is correct; the extension itself never does). On Linux
the loader resolves that soname at run time, so point it at your
Ruby's lib dir first — macOS needs nothing, the absolute install name
is recorded at link time:

```sh
export LD_LIBRARY_PATH="$(ruby -e 'print RbConfig::CONFIG["libdir"]')"
```

`seeds/` is the committed starting corpus; `corpus/` collects
discoveries and stays untracked. Sanitizers stay off (`-s none`):
Ruby's conservative GC stack scanning and ASan disagree, and the
parser crate's fuzzers keep the sanitizer coverage. CI runs the three
targets weekly (`.github/workflows/fuzz.yml`), 15 minutes each by
default.

Deliberately not asserted: on documents whole-document parsing
refuses, splice/patch may still succeed (resolution scans only the
bytes some pointer needs — documented crate semantics), and the pipe
may refuse with `GeneratorError` (lone-surrogate key, non-finite
float) before the parser reaches a later syntax error.

First finds: minify emitted a bare `Infinity` literal (unparseable
output) for huge-exponent floats like `1e999` that parse to Infinity
even in strict mode; fixed to raise the gem-exact `GeneratorError`,
pinned in `spec/reformat_spec.rb`.

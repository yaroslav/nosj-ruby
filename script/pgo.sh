#!/bin/sh
# Profile-guided-optimization build of the nosj gem extension.
#
# Measured effect (Apple Silicon, Ruby 4.0 + YJIT, vs json gem 2.20, mixed-
# workload harness): twitter 0.90 -> 0.81, mesh 1.00 -> 0.71, numbers 0.89
# -> 0.71, canada 0.98 -> 0.89, tolstoy 0.64 -> 0.59, citm 0.91 -> 0.86.
#
# Usage: script/pgo.sh
set -e

# Absolute path: the extension build invokes cargo from a subdirectory.
PGO_DIR="$(pwd)/${PGO_DIR:-tmp/pgo}"
# Dev boxes optimize for the local CPU. Distributed release binaries must
# stay portable (baseline ISA; the crate runtime-detects AVX2), so CI sets
# PGO_BASE_RUSTFLAGS="" (or other flags) to override the default.
BASE_RUSTFLAGS="${PGO_BASE_RUSTFLAGS--C target-cpu=native}"
# llvm-profdata ships in the rustup `llvm-tools` component of the ACTIVE
# toolchain; resolve via sysroot (a $HOME glob once matched a toolchain
# without the component and silently produced an empty path, leaving the
# slow instrumented build installed). The trailing * matches the .exe
# suffix on Windows.
LLVM_PROFDATA="$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-profdata* 2>/dev/null | head -1)"
if [ ! -x "$LLVM_PROFDATA" ]; then
    echo "error: llvm-profdata not found — run: rustup component add llvm-tools" >&2
    exit 1
fi

rm -rf "$PGO_DIR"
mkdir -p "$PGO_DIR"

echo "== 1/3 instrumented build"
RUSTFLAGS="$BASE_RUSTFLAGS -C profile-generate=$PGO_DIR" bundle exec rake compile

echo "== 2/3 training workload"
LLVM_PROFILE_FILE="$PGO_DIR/nosj-%p.profraw" ruby -e '
  $LOAD_PATH.unshift File.expand_path("lib")
  require "nosj"
  %w[twitter tolstoy citm_catalog canada mesh numbers].each do |name|
    path = "benchmark/#{name}.json"
    next unless File.exist?(path)
    data = File.read(path)
    n = name == "canada" ? 60 : 300
    n.times { NOSJ.parse(data) }
    obj = NOSJ.parse(data)
    n.times { NOSJ.generate(obj) }
    (n / 4).times { NOSJ.pretty_generate(obj) }
  end
'

echo "== 3/3 optimized build"
"$LLVM_PROFDATA" merge -o "$PGO_DIR/merged.profdata" "$PGO_DIR"/*.profraw
RUSTFLAGS="$BASE_RUSTFLAGS -C profile-use=$PGO_DIR/merged.profdata" bundle exec rake compile

echo "PGO build installed."

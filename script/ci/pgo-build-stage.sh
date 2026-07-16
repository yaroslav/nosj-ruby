#!/bin/sh
# One PGO-optimized native extension for the ACTIVE Ruby, staged for the
# platform-gem assembly (rake "gem:native[<platform>]").
#
# Runs the full PGO cycle (instrument, train on the benchmark corpus,
# rebuild) with PORTABLE codegen: no target-cpu=native, so the shipped
# binary runs on any CPU of the target arch (the crate runtime-detects
# AVX2 on x86-64). Stages the result under tmp/native-gem/<major.minor>/.
#
# SKIP_PGO=1 builds a plain portable release instead: for targets whose
# Rust distribution has no profiler runtime (x86_64-pc-windows-gnu),
# where -C profile-generate cannot compile at all. Every other platform
# gem is PGO.
#
# Usage: script/ci/pgo-build-stage.sh   (from the repo root)
set -e

ruby_minor="$(ruby -e 'print RUBY_VERSION[/\d+\.\d+/]')"
dlext="$(ruby -e 'print RbConfig::CONFIG["DLEXT"]')"

# A stale profile would otherwise be auto-applied by rake compile.
rm -rf tmp/pgo

if [ "${SKIP_PGO:-}" = "1" ]; then
  echo "== plain build for Ruby ${ruby_minor} (no profiler runtime on this target)"
  RUSTFLAGS="${PGO_BASE_RUSTFLAGS-}" bundle exec rake compile
else
  echo "== PGO build for Ruby ${ruby_minor} (portable codegen)"
  # Default to portable codegen (no target-cpu=native), but let the caller
  # inject platform-required flags (musl needs -crt-static off, see
  # alpine-pgo-build.sh).
  PGO_BASE_RUSTFLAGS="${PGO_BASE_RUSTFLAGS-}" ./script/pgo.sh
fi

stage="tmp/native-gem/${ruby_minor}"
mkdir -p "$stage"
cp "lib/nosj/nosj.${dlext}" "$stage/nosj.${dlext}"
echo "staged ${stage}/nosj.${dlext}"

# The staged copy is the artifact; drop the live one so the next Ruby's
# build starts clean and the assembly step never picks a stale binary.
rm -f "lib/nosj/nosj.${dlext}"

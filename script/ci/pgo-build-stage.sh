#!/bin/sh
# One PGO-optimized native extension for the ACTIVE Ruby, staged for the
# platform-gem assembly (rake "gem:native[<platform>]").
#
# Runs the full PGO cycle (instrument, train on the benchmark corpus,
# rebuild) with PORTABLE codegen: no target-cpu=native, so the shipped
# binary runs on any CPU of the target arch (the crate runtime-detects
# AVX2 on x86-64). Stages the result under tmp/native-gem/<major.minor>/.
#
# Usage: script/ci/pgo-build-stage.sh   (from the repo root)
set -e

ruby_minor="$(ruby -e 'print RUBY_VERSION[/\d+\.\d+/]')"
dlext="$(ruby -e 'print RbConfig::CONFIG["DLEXT"]')"

echo "== PGO build for Ruby ${ruby_minor} (portable codegen)"
rm -rf tmp/pgo
# Default to portable codegen (no target-cpu=native), but let the caller
# inject platform-required flags (musl needs -crt-static off, see
# alpine-pgo-build.sh).
PGO_BASE_RUSTFLAGS="${PGO_BASE_RUSTFLAGS-}" ./script/pgo.sh

stage="tmp/native-gem/${ruby_minor}"
mkdir -p "$stage"
cp "lib/nosj/nosj.${dlext}" "$stage/nosj.${dlext}"
echo "staged ${stage}/nosj.${dlext}"

# The staged copy is the artifact; drop the live one so the next Ruby's
# build starts clean and the assembly step never picks a stale binary.
rm -f "lib/nosj/nosj.${dlext}"

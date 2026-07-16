#!/bin/sh
# musl builds run inside ruby:<version>-alpine containers on a runner of
# the MATCHING architecture, so the PGO training workload executes at
# native speed and the profile reflects real musl binaries. Installs the
# toolchain, then defers to pgo-build-stage.sh.
#
# Usage (inside the container, repo mounted at /work):
#   sh script/ci/alpine-pgo-build.sh
set -e

apk add --no-cache build-base curl bash git linux-headers clang

# rustup: the apk rust is often behind the crate's MSRV; llvm-tools
# provides llvm-profdata for the profile merge.
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
    sh -s -- -y --profile minimal --component llvm-tools
fi
. "$HOME/.cargo/env"

cd /work
# Env-only bundler path: a --local config file would persist root-owned
# state into the mounted checkout and poison later host steps.
export BUNDLE_PATH=vendor/bundle-alpine
bundle install --quiet
./script/ci/pgo-build-stage.sh

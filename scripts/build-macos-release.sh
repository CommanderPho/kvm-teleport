#!/usr/bin/env bash
# Build the release Mousehop.app (icon → cargo-bundle → vendor dylibs).
# Run from the repo root. Matches Homebrew libs to the active rustc host
# so x86_64 toolchains do not link against arm64 /opt/homebrew dylibs.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

rustc_host="$(rustc -vV | awk '/^host:/{print $2}')"
case "$rustc_host" in
  aarch64-apple-darwin)
    brew_prefix="${HOMEBREW_PREFIX:-/opt/homebrew}"
    ;;
  x86_64-apple-darwin)
    brew_prefix="${HOMEBREW_PREFIX:-/usr/local}"
    ;;
  *)
    echo "unsupported rustc host: $rustc_host" >&2
    exit 1
    ;;
esac

export LIBRARY_PATH="${brew_prefix}/lib${LIBRARY_PATH:+:$LIBRARY_PATH}"
export PKG_CONFIG_PATH="${brew_prefix}/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export PATH="${brew_prefix}/bin:${PATH}"

echo "==> rustc host: $rustc_host (Homebrew: $brew_prefix)"
echo "==> generating icons"
scripts/makeicns.sh

echo "==> cargo bundle --release (from mousehop/)"
(cd mousehop && cargo bundle --release)

app="target/release/bundle/osx/Mousehop.app"
binary="$app/Contents/MacOS/mousehop"
echo "==> vendoring dylibs into $app"
scripts/copy-macos-dylib.sh "$binary"

echo "==> done: $root/$app"

#!/usr/bin/env bash
# Build the release Mousehop.app (icon → cargo-bundle → vendor dylibs).
# Run from the repo root. Matches Homebrew libs to the active rustc host
# so x86_64 toolchains do not link against arm64 /opt/homebrew dylibs.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

# Prefer rustup over Homebrew rustc. Login shells often put /usr/local/bin
# first, which can select an x86_64 Homebrew rustc on Apple Silicon and then
# fail to link against arm64 GTK / libadwaita from /opt/homebrew.
cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
export PATH="${cargo_bin}:${PATH}"

# Prefer the Homebrew tree that matches rustc's arch. Do not use
# HOMEBREW_PREFIX here: on Apple Silicon it is often /opt/homebrew even when
# an x86_64 (Rosetta) rustc needs /usr/local, which breaks linking (arm64
# libgit2 vs x86_64).
rustc_host="$(rustc -vV | awk '/^host:/{print $2}')"
case "$rustc_host" in
  aarch64-apple-darwin)
    brew_prefix="/opt/homebrew"
    ;;
  x86_64-apple-darwin)
    brew_prefix="/usr/local"
    ;;
  *)
    echo "unsupported rustc host: $rustc_host" >&2
    exit 1
    ;;
esac
if [[ ! -d "$brew_prefix/lib" ]]; then
  echo "missing Homebrew lib dir for $rustc_host: $brew_prefix/lib" >&2
  exit 1
fi

export LIBRARY_PATH="${brew_prefix}/lib"
export PKG_CONFIG_PATH="${brew_prefix}/lib/pkgconfig"
# Homebrew tools after rustup so `brew`/`pkg-config` resolve, but not rustc.
export PATH="${cargo_bin}:${brew_prefix}/bin:${PATH}"

echo "==> rustc host: $rustc_host (Homebrew: $brew_prefix)"
echo "==> rustc: $(command -v rustc)"
echo "==> generating icons"
scripts/makeicns.sh

echo "==> cargo bundle --release (from mousehop/)"
(cd mousehop && cargo bundle --release)

app="target/release/bundle/osx/Mousehop.app"
binary="$app/Contents/MacOS/mousehop"
echo "==> vendoring dylibs into $app"
scripts/copy-macos-dylib.sh "$binary"

dev_app="MousehopDev.app"
echo "==> copying to $root/$dev_app"
rm -rf "$dev_app"
cp -R "$app" "$dev_app"

echo "==> done: $root/$app"
echo "==> done: $root/$dev_app"

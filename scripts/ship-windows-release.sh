#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="x86_64-pc-windows-gnu"
BINARY="ashe-dictate-rs.exe"
RELEASE_DIR="${RELEASE_DIR:-/root/Desktop/releases}"

if [[ -n "${CARGO:-}" ]]; then
  CARGO_BIN="$CARGO"
elif command -v cargo >/dev/null 2>&1; then
  CARGO_BIN="$(command -v cargo)"
elif [[ -x /root/.cargo/bin/cargo ]]; then
  CARGO_BIN="/root/.cargo/bin/cargo"
else
  echo "cargo was not found" >&2
  exit 1
fi

if ! command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
  echo "x86_64-w64-mingw32-gcc was not found" >&2
  exit 1
fi

cd "$PROJECT_ROOT"
PACKAGE_VERSION="$(awk -F '"' '/^version = / { print $2; exit }' "$PROJECT_ROOT/Cargo.toml")"
GIT_SHA="$(git -C "$PROJECT_ROOT" rev-parse --short HEAD 2>/dev/null || echo nogit)"
if git -C "$PROJECT_ROOT" diff --quiet --ignore-submodules HEAD -- 2>/dev/null; then
  DIRTY=""
else
  DIRTY="-dirty"
fi
BUILD_ID="${ASHE_BUILD_ID:-v${PACKAGE_VERSION}+$(date -u +%Y%m%d%H%M%S)-${GIT_SHA}${DIRTY}}"
ASHE_BUILD_ID="$BUILD_ID" "$CARGO_BIN" build --release --target "$TARGET"
mkdir -p "$RELEASE_DIR"
cp "$PROJECT_ROOT/target/$TARGET/release/$BINARY" "$RELEASE_DIR/$BINARY"
printf '%s\n' "$BUILD_ID" > "$RELEASE_DIR/ashe-dictate-rs.build.txt"
printf 'Shipped %s build_id=%s\n' "$RELEASE_DIR/$BINARY" "$BUILD_ID"

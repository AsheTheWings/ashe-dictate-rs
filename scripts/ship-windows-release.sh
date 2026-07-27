#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="x86_64-pc-windows-gnu"
BINARY="ashe-worker.exe"
DECRYPT_BINARY="ashe-archive-decrypt.exe"
ENV_FILE="$PROJECT_ROOT/.env.local"

read_project_env() {
  local wanted="$1"
  local destination="$2"
  local line name value
  while IFS= read -r line || [[ -n "$line" ]]; do
    line="${line%$'\r'}"
    [[ "$line" =~ ^[[:space:]]*# ]] && continue
    [[ "$line" == *"="* ]] || continue
    name="${line%%=*}"
    name="${name#"${name%%[![:space:]]*}"}"
    name="${name%"${name##*[![:space:]]}"}"
    [[ "$name" == "$wanted" ]] || continue
    value="${line#*=}"
    value="${value#"${value%%[![:space:]]*}"}"
    value="${value%"${value##*[![:space:]]}"}"
    if [[ "$value" == \"*\" && "$value" == *\" ]]; then
      value="${value:1:${#value}-2}"
    elif [[ "$value" == \'*\' && "$value" == *\' ]]; then
      value="${value:1:${#value}-2}"
    fi
    printf -v "$destination" '%s' "$value"
    return 0
  done < "$ENV_FILE"
  return 1
}

if [[ ! -f "$ENV_FILE" ]]; then
  echo ".env.local is required for release configuration" >&2
  exit 1
fi
RELEASE_DIR=""
RECIPIENT_FILE=""
read_project_env ASHE_RELEASE_DIR RELEASE_DIR || true
read_project_env ASHE_ARCHIVE_RECIPIENT_FILE RECIPIENT_FILE || true
if [[ -z "$RELEASE_DIR" ]]; then
  echo "ASHE_RELEASE_DIR is required in .env.local; release aborted" >&2
  exit 1
fi
if [[ -z "$RECIPIENT_FILE" ]]; then
  echo "ASHE_ARCHIVE_RECIPIENT_FILE is required in .env.local; release aborted" >&2
  exit 1
fi

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
if [[ ! -f "$RECIPIENT_FILE" ]]; then
  echo "archive recipient was not found at $RECIPIENT_FILE" >&2
  exit 1
fi
ASHE_BUILD_ID="$BUILD_ID" ASHE_ARCHIVE_RECIPIENT_FILE="$RECIPIENT_FILE" \
  "$CARGO_BIN" build --release --target "$TARGET"
"$CARGO_BIN" build --release --target "$TARGET" \
  --manifest-path "$PROJECT_ROOT/archive-crypto/Cargo.toml" \
  --bin ashe-archive-decrypt
mkdir -p "$RELEASE_DIR"
cp "$PROJECT_ROOT/target/$TARGET/release/$BINARY" "$RELEASE_DIR/$BINARY"
cp "$PROJECT_ROOT/archive-crypto/target/$TARGET/release/$DECRYPT_BINARY" \
  "$RELEASE_DIR/$DECRYPT_BINARY"
cp "$PROJECT_ROOT/.env.example" "$RELEASE_DIR/.env.example"
printf '%s\n' "$BUILD_ID" > "$RELEASE_DIR/ashe-worker.build.txt"
printf 'Shipped %s and %s build_id=%s\n' \
  "$RELEASE_DIR/$BINARY" "$RELEASE_DIR/$DECRYPT_BINARY" "$BUILD_ID"

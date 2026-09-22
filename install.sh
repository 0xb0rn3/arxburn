#!/bin/sh
# install.sh - build (if needed) and install arxburn into /usr/bin.
# Run as root. This is also what `arx tools` invokes after unpacking a published archive.
set -eu

D=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BIN=arxburn
DEST=${DEST:-/usr/bin}

if [ "$(id -u)" -ne 0 ]; then
  echo "install.sh needs root: sudo $0" >&2
  exit 1
fi

# A published archive ships the built binary beside this script; a source checkout does not.
if [ -f "$D/$BIN" ]; then
  src="$D/$BIN"
elif [ -f "$D/target/release/$BIN" ]; then
  src="$D/target/release/$BIN"
elif command -v cargo >/dev/null 2>&1; then
  echo ">> building $BIN"
  ( cd "$D" && cargo build --release --offline 2>/dev/null || cargo build --release )
  src="$D/target/release/$BIN"
else
  echo "no $BIN binary here and no cargo to build one" >&2
  exit 1
fi

[ -f "$src" ] || { echo "build produced no binary at $src" >&2; exit 1; }

install -Dm755 "$src" "$DEST/$BIN"
"$DEST/$BIN" --version >/dev/null || { echo "installed binary does not run" >&2; exit 1; }
echo "  ok $DEST/$BIN ($("$DEST/$BIN" --version))"

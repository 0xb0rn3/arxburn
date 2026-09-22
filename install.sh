#!/bin/sh
# install.sh - build (if needed) and install arxburn into /usr/bin.
# Run as root. This is also what `arx tools` invokes after unpacking a published archive.
#
#   sudo ./install.sh            the CLI, and the GUI too when its libraries are present
#   sudo ./install.sh --no-gui   the CLI only
#   sudo ./install.sh --gui      insist on the GUI, and fail loudly if it cannot be built
set -eu

D=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BIN=arxburn
GUI=arxburn-gui
DEST=${DEST:-/usr/bin}
WANT_GUI=auto

for arg in "$@"; do
  case "$arg" in
    --no-gui) WANT_GUI=no ;;
    --gui) WANT_GUI=yes ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

if [ "$(id -u)" -ne 0 ]; then
  echo "install.sh needs root: sudo $0" >&2
  exit 1
fi

say() { printf '  %s\n' "$*"; }
step() { printf '>> %s\n' "$*"; }

# Building as root is the wrong thing to do even when it works: rustup is per user, so under
# sudo there is usually no default toolchain and cargo refuses, and anything it did build would
# leave root-owned files in the user's checkout. Build as whoever called sudo, install as root.
as_user() {
  if [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != root ]; then
    sudo -u "$SUDO_USER" -- sh -c "$1"
  else
    sh -c "$1"
  fi
}

have_cargo() {
  as_user "command -v cargo >/dev/null 2>&1 && cargo --version >/dev/null 2>&1"
}

cargo_advice() {
  cat >&2 <<'EOF'
  cargo will not run for the user calling sudo.

  If this machine uses rustup, that user has no default toolchain set. Fix it with:

      rustup default stable

  then run this script again. Or build it yourself first and re-run:

      cargo build --release
      sudo ./install.sh
EOF
}

# ---- the CLI ---------------------------------------------------------------------------------
if [ -f "$D/$BIN" ]; then
  src="$D/$BIN"                       # a published archive ships the built binary beside this
elif [ -f "$D/target/release/$BIN" ]; then
  src="$D/target/release/$BIN"
elif have_cargo; then
  step "building $BIN"
  as_user "cd '$D' && cargo build --release --offline 2>/dev/null || cd '$D' && cargo build --release"
  src="$D/target/release/$BIN"
else
  cargo_advice
  exit 1
fi

[ -f "$src" ] || { echo "build produced no binary at $src" >&2; exit 1; }
install -Dm755 "$src" "$DEST/$BIN"
"$DEST/$BIN" --version >/dev/null || { echo "installed binary does not run" >&2; exit 1; }
say "ok $DEST/$BIN ($("$DEST/$BIN" --version))"

# ---- the GUI ---------------------------------------------------------------------------------
# A separate crate with real dependencies, so it is built only when it can be, and never at the
# cost of the CLI. `cargo build --release` alone does NOT build it: this workspace's root is
# itself a package, so a bare build builds only that.
[ "$WANT_GUI" = no ] && exit 0

gui_libs_ok() {
  pkg-config --exists webkit2gtk-4.1 2>/dev/null
}

if [ -f "$D/$GUI" ]; then
  gsrc="$D/$GUI"
elif [ -f "$D/target/release/$GUI" ]; then
  gsrc="$D/target/release/$GUI"
elif gui_libs_ok && have_cargo; then
  step "building $GUI"
  as_user "cd '$D' && cargo build --release -p $GUI"
  gsrc="$D/target/release/$GUI"
else
  gsrc=""
fi

if [ -n "$gsrc" ] && [ -f "$gsrc" ]; then
  install -Dm755 "$gsrc" "$DEST/$GUI"
  if [ -f "$D/src-tauri/icons/256x256.png" ]; then
    install -Dm644 "$D/src-tauri/icons/256x256.png" /usr/share/icons/hicolor/256x256/apps/arxburn.png
  fi
  cat > /usr/share/applications/arxburn.desktop <<EOF
[Desktop Entry]
Type=Application
Name=arxburn
Comment=Write an image to a USB stick, and prove it landed
Exec=$DEST/$GUI
Icon=arxburn
Terminal=false
Categories=System;Utility;
EOF
  say "ok $DEST/$GUI (and a menu entry)"
else
  say "GUI not installed."
  if ! gui_libs_ok; then
    cat <<'EOF'
     It needs webkit2gtk-4.1 and gtk3. On Arch or EndeavourOS:

         sudo pacman -S --needed webkit2gtk-4.1 gtk3 libsoup3

     then run this script again. The command line tool above works without any of that.
EOF
  fi
fi

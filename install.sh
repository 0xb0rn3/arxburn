#!/bin/sh
# arxburn installer. Works two ways, and will ask which you want:
#
#   curl -fsSL https://raw.githubusercontent.com/0xb0rn3/arxburn/main/install.sh | sudo sh
#   sudo ./install.sh                    (from a checkout)
#
# Options, for when you already know:
#   --binary    download the published binaries and verify them against SHA256SUMS
#   --source    build from source (needs cargo; the GUI also needs webkit2gtk-4.1)
#   --no-gui    command line tool only
#   --gui       insist on the window, fail loudly if it cannot be installed
#   --yes       no questions, take the defaults
set -eu

REPO=0xb0rn3/arxburn
BIN=arxburn
GUI=arxburn-gui
DEST=${DEST:-/usr/bin}
MODE=ask
WANT_GUI=auto
ASSUME_YES=no

for arg in "$@"; do
  case "$arg" in
    --binary|--prebuilt) MODE=binary ;;
    --source|--build) MODE=source ;;
    --no-gui) WANT_GUI=no ;;
    --gui) WANT_GUI=yes ;;
    --yes|-y) ASSUME_YES=yes ;;
    -h|--help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

say()  { printf '   %s\n' "$*"; }
ok()   { printf '   \033[32mok\033[0m %s\n' "$*"; }
warn() { printf '   \033[33m!!\033[0m %s\n' "$*"; }
step() { printf '\033[1m>>\033[0m %s\n' "$*"; }
die()  { printf '   \033[31m!!\033[0m %s\n' "$*" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || die "this needs root: sudo $0 (or pipe it to sudo sh)"

# Where are we? A checkout can build from source; a piped script cannot.
D=""
case "$0" in
  -|sh|/dev/fd/*|/proc/self/fd/*) : ;;
  *) D=$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd || true) ;;
esac
[ -n "$D" ] && [ -f "$D/Cargo.toml" ] && HAVE_SOURCE=yes || HAVE_SOURCE=no

# rustup toolchains are per user: under sudo there is usually no default toolchain and cargo
# refuses outright, and anything root built would leave root-owned files in the checkout. So
# every build runs as whoever called sudo, and only the install runs as root.
as_user() {
  if [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != root ]; then
    sudo -u "$SUDO_USER" -- sh -c "$1"
  else
    sh -c "$1"
  fi
}
have_cargo() { as_user "command -v cargo >/dev/null 2>&1 && cargo --version >/dev/null 2>&1"; }
have_webkit() { pkg-config --exists webkit2gtk-4.1 2>/dev/null; }
fetch() { if command -v curl >/dev/null 2>&1; then curl -fsSL "$1" -o "$2"; else wget -qO "$2" "$1"; fi; }

# ---- which way ------------------------------------------------------------------------------
if [ "$MODE" = ask ]; then
  if [ "$ASSUME_YES" = yes ] || [ ! -t 0 ]; then
    # piped or told not to ask: prebuilt is the one that works everywhere
    MODE=binary
  else
    echo
    echo "  How would you like arxburn installed?"
    echo
    echo "    1) Published binaries   quick, verified against their published sha256"
    echo "    2) Build from source    needs cargo; the window also needs webkit2gtk-4.1"
    echo
    printf "  [1/2, default 1]: "
    read -r answer </dev/tty || answer=1
    case "$answer" in
      2) MODE=source ;;
      *) MODE=binary ;;
    esac
    echo
  fi
fi

[ "$MODE" = source ] && [ "$HAVE_SOURCE" = no ] && {
  warn "there is no checkout here to build from, falling back to the published binaries"
  MODE=binary
}

install_one() {  # path, name
  install -Dm755 "$1" "$DEST/$2"
}

desktop_entry() {
  [ -f "$1" ] && install -Dm644 "$1" /usr/share/icons/hicolor/256x256/apps/arxburn.png
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
}

# ---- published binaries ---------------------------------------------------------------------
if [ "$MODE" = binary ]; then
  step "fetching the published binaries"
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  base="https://github.com/$REPO/releases/latest/download"

  fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "cannot reach the release. Try --source."
  fetch "$base/$BIN-x86_64-linux" "$tmp/$BIN" || die "cannot download $BIN"
  if [ "$WANT_GUI" != no ]; then
    fetch "$base/$GUI-x86_64-linux" "$tmp/$GUI" 2>/dev/null || warn "no published window binary"
  fi

  # verify every file we actually downloaded, and refuse anything that does not match
  ( cd "$tmp" && for f in $BIN $GUI; do
      [ -f "$f" ] || continue
      want=$(grep -E "  $f-x86_64-linux\$" SHA256SUMS | awk '{print $1}')
      [ -n "$want" ] || { echo "no published hash for $f" >&2; exit 1; }
      got=$(sha256sum "$f" | awk '{print $1}')
      [ "$want" = "$got" ] || { echo "$f does NOT match its published sha256" >&2; exit 1; }
    done ) || die "download failed its integrity check; nothing was installed"
  ok "sha256 verified against SHA256SUMS"

  install_one "$tmp/$BIN" "$BIN"
  ok "$DEST/$BIN ($("$DEST/$BIN" --version))"

  if [ -f "$tmp/$GUI" ] && [ "$WANT_GUI" != no ]; then
    if have_webkit; then
      install_one "$tmp/$GUI" "$GUI"
      desktop_entry ""
      ok "$DEST/$GUI (and a menu entry)"
    else
      warn "the window needs webkit2gtk-4.1, which is not installed"
      say "sudo pacman -S --needed webkit2gtk-4.1 gtk3 libsoup3   # Arch, EndeavourOS, ArxOS"
      say "then run this again. The command line tool above already works."
    fi
  fi
  exit 0
fi

# ---- from source ----------------------------------------------------------------------------
step "building from source"
have_cargo || die "cargo will not run for the user calling sudo.
     If this machine uses rustup, that user has no default toolchain:
         rustup default stable
     then run this again, or use --binary for the published build."

as_user "cd '$D' && cargo build --release --offline 2>/dev/null || cd '$D' && cargo build --release"
[ -f "$D/target/release/$BIN" ] || die "the build produced no $BIN"
install_one "$D/target/release/$BIN" "$BIN"
ok "$DEST/$BIN ($("$DEST/$BIN" --version))"

[ "$WANT_GUI" = no ] && exit 0

# The window is a separate crate: a bare `cargo build --release` builds only the root package,
# so it needs -p, and it needs a system webkit.
if have_webkit; then
  step "building the window"
  as_user "cd '$D' && cargo build --release -p $GUI"
  if [ -f "$D/target/release/$GUI" ]; then
    install_one "$D/target/release/$GUI" "$GUI"
    desktop_entry "$D/src-tauri/icons/256x256.png"
    ok "$DEST/$GUI (and a menu entry)"
  else
    [ "$WANT_GUI" = yes ] && die "the window failed to build"
    warn "the window did not build; the command line tool is installed"
  fi
else
  [ "$WANT_GUI" = yes ] && die "the window needs webkit2gtk-4.1: sudo pacman -S --needed webkit2gtk-4.1 gtk3 libsoup3"
  warn "skipping the window: webkit2gtk-4.1 is not installed"
  say "sudo pacman -S --needed webkit2gtk-4.1 gtk3 libsoup3, then run this again"
fi

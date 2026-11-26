#!/bin/sh
# Build BDView+ and install it for the current user (no root needed).
#   packaging/install.sh              build + install to ~/.local
#   packaging/install.sh --prefix DIR install under DIR instead
#   packaging/install.sh uninstall    remove what was installed
set -e
cd "$(dirname "$0")/.."
PREFIX="$HOME/.local"
ACTION=install
while [ $# -gt 0 ]; do
    case "$1" in
        --prefix) PREFIX="$2"; shift ;;
        uninstall) ACTION=uninstall ;;
        *) echo "usage: $0 [--prefix DIR] [uninstall]" >&2; exit 1 ;;
    esac
    shift
done
BIN="$PREFIX/bin/bdview-plus"
APP="$PREFIX/share/applications/bdview-plus.desktop"
ICONS="$PREFIX/share/icons/hicolor"
SIZES="16 32 48 64 128 256 512"
if [ "$ACTION" = uninstall ]; then
    rm -f "$BIN" "$APP"
    for s in $SIZES; do rm -f "$ICONS/${s}x${s}/apps/bdview-plus.png"; done
    echo "removed BDView+ from $PREFIX"
    exit 0
fi
cargo build --release
mkdir -p "$PREFIX/bin" "$PREFIX/share/applications"
install -m 755 target/release/bdview-plus "$BIN"
for s in $SIZES; do
    mkdir -p "$ICONS/${s}x${s}/apps"
    install -m 644 "packaging/icons/bdview-plus-$s.png" "$ICONS/${s}x${s}/apps/bdview-plus.png"
done
sed "s|@BIN@|$BIN|" packaging/bdview-plus.desktop > "$APP"
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$ICONS" 2>/dev/null || true
echo "installed $BIN with icons and menu entry"

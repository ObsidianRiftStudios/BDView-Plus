#!/bin/sh
# Build TuxInfo and install it for the current user (no root needed).
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
BIN="$PREFIX/bin/tuxinfo"
APP="$PREFIX/share/applications/tuxinfo.desktop"
ICONS="$PREFIX/share/icons/hicolor"
SIZES="16 32 48 64 128 256 512"
if [ "$ACTION" = uninstall ]; then
    rm -f "$BIN" "$APP"
    for s in $SIZES; do rm -f "$ICONS/${s}x${s}/apps/tuxinfo.png"; done
    echo "removed TuxInfo from $PREFIX"
    exit 0
fi
cargo build --release
mkdir -p "$PREFIX/bin" "$PREFIX/share/applications"
install -m 755 target/release/tuxinfo "$BIN"
ln -sf tuxinfo   # the old command name keeps working
for s in $SIZES; do
    mkdir -p "$ICONS/${s}x${s}/apps"
    install -m 644 "packaging/icons/tuxinfo-$s.png" "$ICONS/${s}x${s}/apps/tuxinfo.png"
done
sed "s|@BIN@|$BIN|" packaging/tuxinfo.desktop > "$APP"
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$ICONS" 2>/dev/null || true
echo "installed $BIN with icons and menu entry"

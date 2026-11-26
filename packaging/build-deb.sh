#!/bin/sh
# Build a .deb package of BDView+ (needs only cargo and dpkg-deb).
#   packaging/build-deb.sh          -> target/bdview-plus_<version>_<arch>.deb
# Install with:  sudo apt install ./target/bdview-plus_*.deb
set -e
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n1)
ARCH=$(dpkg --print-architecture)
cargo build --release
ROOT=target/deb/bdview-plus_${VERSION}_${ARCH}
rm -rf "$ROOT"
mkdir -p "$ROOT/DEBIAN" "$ROOT/usr/bin" "$ROOT/usr/share/applications" "$ROOT/usr/share/doc/bdview-plus"
install -m 755 target/release/bdview-plus "$ROOT/usr/bin/bdview-plus"
for s in 16 32 48 64 128 256 512; do
    mkdir -p "$ROOT/usr/share/icons/hicolor/${s}x${s}/apps"
    install -m 644 "packaging/icons/bdview-plus-$s.png" "$ROOT/usr/share/icons/hicolor/${s}x${s}/apps/bdview-plus.png"
done
sed "s|@BIN@|bdview-plus|" packaging/bdview-plus.desktop > "$ROOT/usr/share/applications/bdview-plus.desktop"
chmod 644 "$ROOT/usr/share/applications/bdview-plus.desktop"
install -m 644 LICENSE "$ROOT/usr/share/doc/bdview-plus/copyright"
install -m 644 README.md "$ROOT/usr/share/doc/bdview-plus/README.md"
SIZE=$(du -sk "$ROOT" | cut -f1)
cat > "$ROOT/DEBIAN/control" <<CTL
Package: bdview-plus
Version: $VERSION
Section: video
Priority: optional
Architecture: $ARCH
Installed-Size: $SIZE
Maintainer: ${DEB_MAINTAINER:-Obsidian Rift Studios <support@obsidianrift.net>}
Depends: libc6, libgcc-s1, libgl1, libegl1, libxkbcommon0, libx11-6, libxcursor1, libxi6, libxrandr2, libwayland-client0
Recommends: libmpv2, libbluray2, xdg-desktop-portal
Conflicts: tuxinfo
Replaces: tuxinfo
Description: Blu-ray and Ultra HD disc analyzer
 BDView+ shows the playlists, streams and measured bit rates of a Blu-ray or
 UHD disc (drive, folder or ISO image), with bit rate charts, a report, and a
 video preview. Based on BDInfo.
 .
 Disc decryption and the preview use libbluray and libmpv when they are
 installed (libmmbd from MakeMKV can be used for protected discs).
CTL
cat > "$ROOT/DEBIAN/postinst" <<'POST'
#!/bin/sh
set -e
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t /usr/share/icons/hicolor 2>/dev/null || true
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q 2>/dev/null || true
exit 0
POST
chmod 755 "$ROOT/DEBIAN/postinst"
dpkg-deb --root-owner-group --build "$ROOT" "target/bdview-plus_${VERSION}_${ARCH}.deb"
echo "built target/bdview-plus_${VERSION}_${ARCH}.deb"

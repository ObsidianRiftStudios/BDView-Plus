#!/bin/sh
# Build a .deb package of TuxInfo (needs only cargo and dpkg-deb).
#   packaging/build-deb.sh          -> target/tuxinfo_<version>_<arch>.deb
# Install with:  sudo apt install ./target/tuxinfo_*.deb
set -e
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n1)
ARCH=$(dpkg --print-architecture)
cargo build --release
ROOT=target/deb/tuxinfo_${VERSION}_${ARCH}
rm -rf "$ROOT"
mkdir -p "$ROOT/DEBIAN" "$ROOT/usr/bin" "$ROOT/usr/share/applications" "$ROOT/usr/share/doc/tuxinfo"
install -m 755 target/release/tuxinfo "$ROOT/usr/bin/tuxinfo"
for s in 16 32 48 64 128 256 512; do
    mkdir -p "$ROOT/usr/share/icons/hicolor/${s}x${s}/apps"
    install -m 644 "packaging/icons/tuxinfo-$s.png" "$ROOT/usr/share/icons/hicolor/${s}x${s}/apps/tuxinfo.png"
done
sed "s|@BIN@|tuxinfo|" packaging/tuxinfo.desktop > "$ROOT/usr/share/applications/tuxinfo.desktop"
chmod 644 "$ROOT/usr/share/applications/tuxinfo.desktop"
install -m 644 LICENSE "$ROOT/usr/share/doc/tuxinfo/copyright"
install -m 644 README.md "$ROOT/usr/share/doc/tuxinfo/README.md"
SIZE=$(du -sk "$ROOT" | cut -f1)
cat > "$ROOT/DEBIAN/control" <<CTL
Package: tuxinfo
Version: $VERSION
Section: video
Priority: optional
Architecture: $ARCH
Installed-Size: $SIZE
Maintainer: ${DEB_MAINTAINER:-TuxInfo <tuxinfo@localhost>}
Depends: libc6, libgcc-s1, libgl1, libegl1, libxkbcommon0, libx11-6, libxcursor1, libxi6, libxrandr2, libwayland-client0
Recommends: libmpv2, libbluray2, xdg-desktop-portal
Description: Blu-ray and Ultra HD disc analyzer
 TuxInfo shows the playlists, streams and measured bit rates of a Blu-ray or
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
dpkg-deb --root-owner-group --build "$ROOT" "target/tuxinfo_${VERSION}_${ARCH}.deb"
echo "built target/tuxinfo_${VERSION}_${ARCH}.deb"

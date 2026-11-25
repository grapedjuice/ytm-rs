#!/bin/sh
# Build the AppImage: one file holding the app, its menu entry and icon. Like the
# tarballs it runs on the system's own libraries (GTK, WebKitGTK, ALSA, …), so it needs
# what they need. Beside the app goes appimage.txt, which has it tell you when a new
# version is out instead of updating itself.
#
#   linux/appimage.sh <binary> <version> <out dir>
#
# Needs appimagetool and the AppImage runtime to put in front, given as APPIMAGETOOL
# and APPIMAGE_RUNTIME (the release workflow downloads both, pinned).
set -e

bin=$1
version=$2
out=$3
[ -f "$bin" ] && [ -n "$version" ] && [ -n "$out" ] || { echo "usage: $0 <binary> <version> <out dir>"; exit 1; }
[ -x "$APPIMAGETOOL" ] && [ -f "$APPIMAGE_RUNTIME" ] || { echo "set APPIMAGETOOL and APPIMAGE_RUNTIME"; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
arch=$(uname -m)

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
app=$work/ytm-rs.AppDir

install -Dm755 "$bin" "$app/usr/lib/ytm-rs/ytm-rs"
echo "Run from the AppImage. This file has ytm-rs tell you about new versions instead of updating itself." > "$app/usr/lib/ytm-rs/appimage.txt"
# The AppImage starts AppRun; the app beside its marker, wherever the image is mounted.
cat > "$app/AppRun" <<'EOF'
#!/bin/sh
here=$(dirname "$(readlink -f "$0")")
exec "$here/usr/lib/ytm-rs/ytm-rs" "$@"
EOF
chmod 755 "$app/AppRun"
# appimagetool takes the menu entry and icon from the top of the AppDir.
install -Dm644 "$root/linux/ytm-rs.desktop" "$app/ytm-rs.desktop"
install -Dm644 "$root/assets/icon/icon.png" "$app/ytm-rs.png"
install -Dm644 "$root/assets/icon/icon.png" "$app/usr/share/icons/hicolor/256x256/apps/ytm-rs.png"

mkdir -p "$out"
image="$out/ytm-rs-$version-linux-$arch.AppImage"
# Unpacked rather than mounted: CI runners have no FUSE.
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=$arch "$APPIMAGETOOL" --no-appstream --runtime-file "$APPIMAGE_RUNTIME" "$app" "$image"
echo "Built $image"

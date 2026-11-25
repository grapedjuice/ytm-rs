#!/bin/sh
# Build the AppImage: one file holding the app, its menu entry and icon. Like the
# tarballs it runs on the system's own libraries (GTK, WebKitGTK, ALSA, …), so it needs
# what they need. Beside the app goes appimage.txt, which has it update by putting the
# new AppImage in place of the file it was started from.
#
#   linux/appimage.sh <binary> <version> <out dir>
#
# Needs appimagetool and the AppImage runtime to put in front, given as APPIMAGETOOL
# and APPIMAGE_RUNTIME, with the runtime's license and release tag as
# APPIMAGE_RUNTIME_LICENSE and APPIMAGE_RUNTIME_TAG (the release workflow downloads
# them, pinned).
set -e

bin=$1
version=$2
out=$3
[ -f "$bin" ] && [ -n "$version" ] && [ -n "$out" ] || { echo "usage: $0 <binary> <version> <out dir>"; exit 1; }
[ -x "$APPIMAGETOOL" ] && [ -f "$APPIMAGE_RUNTIME" ] && [ -f "$APPIMAGE_RUNTIME_LICENSE" ] && [ -n "$APPIMAGE_RUNTIME_TAG" ] ||
    { echo "set APPIMAGETOOL, APPIMAGE_RUNTIME, APPIMAGE_RUNTIME_LICENSE and APPIMAGE_RUNTIME_TAG"; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
arch=$(uname -m)

mkdir -p "$out"
work=$(mktemp -d "$out/.appimage.XXXXXX")
trap 'rm -rf "$work"' EXIT
app=$work/ytm-rs.AppDir

install -Dm755 "$bin" "$app/usr/lib/ytm-rs/ytm-rs"
install -Dm644 "$root/LICENSE" "$app/usr/share/licenses/ytm-rs/LICENSE"
{
    echo "The AppImage runtime in front of this image is type2-runtime $APPIMAGE_RUNTIME_TAG. It statically"
    echo "links libfuse (LGPL-2.1); its source, patches and build scripts are at"
    echo "https://github.com/AppImage/type2-runtime/tree/$APPIMAGE_RUNTIME_TAG"
    echo
    cat "$APPIMAGE_RUNTIME_LICENSE"
} > "$app/usr/share/licenses/ytm-rs/runtime.txt"
echo "Run from the AppImage. This file has ytm-rs update by replacing the AppImage file it was started from." > "$app/usr/lib/ytm-rs/appimage.txt"
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

image="$out/ytm-rs-$version-linux-$arch.AppImage"
# Unpacked rather than mounted: CI runners have no FUSE. Built aside, on the same disk,
# and moved in, so a failed build leaves no half-written image to attach.
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=$arch "$APPIMAGETOOL" --no-appstream --runtime-file "$APPIMAGE_RUNTIME" "$app" "$work/image"
mv "$work/image" "$image"
echo "Built $image"

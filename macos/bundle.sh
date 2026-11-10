#!/bin/sh
# Make ytm-rs.app from a built binary:
#   macos/bundle.sh target/release/ytm-rs 0.2.24 out [portable]
# With `portable`, the app keeps its data in a folder beside itself (see portable.txt).
# The app is signed ad hoc (there's no Developer ID), so macOS asks before its first
# launch: right-click it and pick Open, or run `xattr -dr com.apple.quarantine ytm-rs.app`.
set -e

bin=$1
version=$2
out=$3
kind=$4
[ -n "$bin" ] && [ -n "$version" ] && [ -n "$out" ] || { echo "usage: $0 <binary> <version> <out dir>"; exit 1; }
here=$(cd "$(dirname "$0")" && pwd)
icon="$here/../assets/icon/icon.png"
app="$out/ytm-rs.app"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/ytm-rs"
chmod 755 "$app/Contents/MacOS/ytm-rs"
# macOS wants numbers only: a nightly (0.2.34-nightly.20261110) shows as its base version.
sed "s/@VERSION@/${version%%-*}/g" "$here/Info.plist" > "$app/Contents/Info.plist"

# The Dock and Finder icon, at each size iconutil takes up to the 256 px source.
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
set=$tmp/ytm-rs.iconset
mkdir "$set"
for spec in 16:16x16 32:16x16@2x 32:32x32 64:32x32@2x 128:128x128 256:128x128@2x 256:256x256; do
    sips -z "${spec%%:*}" "${spec%%:*}" "$icon" --out "$set/icon_${spec#*:}.png" >/dev/null
done
iconutil -c icns "$set" -o "$app/Contents/Resources/ytm-rs.icns"

# Before signing: the signature seals everything in Resources.
[ "$kind" = portable ] && cp "$here/portable.txt" "$app/Contents/Resources/portable.txt"

codesign --force --sign - "$app"
echo "Made $app"

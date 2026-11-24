#!/usr/bin/env bash
# Build the Arch package inside archlinux:latest, with the checkout mounted at /src:
# the app in /usr/lib/ytm-rs, a `ytm-rs` command, and a menu entry with its icon. Beside
# the app goes pacman.txt, which has it tell you when a new version is out instead of
# updating itself (pacman owns the files). A new package installs over the old one.
#
#   bash linux/arch.sh <release-version>
set -euo pipefail

version="${1:?usage: bash linux/arch.sh <version>}"
# pkgver can't have a hyphen. A nightly (0.3.0-nightly.20261111) becomes
# 0.3.0.nightly.20261111, which pacman puts after 0.3.0 and before the next version,
# as in the app's own updater. The app looks for this name in the release.
pkg_version="${version//-/.}"

if [[ ! "$pkg_version" =~ ^[0-9][0-9A-Za-z.+_]*$ ]]; then
  echo "Invalid Arch package version: $pkg_version" >&2
  exit 1
fi
# 2.0.x was a joke: from 0.4.0 on, epoch=1 puts a version after it, as the
# app's updater does. makepkg puts it in the file's name (1:0.4.0); the release's
# copy leaves it out.
case $version in
  2.0.*) epoch= ;;
  *) epoch=1 ;;
esac

pacman -Syu --noconfirm --needed \
  base-devel git rust pkgconf clang cmake nasm \
  openssl alsa-lib gtk3 webkit2gtk-4.1 \
  libayatana-appindicator dbus zstd

# makepkg refuses to run as root. Use the checkout's UID so its build
# products and Cargo cache remain writeable on the mounted workspace.
repo_uid="$(stat -c %u /src)"
useradd -m -u "$repo_uid" builder

runuser -u builder -- cargo build --release --locked

mkdir -p _archpkg out
cat > _archpkg/PKGBUILD <<'PKGBUILD'
pkgname=ytm-rs
pkgver=0.0.0
pkgrel=1
pkgdesc='Native YouTube Music client'
arch=('x86_64')
url='https://github.com/grapedjuice/ytm-rs'
license=('GPL-3.0-only')
# The libraries it links. The tray's appindicator is loaded at run time.
# libssl.so and libcrypto.so: makepkg turns them into the major version the app was
# linked against here (libssl.so=4-64), so pacman refuses the package on a system with
# an older OpenSSL instead of installing one that can't start (#331).
# desktop-file-utils builds the MIME cache that `ytm-rs://` links are found through.
depends=('alsa-lib' 'gtk3' 'webkit2gtk-4.1' 'libayatana-appindicator' 'dbus' 'openssl' 'libssl.so' 'libcrypto.so' 'desktop-file-utils')
optdepends=('libpulse: Recognize song, through its parec')
options=('!debug')

package() {
  install -Dm755 /src/target/release/ytm-rs \
    "$pkgdir/usr/lib/ytm-rs/ytm-rs"
  echo "Installed from the Arch package. This file has ytm-rs tell you about new versions instead of updating itself." \
    > "$pkgdir/usr/lib/ytm-rs/pacman.txt"
  chmod 644 "$pkgdir/usr/lib/ytm-rs/pacman.txt"
  install -d "$pkgdir/usr/bin"
  ln -s ../lib/ytm-rs/ytm-rs "$pkgdir/usr/bin/ytm-rs"

  install -Dm644 /src/linux/ytm-rs.desktop \
    "$pkgdir/usr/share/applications/ytm-rs.desktop"
  install -Dm644 /src/assets/icon/icon.png \
    "$pkgdir/usr/share/icons/hicolor/256x256/apps/ytm-rs.png"
  install -Dm644 /src/LICENSE \
    "$pkgdir/usr/share/licenses/ytm-rs/LICENSE"
}
PKGBUILD

sed -i "s/^pkgver=.*/pkgver=$pkg_version/" _archpkg/PKGBUILD
[ -z "$epoch" ] || sed -i "s/^pkgver=/epoch=$epoch\npkgver=/" _archpkg/PKGBUILD
chown -R builder:builder _archpkg
runuser -u builder -- bash -c \
  'cd /src/_archpkg && makepkg --force --noconfirm --nodeps'

pkg="ytm-rs-$pkg_version-1-x86_64.pkg.tar.zst"
built="ytm-rs-${epoch:+$epoch:}$pkg_version-1-x86_64.pkg.tar.zst"
cp "_archpkg/$built" "out/$pkg"
ls -lh "out/$pkg"

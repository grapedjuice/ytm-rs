#!/usr/bin/env bash
# Build Arch Linux packages inside archlinux:latest, mounted at /src.
# Usage: bash linux/arch.sh <release-version>
set -euo pipefail

version="${1:?usage: bash linux/arch.sh <version>}"
pkg_version="${version//-/.}"

if [[ ! "$pkg_version" =~ ^[0-9][0-9A-Za-z.+_]*$ ]]; then
  echo "Invalid Arch package version: $pkg_version" >&2
  exit 1
fi

pacman -Syu --noconfirm --needed \
  base-devel git rust pkgconf clang cmake nasm \
  openssl alsa-lib gtk3 webkit2gtk-4.1 xdotool \
  libayatana-appindicator dbus zstd

# makepkg refuses to run as root. Use the checkout's UID so its build
# products and Cargo cache remain writeable on the mounted workspace.
repo_uid="$(stat -c %u /src)"
useradd -m -u "$repo_uid" builder

runuser -u builder -- cargo build --release --locked

mkdir -p _archpkg out _portable
cat > _archpkg/PKGBUILD <<'PKGBUILD'
pkgname=ytm-rs
pkgver=0.0.0
pkgrel=1
pkgdesc='Native YouTube Music client'
arch=('x86_64')
url='https://github.com/grapedjuice/ytm-rs'
license=('GPL-3.0-only')
depends=('alsa-lib' 'gtk3' 'webkit2gtk-4.1' 'libayatana-appindicator' 'xdotool' 'dbus' 'openssl')
options=('!debug')

package() {
  install -Dm755 /src/target/release/ytm-rs \
    "$pkgdir/usr/lib/ytm-rs/ytm-rs"
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
chown -R builder:builder _archpkg
runuser -u builder -- bash -c \
  'cd /src/_archpkg && makepkg --force --noconfirm --nodeps'

cp _archpkg/*.pkg.tar.zst out/

portable="ytm-rs-portable-${version}-linux-x86_64"
mkdir -p "_portable/$portable"
install -m755 target/release/ytm-rs "_portable/$portable/ytm-rs"
cp linux/portable.txt README.md LICENSE "_portable/$portable/"
tar -I 'zstd -T0 -19' -cf "out/$portable.tar.zst" \
  -C _portable "$portable"

ls -lh out/*.zst

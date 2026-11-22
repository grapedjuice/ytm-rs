{
  lib,
  rustPlatform,
  pkg-config,
  wrapGAppsHook3,
  stdenv,
  nasm,

  alsa-lib,
  dbus,
  fontconfig,
  glib-networking,
  gsettings-desktop-schemas,
  gtk3,
  libayatana-appindicator,
  libGL,
  libx11,
  libxkbcommon,
  libxcursor,
  libxi,
  libxrandr,
  openssl,
  pulseaudio,
  vulkan-loader,
  wayland,
  webkitgtk_4_1,
}:
let
  cargoToml = fromTOML (builtins.readFile ../Cargo.toml);
  version = cargoToml.package.version;

  inherit (stdenv.hostPlatform) isx86_64;
in
rustPlatform.buildRustPackage {
  pname = "ytm-rs";
  inherit version;
  src = ./..;
  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    wrapGAppsHook3
  ]
  ++ lib.optionals isx86_64 [
    nasm
  ];

  buildInputs = [
    alsa-lib
    dbus
    fontconfig
    glib-networking
    gsettings-desktop-schemas
    gtk3
    libayatana-appindicator
    libGL
    libx11
    libxkbcommon
    libxcursor
    libxi
    libxrandr
    openssl
    vulkan-loader
    wayland
    webkitgtk_4_1
  ];

  # The app loads these at run time rather than linking them. wrapGAppsHook3 builds the
  # wrapper from gappsWrapperArgs only (makeWrapperArgs isn't read for a Rust package).
  preFixup = ''
    gappsWrapperArgs+=(
      --prefix LD_LIBRARY_PATH : "${
        lib.makeLibraryPath [
          libayatana-appindicator
          libGL
          libx11
          libxkbcommon
          libxcursor
          libxi
          libxrandr
          vulkan-loader
          wayland
        ]
      }"
      # Recognize song records with parec: the system's first, if it has one.
      --suffix PATH : "${lib.makeBinPath [ pulseaudio ]}"
    )
  '';

  postInstall = ''
    install -Dm644 linux/ytm-rs.desktop $out/share/applications/ytm-rs.desktop
    install -Dm644 assets/icon/icon.png $out/share/icons/hicolor/256x256/apps/ytm-rs.png
    install -Dm644 LICENSE $out/share/licenses/ytm-rs/LICENSE
  '';

  meta = {
    description = "Native YouTube Music client";
    homepage = "https://github.com/grapedjuice/ytm-rs";
    license = lib.licenses.gpl3Only;
    mainProgram = "ytm-rs";
    platforms = lib.platforms.linux;
  };
}

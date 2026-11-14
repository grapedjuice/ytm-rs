{
  description = "Native YouTube Music client";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      forAllSystems = fn: nixpkgs.lib.genAttrs supportedSystems (system: fn system);
      pkgs = system: nixpkgs.legacyPackages.${system};
    in
    {
      packages = forAllSystems (system: {
        ytm-rs = (pkgs system).callPackage ./linux/package.nix { };
        default = self.packages.${system}.ytm-rs;
      });
      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/ytm-rs";
        };
      });
      devShells = forAllSystems (system: {
        default = (pkgs system).mkShell {
          packages = with (pkgs system); [
            stdenv
            rustc
            cargo
            cargo-auditable
            clippy
            pkg-config
            wrapGAppsHook3

            webkitgtk_4_1
            gtk3
            cairo
            gdk-pixbuf
            libsoup_3
            glib
            openssl
            dbus
            alsa-lib
          ];
          LD_LIBRARY_PATH = (pkgs system).lib.makeLibraryPath (
            with (pkgs system);
            [
              wayland
              libxkbcommon
              vulkan-loader
              libGL
              libayatana-appindicator
              libx11
              libxcursor
              libxrandr
            ]
          );
        };
      });
    };
}

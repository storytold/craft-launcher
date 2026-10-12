{
  description = "Experimental Nix flake for ArtCraft Launcher";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rustfmt" "clippy" "rust-src" ];
        };

        version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

        commonDeps = with pkgs; [
          pkg-config
          openssl
          libxkbcommon
          libx11
          libxcursor
          libxrandr
          libxi
          libxcb
          libGL
          vulkan-loader
          mesa
          wayland
          wayland-protocols
          fontconfig
          freetype
          alsa-lib
          zlib
        ];
      in
      {
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rust
            cmake
            ninja
            libclang
          ];
          buildInputs = commonDeps;

          shellHook = ''
            export LIBCLANG_PATH="${pkgs.libclang.lib}/lib"
            export PKG_CONFIG_PATH="${pkgs.openssl.dev}/lib/pkgconfig:$PKG_CONFIG_PATH"
          '';
        };

        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "artcraft-launcher";
          version = version;
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [
            pkg-config
            cmake
            ninja
            libclang
          ];
          buildInputs = commonDeps;

          doCheck = false;
        };

        apps.default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/artcraft-launcher";
        };
      });
}

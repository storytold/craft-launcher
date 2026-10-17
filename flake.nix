{
  description = "Experimental Nix flake for ArtCraft Launcher";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ] (system:
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
            makeBinaryWrapper
          ];
          buildInputs = commonDeps;

          # Keep Ninja available for dependencies without replacing Cargo's phases.
          dontUseNinjaBuild = true;
          dontUseNinjaCheck = true;
          dontUseNinjaInstall = true;

          doCheck = false;

          postInstall = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
            install -Dm644 packaging/linux/ai.storyteller.artcraft-launcher.desktop \
              "$out/share/applications/ai.storyteller.artcraft-launcher.desktop"
            mkdir -p "$out/share/icons"
            cp -R assets/app-icon/hicolor "$out/share/icons/"
          '';

          # winit and wgpu load these libraries dynamically, outside the ELF RPATH.
          postFixup = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
            wrapProgram "$out/bin/artcraft-launcher" \
              --set ARTCRAFT_LAUNCHER_APPIMAGE_RUNNER "${pkgs.lib.getExe pkgs.appimage-run}" \
              --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath (with pkgs; [
                libxkbcommon
                libx11
                libxcursor
                libxrandr
                libxi
                libxcb
                libGL
                vulkan-loader
                wayland
              ])}"
          '';
        };

        apps.default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/artcraft-launcher";
        };
      });
}

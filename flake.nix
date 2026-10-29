{
  description = "vesper - a simple freedesktop .desktop entry runner";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    { self, nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        inherit (pkgs) lib;
        cargoToml = lib.importTOML ./Cargo.toml;
        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./src
            ./tests
            ./fixtures
          ];
        };

        mkPackage =
          { withXdgWrappers ? false }:
          pkgs.rustPlatform.buildRustPackage {
            pname = cargoToml.package.name;
            inherit (cargoToml.package) version;
            inherit src;
            cargoLock.lockFile = ./Cargo.lock;
            meta.mainProgram = "vesper";

            postInstall = ''
              ln -s vesper "$out/bin/v"
            ''
            + lib.optionalString withXdgWrappers ''
              ln -s vesper "$out/bin/xdg-open"
              ln -s vesper "$out/bin/xdg-mime"
              ln -s vesper "$out/bin/xdg-settings"
            '';
          };
      in
      {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
            gcc
          ];
        };

        packages.default = mkPackage { };
        packages.xdgWrappers = mkPackage { withXdgWrappers = true; };

        # `nix flake check` runs the tests (via the package build), fmt and clippy.
        checks = {
          package = self.packages.${system}.default;
          lint = pkgs.rustPlatform.buildRustPackage {
            pname = "${cargoToml.package.name}-lint";
            inherit (cargoToml.package) version;
            inherit src;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = with pkgs; [
              rustfmt
              clippy
            ];
            buildPhase = ''
              cargo fmt -- --check
              cargo clippy --all-targets -- -D warnings
            '';
            doCheck = false;
            installPhase = "touch $out";
          };
        };
      }
    );
}

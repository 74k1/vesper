{
  description = "vesper - a simple freedesktop .desktop entry runner";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    { nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
      in
      {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
          ];
        };

        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "vesper";
          version = "0.1.0";
          src = ./.;
          cargoHash = "sha256-kWBB1uqX90goVCwNZ5a0Qs9/hkb5Jo8ERjLSng/Rg8Y=";
          # buildInputs = with pkgs; [ ];
          meta.mainProgram = "vesper";
        };
      }
    );
}

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
          cargoHash = "sha256-9QCvoikM4VOQINHqFv0F5HyOy+TL2692abYDv428T8E=";
          # buildInputs = with pkgs; [ ];
          meta.mainProgram = "vesper";
        };
      }
    );
}

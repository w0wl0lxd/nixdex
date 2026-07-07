{
  description = "nixdex - modern nix-index rewrite";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            pkgs.rustup
            pkgs.mise
            pkgs.nix-eval-jobs
            pkgs.nix
            pkgs.mold
            pkgs.clang
            pkgs.sccache
            pkgs.pkg-config
            pkgs.openssl
            pkgs.protobuf
          ];

          shellHook = ''
            eval "$(mise activate bash)"
          '';
        };
      }
    );
}

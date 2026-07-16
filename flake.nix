{
  description = "nixdex - modern nix-index / nix-locate rewrite";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      crane,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        craneLib = crane.mkLib pkgs;

        src = craneLib.cleanCargoSource ./.;

        commonArgs = {
          inherit src;
          strictDeps = true;
          buildInputs = [ ];
          nativeBuildInputs = [
            pkgs.pkg-config
            pkgs.openssl
          ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        nixdex = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            pname = "nixdex";
            cargoExtraArgs = "-p nixdex-cli";
            doCheck = true;
            cargoTestExtraArgs = "-p nixdex-core";
          }
        );
      in
      {
        packages.default = nixdex;
        packages.nixdex = nixdex;

        apps.default = flake-utils.lib.mkApp {
          drv = nixdex;
          name = "nix-locate";
        };

        checks = {
          inherit nixdex;
        };

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
            pkgs.gitleaks
            pkgs.ripsecrets
            pkgs.hyperfine
          ];

          shellHook = ''
            eval "$(mise activate bash)"

            # Rustup-distributed nightly rustc dynamically links libz, which is not
            # present in the NixOS FHS. Expose it to mise/rustup toolchains.
            export LD_LIBRARY_PATH="${
              pkgs.lib.makeLibraryPath [ pkgs.zlib ]
            }''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

            git config core.hooksPath .githooks 2>/dev/null || true
          '';
        };
      }
    );
}

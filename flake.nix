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

        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter =
            path: type:
            let
              base = baseNameOf path;
              inCratesCliAssets = pkgs.lib.hasInfix "/crates/nixdex-cli/assets" path;
            in
            craneLib.filterCargoSources path type
            || (
              inCratesCliAssets
              && (base == "assets" || base == "command-not-found.sh" || base == "command-not-found.nu" || base == "command-not-found.fish")
            );
        };

        commonArgs = {
          inherit src;
          strictDeps = true;
          buildInputs = [ pkgs.openssl ];
          nativeBuildInputs = [
            pkgs.cacert
            pkgs.clang
            pkgs.mold
            pkgs.pkg-config
          ];

          # The workspace `.cargo/config.toml` enables `sccache`, which is not
          # available (or useful) inside the Nix sandbox.
          RUSTC_WRAPPER = "";

          # `rustls-native-certs` has no system cert store in the sandbox.
          SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
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

            postInstall = ''
              mkdir -p $out/etc/profile.d
              substitute ${src}/crates/nixdex-cli/assets/command-not-found.sh \
                $out/etc/profile.d/command-not-found.sh \
                --replace-fail "@out@" "$out"
              chmod +x $out/etc/profile.d/command-not-found.sh
              substitute ${src}/crates/nixdex-cli/assets/command-not-found.nu \
                $out/etc/profile.d/command-not-found.nu \
                --replace-fail "@out@" "$out"
              chmod 444 $out/etc/profile.d/command-not-found.nu
              substitute ${src}/crates/nixdex-cli/assets/command-not-found.fish \
                $out/etc/profile.d/command-not-found.fish \
                --replace-fail "@out@" "$out"
              chmod 444 $out/etc/profile.d/command-not-found.fish
            '';
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
    )
    // {
      nixosModules.default =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        import ./nix/nixos-module.nix { package = self.packages.${pkgs.system}.nixdex or pkgs.nixdex; } {
          inherit config lib pkgs;
        };

      homeModules.default =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        import ./nix/home-module.nix { package = self.packages.${pkgs.system}.nixdex or pkgs.nixdex; } {
          inherit config lib pkgs;
        };
    };
}

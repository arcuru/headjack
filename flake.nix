{
  description = "Headjack - Jack into Matrix.";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

    crane.url = "github:ipetkov/crane";

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.rust-analyzer-src.follows = "";
    };

    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };

    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs @ {flake-parts, ...}:
    flake-parts.lib.mkFlake {inherit inputs;} {
      imports = [
        inputs.treefmt-nix.flakeModule
      ];

      systems = [
        "aarch64-linux"
        "x86_64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];

      perSystem = {
        config,
        system,
        pkgs,
        lib,
        ...
      }: let
        # Pin to Rust 1.93.0 to avoid rustc regression (rust-lang/rust#152942)
        # that causes matrix-sdk 0.16 to hit query depth limits on 1.94+
        fenixStable = inputs.fenix.packages.${system}.toolchainOf {
          channel = "1.93.0";
          sha256 = "sha256-vra6TkHITpwRyA5oBKAHSX0Mi6CBDNQD+ryPSpxFsfg=";
        };
        rustSrc = fenixStable.rust-src;
        toolChain = fenixStable.completeToolchain;

        craneLib = (inputs.crane.mkLib pkgs).overrideToolchain toolChain;

        src = craneLib.cleanCargoSource (craneLib.path ./.);

        commonArgs = {
          inherit src;
          nativeBuildInputs = with pkgs; [
            pkg-config
          ];
          buildInputs = [
          ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        buildArgs = commonArgs // {inherit cargoArtifacts;};

        headjack = craneLib.buildPackage buildArgs;

        headjack-clippy = craneLib.cargoClippy (buildArgs
          // {
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          });

        headjack-doc = craneLib.cargoDoc buildArgs;
        headjack-fmt = craneLib.cargoFmt buildArgs;
        headjack-nextest = craneLib.cargoNextest (buildArgs
          // {
            cargoNextestExtraArgs = "--no-tests=warn";
          });

        headjack-deny = craneLib.mkCargoDerivation (buildArgs
          // {
            pnameSuffix = "-deny";
            buildPhaseCargoCommand = "cargo deny check --config .config/deny.toml";
            nativeBuildInputs = (buildArgs.nativeBuildInputs or []) ++ [pkgs.cargo-deny];
            src = ./.;
          });

        mkAggregate = name: packages:
          pkgs.symlinkJoin {
            inherit name;
            paths = builtins.attrValues packages;
          };

        lintDefaults = {
          inherit headjack-clippy;
        };

        lintAll =
          lintDefaults
          // {
            inherit headjack-deny;
          };
      in {
        legacyPackages = {
          lint =
            lintAll
            // {
              default = mkAggregate "lint" lintDefaults;
              all = mkAggregate "lint-all" lintAll;
            };

          test = {
            default = headjack-nextest;
          };

          doc = {
            default = headjack-doc;
          };

          default = headjack;
        };

        packages = {
          inherit headjack;
          default = headjack;
        };

        checks = {
          build = headjack;
          test = headjack-nextest;
          lint = mkAggregate "lint" lintDefaults;
          doc = headjack-doc;
        };

        treefmt = {
          projectRootFile = "flake.nix";
          programs = {
            alejandra.enable = true;
            prettier.enable = true;
            rustfmt.enable = true;
          };
        };

        devShells.default = pkgs.mkShell {
          name = "headjack";
          shellHook = ''
            echo ---------------------
            just --list
            echo ---------------------
          '';

          inputsFrom = [
            headjack
            headjack-clippy
          ];

          nativeBuildInputs = with pkgs; [
            alejandra
            cargo-deny
            cargo-nextest
            deadnix
            git-cliff
            just
            prettier
            nix-fast-build
            statix
            config.treefmt.build.wrapper
          ];

          RUST_SRC_PATH = "${rustSrc}/lib/rustlib/src/rust/library";
        };
      };
    };
}

{
  description = "Bibliography metadata fetcher";

  inputs = {
    flake-parts.url = "github:hercules-ci/flake-parts";
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    treefmt-nix.url = "github:numtide/treefmt-nix";
  };

  outputs = inputs @ {flake-parts, ...}:
    flake-parts.lib.mkFlake {inherit inputs;} {
      systems = ["x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin"];

      imports = [
        inputs.treefmt-nix.flakeModule
      ];

      perSystem = {
        config,
        pkgs,
        system,
        ...
      }: {
        _module.args.pkgs = import inputs.nixpkgs {
          inherit system;
          overlays = [
            inputs.rust-overlay.overlays.default

            (final: prev: {
              rustToolchains = {
                stable = pkgs.rust-bin.stable.latest.default.override {
                  extensions = [
                    "rust-src"
                    "rust-analyzer"
                    "llvm-tools"
                  ];
                };
                nightly = pkgs.rust-bin.nightly.latest.default.override {
                  extensions = [
                    "rust-src"
                    "miri"
                  ];
                };
              };
            })
          ];
        };

        packages.default = pkgs.rustPlatform.buildRustPackage {
          name = "bib";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
        };

        # Run `nix develop` to get a standardised shell anywhere with all the packages
        # needed for this project.
        devShells.default = pkgs.mkShell {
          inputsFrom = [
            config.treefmt.build.devShell
          ];

          shellHook = ''
            # For rust-analyzer 'hover' tooltips to work
            export RUSTC_SRC_PATH=${pkgs.rustPlatform.rustLibSrc}
          '';

          nativeBuildInputs = with pkgs; [
            nixd
            alejandra
            nixfmt
            statix
            deadnix
            rustToolchains.stable
            cargo
            rustc
            bacon
            cargo-watch
            cargo-expand
            cargo-tarpaulin
            rust-analyzer
          ];

          # Environment variable inside the shell
          env = {
            RUST_BACKTRACE = "full";
          };
        };

        # Nightly compilator to run miri tests
        devShells.nightly = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rustToolchains.nightly
            bacon
            rust-analyzer
          ];
        };

        # Auto-format your entire project tree
        treefmt.config = {
          projectRootFile = "flake.nix";
          programs = {
            alejandra.enable = true;
            rustfmt.enable = true;
          };
        };
      };
    };
}

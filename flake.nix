{
  description = "The EdgeDB CLI";
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs";
    flake-parts.url = "github:hercules-ci/flake-parts";

    # provides rust toolchain
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.rust-analyzer-src.follows = "";
    };

    edgedb = {
      url = "github:edgedb/packages-nix";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-parts.follows = "flake-parts";
    };
  };

  outputs =
    inputs@{
      flake-parts,
      fenix,
      edgedb,
      ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      perSystem =
        {
          config,
          system,
          pkgs,
          ...
        }:
        let
          fenix_pkgs = fenix.packages.${system};
          rust_toolchain = fenix_pkgs.toolchainOf {
            channel = "1.88";
            sha256 = "sha256-Hn2uaQzRLidAWpfmRwSRdImifGUCAb9HeAqTYFXWeQk=";
          };

          common =
            [
              # needed for running tests
              edgedb.packages.${system}.gel-server-nightly
            ]
            ++ pkgs.lib.optional pkgs.stdenv.isDarwin [
              pkgs.libiconv
              pkgs.darwin.apple_sdk.frameworks.CoreServices
              pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
            ];

        in
        {
          devShells.default = pkgs.mkShell {
            buildInputs = common ++ [
              (rust_toolchain.withComponents [
                "rustc"
                "cargo"
                "rust-std"
                "clippy"
                "rustfmt"
                "rust-src"
                "rust-analyzer"
              ])
            ];
            shellHook = ''
              export PATH="$(git rev-parse --show-toplevel)/target/debug:$PATH"
            '';
          };
        };
    };
}

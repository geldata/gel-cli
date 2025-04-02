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
      inputs.fenix.follows = "fenix";
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

          common =
            [
              # needed for running tests
              edgedb.packages.${system}.edgedb-server-nightly
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
              (fenix_pkgs.combine [
                (fenix_pkgs.fromToolchainFile {
                  file = ./rust-toolchain.toml;
                  sha256 = "sha256-Hn2uaQzRLidAWpfmRwSRdImifGUCAb9HeAqTYFXWeQk=";
                })
                (fenix_pkgs.targets.x86_64-unknown-linux-musl.fromToolchainFile {
                  file = ./rust-toolchain.toml;
                  sha256 = "sha256-Hn2uaQzRLidAWpfmRwSRdImifGUCAb9HeAqTYFXWeQk=";
                })
              ])
            ];
          };
        };
    };
}

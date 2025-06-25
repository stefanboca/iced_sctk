{
  description = "Limbo bar, now with more rust";

  inputs = {
    flake-parts.url = "github:hercules-ci/flake-parts";
    nixpkgs.url = "github:cachix/devenv-nixpkgs/rolling";
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    inputs@{ flake-parts, nixpkgs, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      perSystem =
        {
          config,
          self',
          inputs',
          pkgs,
          lib,
          system,
          ...
        }:
        {
          # use fenix overlay
          _module.args.pkgs = import nixpkgs {
            inherit system;
            overlays = [ inputs.fenix.overlays.default ];
          };

          devShells.default = pkgs.mkShell {
            nativeBuildInputs = with pkgs; [ pkg-config ];
            LD_LIBRARY_PATH = lib.makeLibraryPath (
              with pkgs;
              [
                libGL
                libxkbcommon
                vulkan-loader
                wayland
              ]
            );
            PKG_CONFIG_PATH = lib.concatStringsSep ":" [
              "${pkgs.wayland.dev}/lib/pkgconfig"
              "${pkgs.libxkbcommon.dev}/lib/pkgconfig"
            ];
          };
        };
    };
}

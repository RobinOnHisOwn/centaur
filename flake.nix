{
  description = "Centaur terminal client";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = {self, nixpkgs}: let
    systems = [
      "x86_64-linux"
      "aarch64-linux"
      "x86_64-darwin"
      "aarch64-darwin"
    ];
  in {
    packages = nixpkgs.lib.genAttrs systems (system: let
      pkgs = import nixpkgs {inherit system;};
      centaur-session-cli = pkgs.callPackage ./nix/centaur-session-cli.nix {
        src = self;
      };
    in {
      inherit centaur-session-cli;
      default = centaur-session-cli;
    });
  };
}

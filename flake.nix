{
  description = "qOS collaborative desktop";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in {
      packages = forAllSystems (system:
        let pkgs = import nixpkgs { inherit system; };
        in {
          qos-session = pkgs.callPackage ./dist/nix/package.nix { };
          default = self.packages.${system}.qos-session;
        });

      nixosModules.qos-session = { config, lib, pkgs, ... }:
        let cfg = config.services.qos-session;
        in {
          options.services.qos-session = {
            enable = lib.mkEnableOption "the qOS Wayland session";
            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.system}.qos-session;
              description = "qOS session package to install.";
            };
          };
          config = lib.mkIf cfg.enable {
            environment.systemPackages = [ cfg.package ];
            services.displayManager.sessionPackages = [ cfg.package ];
            environment.etc."pam.d/qos-unlock".source = "${cfg.package}/etc/pam.d/qos-unlock";
            xdg.portal.extraPortals = [ cfg.package ];
          };
        };

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt-rfc-style);
    };
}

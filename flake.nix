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

      checks = forAllSystems (system: {
        # Force the exported module through a real NixOS option evaluation on
        # each release architecture. `nix flake check --no-build` then catches
        # stale display-manager, PAM, or portal option wiring without building
        # a second system closure.
        qos-session-module = (nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            self.nixosModules.qos-session
            {
              # Supply the minimal machine shape required by a complete NixOS
              # evaluation; this check validates the session module rather
              # than producing a bootable image.
              fileSystems."/" = {
                device = "none";
                fsType = "tmpfs";
              };
              boot.loader.grub.devices = [ "nodev" ];
              services.qos-session.enable = true;
              system.stateVersion = "24.11";
            }
          ];
        }).config.system.build.toplevel;
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
            environment.etc."systemd/user/qos-graphical-session.target".source =
              "${cfg.package}/share/systemd/user/qos-graphical-session.target";
            xdg.portal.enable = true;
            xdg.portal.extraPortals = [ cfg.package ];
          };
        };

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt-rfc-style);
    };
}

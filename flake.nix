{
  description = "Tilo collaborative desktop";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in {
      packages = forAllSystems (system:
        let pkgs = import nixpkgs { inherit system; };
        in {
          tilo-session = pkgs.callPackage ./dist/nix/package.nix { };
          tilo-runtime = pkgs.callPackage ./dist/nix/runtime.nix { };
          default = self.packages.${system}.tilo-session;
        });

      checks = forAllSystems (system: {
        # Force the exported module through a real NixOS option evaluation on
        # each release architecture. `nix flake check --no-build` then catches
        # stale display-manager, PAM, or portal option wiring without building
        # a second system closure.
        tilo-session-module = (nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            self.nixosModules.tilo-session
            {
              # Supply the minimal machine shape required by a complete NixOS
              # evaluation; this check validates the session module rather
              # than producing a bootable image.
              fileSystems."/" = {
                device = "none";
                fsType = "tmpfs";
              };
              boot.loader.grub.devices = [ "nodev" ];
              services.tilo-session.enable = true;
              system.stateVersion = "24.11";
            }
          ];
        }).config.system.build.toplevel;
        tilo-runtime-module = (nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            self.nixosModules.tilo-runtime
            {
              fileSystems."/" = {
                device = "none";
                fsType = "tmpfs";
              };
              boot.loader.grub.devices = [ "nodev" ];
              services.tilo-runtime.enable = true;
              system.stateVersion = "24.11";
            }
          ];
        }).config.system.build.toplevel;
      });

      nixosModules.tilo-session = { config, lib, pkgs, ... }:
        let cfg = config.services.tilo-session;
        in {
          options.services.tilo-session = {
            enable = lib.mkEnableOption "the Tilo Wayland session";
            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.system}.tilo-session;
              description = "Tilo session package to install.";
            };
          };
          config = lib.mkIf cfg.enable {
            environment.systemPackages = [ cfg.package ];
            services.displayManager.sessionPackages = [ cfg.package ];
            environment.etc."pam.d/tilo-unlock".source = "${cfg.package}/etc/pam.d/tilo-unlock";
            environment.etc."systemd/user/tilo-graphical-session.target".source =
              "${cfg.package}/share/systemd/user/tilo-graphical-session.target";
            # NixOS's PipeWire reads /etc/pipewire, not the package's share
            # directory; Settings offers "Reduce echo in calls" only when
            # `pipewire -c tilo-echo-cancel.conf` can find the config.
            environment.etc."pipewire/tilo-echo-cancel.conf".source =
              "${cfg.package}/share/pipewire/tilo-echo-cancel.conf";
            xdg.portal.enable = true;
            xdg.portal.extraPortals = [ cfg.package ];
          };
        };

      nixosModules.tilo-runtime = { config, lib, pkgs, ... }:
        let cfg = config.services.tilo-runtime;
        in {
          options.services.tilo-runtime = {
            enable = lib.mkEnableOption "the Tilo runtime for running Tilo apps on this desktop";
            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.system}.tilo-runtime;
              description = "Tilo runtime package to install.";
            };
          };
          config = lib.mkIf cfg.enable {
            environment.systemPackages = [ cfg.package ];
            systemd.packages = [ cfg.package ];
            systemd.user.services.tilo-runtime.wantedBy = [ "default.target" ];
          };
        };

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt-rfc-style);
    };
}

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
          default = self.packages.${system}.tilo-session;
        });

      checks = forAllSystems (system: {
        # Validate Cargo.lock against the fixed vendor output without compiling
        # the entire desktop session.
        tilo-cargo-vendor = self.packages.${system}.tilo-session.cargoDeps;

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
              services.displayManager.enable = true;
              services.pipewire.enable = true;
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
            # NixOS owns /etc/systemd/user as one generated tree. Adding a
            # nested environment.etc file under it fails while assembling /etc.
            systemd.user.units."tilo-graphical-session.target".text =
              builtins.readFile ./os/usr/lib/systemd/user/tilo-graphical-session.target;
            # PipeWire builds /etc/pipewire from configPackages. Supplying an
            # environment.etc child underneath that tree conflicts with it.
            services.pipewire.configPackages = [ cfg.package ];
            xdg.portal.enable = true;
            xdg.portal.extraPortals = [ cfg.package ];
          };
        };

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt-rfc-style);
    };
}

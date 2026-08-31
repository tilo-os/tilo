# Packaging the qOS desktop session

The host package installs qOS as an additional Wayland session on an existing
Linux system. It is deliberately smaller than the bootc operating-system image:
the package contains the compositor, sandbox launcher, unlock helper, PAM policy,
portal registration, and display-manager entry. It does not replace the host's
boot chain, greeter, or package manager.

## Supported outputs

| Distribution | Release files | Architectures |
| --- | --- | --- |
| Ubuntu / Debian | `qos-session_*.deb`, `qos-session-devel_*.deb` | `amd64`, `arm64` |
| Fedora | `qos-session-*.rpm`, `qos-session-devel-*.rpm` | `x86_64`, `aarch64` |
| Arch / Arch Linux ARM | `qos-session-*.pkg.tar.zst`, `qos-session-devel-*.pkg.tar.zst` | `x86_64`, `aarch64` |
| Nix / NixOS | flake package and NixOS module; tagged store-closure export | `x86_64-linux`, `aarch64-linux` |

`qos-session-devel` is optional and is only for machines developing qOS. It
adds a second login entry that can start a user-selected local build after
checking its ownership, directory permissions, and embedded development marker.

## Local builds

Install the native dependencies listed in `DEVELOPMENT.md`, then run one of:

```bash
dist/debian/build-deb.sh
dist/rpm/build-rpm.sh
dist/arch/build-arch.sh       # makepkg must run as a non-root user
nix build .#qos-session
```

All FHS packages share `dist/stage-payload.sh`; `dist/verify-payload.sh` asserts
the installed paths and the session/portal wiring. Release builds exclude the
test-control socket and ffmpeg-backed HDR decoding so the login-session binary
does not acquire a distribution-specific libavcodec ABI.

The package version has one source of truth: `dist/VERSION`. Before tagging,
update it together with the package changelogs, then create a matching `vX.Y.Z`
tag.

## NixOS

The flake exposes both a package and a module:

```nix
{
  inputs.qos.url = "git+https://code.sandbox.example/qos/qos";
  outputs = { nixpkgs, qos, ... }: {
    nixosConfigurations.my-machine = nixpkgs.lib.nixosSystem {
      modules = [
        qos.nixosModules.qos-session
        { services.qos-session.enable = true; }
      ];
    };
  };
}
```

The module registers the display-manager session, PAM policy, and portal. Using
the package alone is useful for nested development, but a real login session
should use the module so `/etc/pam.d/qos-unlock` is activated declaratively.

## CI and publishing

`.forgejo/workflows/package-desktop.yml` is the release gate. Pull requests
that touch packaging or build inputs execute the complete matrix. Version tags
execute it regardless of changed paths, wait for every install test, download
the artifacts, and create the Forgejo release only after all eight native jobs
succeed.

The workflow requires native Forgejo runners carrying these labels:

- `runner,amd64` on an x86-64 machine;
- `runner,arm64` on an AArch64 machine.

Native runners are required because qOS links the distribution's DRM, input,
seat, graphics, PipeWire, and portal libraries. QEMU builds would be much slower
and would not test the package against the actual target ABI. Each runner must
support job containers. No publishing secret beyond the existing
`REGISTRY_TOKEN` is introduced; it must have repository write permission for
tag releases.

Every distro job builds inside its target distribution and then installs the
result with that distribution's package manager. The Nix job builds the flake,
checks the session entry and ELF architecture, and exports the complete store
closure as a `.nar.gz`. Tagged release assets therefore contain only artifacts
that passed their native install test.

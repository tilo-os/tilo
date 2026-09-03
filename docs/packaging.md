# Packaging the qOS desktop session

The host package installs qOS as an additional Wayland session on an existing
Linux system. It is deliberately smaller than the bootc operating-system image:
the package contains the compositor, sandbox launcher, unlock helper, PAM policy,
portal registration, display-manager entry, and AGPL-3.0 license text. It does
not replace the host's boot chain, greeter, or package manager.

## Supported outputs

| Distribution | Release files | Architectures |
| --- | --- | --- |
| Ubuntu 24.04 | `qos-session_*+ubuntu24.04*.deb` | `amd64` (`arm64` packaging implemented, release lane deferred) |
| Debian 13 | `qos-session_*+deb13*.deb` | `amd64` (`arm64` packaging implemented, release lane deferred) |
| Fedora 44 | `qos-session-*.rpm`, `qos-session-devel-*.rpm` | `x86_64` (`aarch64` deferred) |
| Arch Linux | `qos-session-*.pkg.tar.zst`, `qos-session-devel-*.pkg.tar.zst` | `x86_64` (`aarch64` deferred) |
| NixOS | flake package and NixOS module; tagged store-closure export | `x86_64-linux` (`aarch64-linux` deferred) |

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
update it, then create a matching `vX.Y.Z` tag. Debian and Ubuntu receive
separate package revisions because each binary is linked and dependency-scanned
inside its own distribution build root.

## Install from code.podesta.ai

Tagged releases are uploaded both as release assets and to Forgejo's native
package registries under the `qos` organization. Once the organization is
public, Debian 13 (`trixie`) and Ubuntu 24.04 (`noble`) users can add the apt
repository with:

```bash
sudo install -d -m 0755 /etc/apt/keyrings
sudo curl -fsSL https://code.podesta.ai/api/packages/qos/debian/repository.key \
  -o /etc/apt/keyrings/forgejo-qos.asc
echo "deb [signed-by=/etc/apt/keyrings/forgejo-qos.asc] https://code.podesta.ai/api/packages/qos/debian noble main" \
  | sudo tee /etc/apt/sources.list.d/qos.list
sudo apt update
sudo apt install qos-session
```

Use `trixie` instead of `noble` on Debian 13. Fedora 44 users can add the RPM
registry and install directly with `dnf`:

```bash
sudo dnf config-manager addrepo \
  --from-repofile=https://code.podesta.ai/api/packages/qos/rpm/fedora/44.repo
sudo dnf install qos-session
```

For Arch or Arch Linux ARM, add this repository to `/etc/pacman.conf`:

```ini
[qos.stable.code.podesta.ai]
SigLevel = Required
Server = https://code.podesta.ai/api/packages/qos/arch/stable/$arch
```

Import and locally trust the registry key once, then install normally:

```bash
curl -fsSL https://code.podesta.ai/api/packages/qos/arch/repository.key -o qos-forgejo.gpg
sudo pacman-key --add qos-forgejo.gpg
sudo pacman-key --lsign-key 'qos@noreply.code.podesta.ai'
sudo pacman -Sy qos-session
```

While the organization remains private, use a Forgejo username and personal
access token in the repository URL. Do not put a token in this repository.

## NixOS

The flake exposes both a package and a module:

```nix
{
  inputs.qos.url = "git+https://code.podesta.ai/qos/qos";
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
Tagged builds also publish an exact Nix store closure as a generic Forgejo
package for disaster recovery; normal installations should use the flake so
Nix can build or substitute the package declaratively.

## CI and publishing

`.forgejo/workflows/package-desktop.yml` is the release gate. Pull requests
that touch packaging or the packaged binaries execute the current x86-64 matrix.
Version tags execute it regardless of changed paths, wait for every x86-64 install
test, download the artifacts, require the complete current output set, and only then
publish the apt, RPM, pacman, and generic Nix registries plus a checksummed
Forgejo release. The five target userspaces run sequentially in one
capacity-bounded lane.

The workflow uses the existing `runner` Forgejo label. amd64 containers run
directly, and every target userspace is pinned to a verified OCI digest rather
than a mutable distribution tag. The `dist/ci` runner can execute other
architectures through a job-private PRoot and checksum-pinned QEMU without
registering a host binfmt handler, but that mechanism is not the release gate:
the Nix closure must be validated on a native ARM64 runner first.
The package matrix runs as one sequential x86-64 lane; native Cargo builds use
at most four compiler jobs. ARM64 packaging remains implemented in `dist/ci`,
but is deliberately excluded until a native runner can verify its Nix closure.
This leaves capacity for main-branch checks on the shared runner pool.
The Debian-family, Fedora, and Arch jobs reuse the runners' optional remote
compiler cache in isolated distro/architecture namespaces and fail open to an
ordinary build if that service is unavailable. Cache credentials are only
forwarded on trusted branch, tag, and manual runs; pull requests build cold.
Nix builds remain pure.

The build is not a generic cross-compile: Cargo and the target package manager
both execute inside the selected Ubuntu, Debian, Fedora, Arch, or Nix userspace,
so dependency resolution, ELF linkage, package metadata, installation, and
`ldd` verification all exercise the target ABI. The runner must expose Docker
and allow ordinary Docker create/copy/export/start operations. The deferred
cross-architecture path additionally needs unprivileged `ptrace`. No publishing secret
beyond the existing `REGISTRY_TOKEN` is introduced; it must have repository and
package write permission for tag releases.

Every distro job first checks the architecture visible inside its container,
builds inside its target distribution, and then installs both the main and
developer packages with that distribution's package manager. The Nix job builds
the flake, checks every installed path, immutable store substitution, native ELF
architecture and shared-library closure, and exports the complete store closure
as a `.nar.gz`. Tagged release assets therefore contain only artifacts that
passed their target-architecture install test.

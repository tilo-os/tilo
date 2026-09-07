# Packaging the qOS desktop session

The host package installs qOS as an additional Wayland session on an existing
Linux system. It is deliberately smaller than the bootc operating-system image:
the package contains the compositor, sandbox launcher, unlock helper, PAM policy,
portal registration, display-manager entry, and AGPL-3.0 license text. It does
not replace the host's boot chain, greeter, or package manager.

The session scans the host's normal XDG application directories, including
per-user and system Flatpak exports. Those existing applications keep their
real home directory and their original confinement model, and qOS labels them
as unconfined host apps rather than implying they use qOS package policy. On
Fedora, `xwayland-satellite` is a hard package dependency so legacy X11-only
applications continue to open; native Wayland applications still use the qOS
socket directly. The complete qOS image remains Wayland-only—this compatibility
bridge is enabled only by the explicit host-desktop session mode.

The wrapper owns `graphical-session.target` for the lifetime of qOS, clears
display variables left by the preceding desktop before starting it, and removes
qOS's desktop identity when the session ends. That is what allows portal and
other standard user services to use qOS while logged in and ensures they stop
before GNOME, KDE, or another session is selected next.
The portal frontend and filtered D-Bus proxy are hard package dependencies, so
sandboxed qOS applications do not silently lose desktop APIs on minimal hosts.

The package-owned wrapper sends compositor stdout and stderr directly to the
user journal with `SYSLOG_IDENTIFIER=qos-session`. After returning to another
desktop, startup, portal, application-launch, and crash diagnostics remain
available with `journalctl --user -t qos-session`; no GDM-specific session-log
path is required. The wrapper records no environment dump or credentials, and
the journal keeps the host's normal per-user/root access controls.

## Supported outputs

| Distribution | Release files | Architectures |
| --- | --- | --- |
| Ubuntu 24.04 | `qos-session_*+ubuntu24.04*.deb` | `amd64`; `arm64` when enabled |
| Debian 13 | `qos-session_*+deb13*.deb` | `amd64`; `arm64` when enabled |
| Fedora 44 | `qos-session-*.rpm`, `qos-session-devel-*.rpm` | `x86_64`; `aarch64` when enabled |
| Arch Linux | `qos-session-*.pkg.tar.zst`, `qos-session-devel-*.pkg.tar.zst` | `x86_64`; `aarch64` when enabled |
| NixOS | flake package and NixOS module; tagged store-closure export | `x86_64-linux`; `aarch64-linux` when enabled |

The complete ARM64 package definitions and release verifier remain in tree,
but ARM64 publication is temporarily disabled by
`QOS_DESKTOP_ARM64_RELEASES` until a native `runner-arm64` host is available.
Current tags therefore publish the x86-64 set. Changing that single workflow
switch to `"1"` restores the complete native ARM64 release lane.

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
update it, then create a matching `desktop-vX.Y.Z` tag. Debian and Ubuntu receive
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

Use `trixie` instead of `noble` on Debian 13. Fedora 44 users must first
bootstrap the RPM release key and maintained repo definition from the exact
`desktop-vX.Y.Z` release they are installing. Forgejo's generated RPM repo
file does not advertise a qOS key or enable package verification; the release
workflow publishes a repo definition with those settings already present:
`https://code.podesta.ai/api/packages/qos/rpm/fedora/44.repo` is the upstream
Forgejo-generated endpoint, retained for diagnostics and private-server
fallbacks only.

```bash
QOS_DESKTOP_VERSION=0.1.5
sudo install -d -m 0755 /etc/pki/rpm-gpg
sudo curl --fail --location \
  --output /etc/pki/rpm-gpg/qos-rpm-signing-key.asc \
  "https://code.podesta.ai/qos/qos/releases/download/desktop-v${QOS_DESKTOP_VERSION}/qos-rpm-signing-key.asc"
sudo curl --fail --location \
  --output /etc/yum.repos.d/qos.repo \
  "https://code.podesta.ai/qos/qos/releases/download/desktop-v${QOS_DESKTOP_VERSION}/qos-fedora-44.repo"
```

`gpgcheck=1` makes DNF/rpm-ostree verify every qOS RPM. Forgejo's native RPM
registry's generated `.repo` does not publish `repomd.xml.asc`, so the
maintained definition uses `repo_gpgcheck=0`; HTTPS protects the unsigned
metadata and the package signature protects the payload. Compare the release
key's full fingerprint with the fingerprint published through an independent
qOS channel before trusting it. No manual repo-file edit is needed on the
public path.

On a traditional mutable Fedora system, install from the configured repo with:

```bash
sudo dnf install qos-session
```

On Fedora Atomic desktops such as Silverblue, Kinoite, or an rpm-ostree-based
derivative, use the same configured repo and layer the package into the next
deployment instead:

```bash
sudo rpm-ostree install qos-session
```

Save your work and reboot into the prepared deployment. GDM will continue to
offer the existing desktop and will also offer qOS in its session menu. The
layering transaction preserves other requested and locally installed packages;
applications installed system-wide or for your user, including Flatpaks, remain
installed and discoverable from qOS under **Host Apps**. Select the existing
desktop in GDM whenever you want to switch back. Removing qOS later uses
`sudo rpm-ostree uninstall qos-session` and another reboot. Do not use the
development subpackage for an ordinary installation.

If a later host or qOS deployment is unhealthy, choose the previous deployment
from the boot menu or run `sudo rpm-ostree rollback`, then reboot. Rollback and
uninstall change only the immutable deployment/package layer; the existing
home directory, layered applications, and Flatpak data remain untouched.

When this repository publishes a newer `qos-session`, the normal
`sudo rpm-ostree update` command (an alias for `rpm-ostree upgrade`) resolves
it into the next deployment together with the host OS update. Users do not
download or reinstall release RPMs by hand.

If the qOS Forgejo organization is still private, the registry requires
authenticated package-read access. Public qOS organization/package visibility
is the preferred release setup: it avoids credentials at install time and lets
the public repository work with both `dnf` and `rpm-ostree` without any extra
configuration.

For a private registry, Forgejo requires authentication for both the generated
`.repo` file and every later metadata/RPM request. Do not assume that
`dnf config-manager` persists credentials for you. Fedora's DNF5/librepo
repository parser accepts `username=` and `password=` options, so put those
options in the repo stanza rather than embedding the token in `baseurl`.
The following fetches the repo definition with a temporary netrc (the token is
not a command-line argument or shell-history entry), installs it with
root-only permissions, and asks you to add the two options to the generated
stanza (the maintained release file already contains the qOS verification
settings):

```bash
read -r -p 'Forgejo username: ' QOS_FORGEJO_USER
read -r -s -p 'Forgejo read:package token: ' QOS_FORGEJO_TOKEN
printf '\n'
QOS_NETRC=$(mktemp)
QOS_REPO=$(mktemp)
QOS_KEY=$(mktemp)
trap 'rm -f -- "$QOS_NETRC" "$QOS_REPO" "$QOS_KEY"' EXIT
chmod 600 "$QOS_NETRC"
printf 'machine code.podesta.ai login %s password %s\n' \
  "$QOS_FORGEJO_USER" "$QOS_FORGEJO_TOKEN" > "$QOS_NETRC"
QOS_DESKTOP_VERSION=0.1.5
curl --fail --location --netrc-file "$QOS_NETRC" \
  --output "$QOS_REPO" \
  "https://code.podesta.ai/qos/qos/releases/download/desktop-v${QOS_DESKTOP_VERSION}/qos-fedora-44.repo"
curl --fail --location --netrc-file "$QOS_NETRC" \
  --output "$QOS_KEY" \
  "https://code.podesta.ai/qos/qos/releases/download/desktop-v${QOS_DESKTOP_VERSION}/qos-rpm-signing-key.asc"
sudo install -o root -g root -m 0600 "$QOS_REPO" /etc/yum.repos.d/qos.repo
sudo install -o root -g root -m 0644 "$QOS_KEY" \
  /etc/pki/rpm-gpg/qos-rpm-signing-key.asc
unset QOS_FORGEJO_USER QOS_FORGEJO_TOKEN
sudoedit /etc/yum.repos.d/qos.repo
```

Inside the repo stanza, add the actual values (not the placeholders) and save:

```ini
username=<Forgejo username>
password=<PAT with read:package>
gpgcheck=1
repo_gpgcheck=0
gpgkey=file:///etc/pki/rpm-gpg/qos-rpm-signing-key.asc
```

Keep the file mode at `0600`; the token is necessarily stored there so future
metadata and RPM fetches can authenticate. After saving, use `sudo dnf install
qos-session` on a mutable Fedora host or `sudo rpm-ostree install qos-session`
on Fedora Atomic. Do not put a PAT in this repository or paste it into a
shared terminal. The Forgejo administrator must enable the package registry,
grant the user read access to the `qos` organization/packages, and keep the
HTTPS reverse proxy's `/api/packages` routes reachable. Without public
organization visibility or those authenticated settings, an install (and
therefore a future `rpm-ostree update`) is expected to fail with HTTP 401/403.
The key URL is intentionally pinned to the release tag used for bootstrap;
keep using that key for ordinary updates. For a signing-key rotation, bootstrap
the replacement key and update `gpgkey=` before publishing RPMs made only with
the new key.

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
`desktop-v*` tags execute it regardless of changed paths, wait for every enabled
native-architecture install test, download the artifacts, require the complete
current output set, and only then
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
at most four compiler jobs. Ordinary changes allocate only that x86-64 lane;
tags and manual runs with `full_native` add the native ARM64 lane and verify its
Nix closure without QEMU-user.
The Debian-family, Fedora, and Arch jobs reuse the runners' optional remote
compiler cache in an architecture namespace and fail open to an
ordinary build if that service is unavailable. Cache credentials are only
forwarded on trusted branch, tag, and manual runs; pull requests build cold.
The compiler, flags, system headers, and environment are hashed into each
sccache key, so incompatible distro objects miss while the common Rust graph
can hit. Nix builds remain pure.

The build is not a generic cross-compile: Cargo and the target package manager
both execute inside the selected Ubuntu, Debian, Fedora, Arch, or Nix userspace,
so dependency resolution, ELF linkage, package metadata, installation, and
`ldd` verification all exercise the target ABI. The runner must expose Docker
and allow ordinary Docker create/copy/export/start operations. The deferred
cross-architecture path additionally needs unprivileged `ptrace`. RPM tag
publication additionally requires protected `QOS_RPM_SIGNING_KEY_B64` and
`QOS_RPM_SIGNING_KEY_ID` secrets plus the protected
`QOS_RPM_SIGNING_FINGERPRINT` variable. The key must be an unencrypted armored
private key because non-interactive `rpmsign` cannot prompt for a passphrase;
the signing workflow imports it only into a mode-0700 temporary keyring and
never publishes it. If policy requires a passphrase-protected key, add an
approved RPM-signing credential-broker implementation before enabling release
publication. The existing `REGISTRY_TOKEN` remains the package-upload
credential and must have repository and package write permission for tag
releases.

Every distro job first checks the architecture visible inside its container,
builds inside its target distribution, and then installs both the main and
developer packages with that distribution's package manager. The Nix job builds
the flake, checks every installed path, immutable store substitution, native ELF
architecture and shared-library closure, and exports the complete store closure
as a `.nar.gz`. Tagged release assets therefore contain only artifacts that
passed their target-architecture install test.

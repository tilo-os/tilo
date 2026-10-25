# Packaging the Tilo Desktop session

The host package installs Tilo as an additional Wayland session on an existing
Linux system. It is deliberately smaller than the bootc operating-system image:
the package contains the compositor, sandbox launcher, unlock helper, PAM
policy, portal registration, display-manager entry, and AGPL-3.0 license text.
It does not replace the host's boot chain, greeter, or package manager.

The session scans the host's normal XDG application directories, including
per-user and system Flatpak exports. Those existing applications keep their real
home directory and their original confinement model, and Tilo labels them as
unconfined host apps rather than implying they use Tilo package policy. On
Fedora, `xwayland-satellite` is a hard package dependency so legacy X11-only
applications continue to open; native Wayland applications still use the Tilo
socket directly. The complete Tilo image ships the same bridge. Because X11 does
not isolate its clients from each other, every workspace gets its own X server,
display number and authority cookie, started when its first X11 client connects
and stopped when the workspace closes or no X client remains; an app only ever
receives the display of the workspace it opens in.

The wrapper owns `graphical-session.target` for the lifetime of Tilo, clears
display variables left by the preceding desktop before starting it, and removes
Tilo's desktop identity when the session ends. That is what allows portal and
other standard user services to use Tilo while logged in and ensures they stop
before GNOME, KDE, or another session is selected next. The portal frontend and
filtered D-Bus proxy are hard package dependencies, so sandboxed Tilo
applications do not silently lose desktop APIs on minimal hosts.

The package-owned wrapper sends compositor stdout and stderr directly to the
user journal with `SYSLOG_IDENTIFIER=tilo-session`. After returning to another
desktop, startup, portal, application-launch, and crash diagnostics remain
available with `journalctl --user -t tilo-session`; no GDM-specific session-log
path is required. The wrapper records no environment dump or credentials, and
the journal keeps the host's normal per-user/root access controls.

## Supported outputs

| Distribution | Release files                                                    | Architectures                                |
| ------------ | ---------------------------------------------------------------- | -------------------------------------------- |
| Ubuntu 24.04 | `tilo-session_*+ubuntu24.04*.deb`                                | `amd64`; `arm64` when enabled                |
| Debian 13    | `tilo-session_*+deb13*.deb`                                      | `amd64`; `arm64` when enabled                |
| Fedora 44    | `tilo-session-*.rpm`, `tilo-session-devel-*.rpm`                 | `x86_64`; `aarch64` when enabled             |
| Arch Linux   | `tilo-session-*.pkg.tar.zst`, `tilo-session-devel-*.pkg.tar.zst` | `x86_64`; `aarch64` when enabled             |
| NixOS        | flake package and NixOS module; tagged store-closure export      | `x86_64-linux`; `aarch64-linux` when enabled |

The complete ARM64 package definitions and release verifier are enabled by
`TILO_DESKTOP_ARM64_RELEASES`. Tagged and explicit full-native builds use the
native `runner-arm64` pool and publish both architecture sets; ordinary changes
retain one amd64 lane for faster feedback. The single workflow switch remains
the emergency control for temporarily pausing ARM64 scheduling.

`tilo-session-devel` is optional and is only for machines developing Tilo. It
adds a second login entry that can start a user-selected local build after
checking its ownership, directory permissions, and embedded development marker.

## Local builds

Install the relevant
[native dependencies](development/platforms/linux.md#native-dependencies-by-component),
then run one of:

```bash
dist/debian/build-deb.sh
dist/rpm/build-rpm.sh
dist/arch/build-arch.sh       # makepkg must run as a non-root user
nix build .#tilo-session
```

All FHS packages share `dist/stage-payload.sh`; `dist/verify-payload.sh` asserts
the installed paths and the session/portal wiring. Release builds exclude the
test-control socket. Video encode and decode go through the distribution's
GStreamer (a stable 1.x ABI), which picks hardware codecs at runtime.

The package version has one source of truth: `dist/VERSION`. Before tagging,
update it, then create a matching `desktop-vX.Y.Z` tag. Debian and Ubuntu
receive separate package revisions because each binary is linked and
dependency-scanned inside its own distribution build root.

## Install from code.tilo.so

Tagged releases are uploaded both as release assets and to Forgejo's native
package registries under the `tilo` organization. Once the organization is
public, Debian 13 (`trixie`) and Ubuntu 24.04 (`noble`) users can add the apt
repository with:

```bash
sudo install -d -m 0755 /etc/apt/keyrings
sudo curl -fsSL https://code.tilo.so/api/packages/tilo/debian/repository.key \
  -o /etc/apt/keyrings/forgejo-tilo.asc
echo "deb [signed-by=/etc/apt/keyrings/forgejo-tilo.asc] https://code.tilo.so/api/packages/tilo/debian noble main" \
  | sudo tee /etc/apt/sources.list.d/tilo.list
sudo apt update
sudo apt install tilo-session
```

Use `trixie` instead of `noble` on Debian 13. Fedora 44 users must first
bootstrap the RPM release key and maintained repo definition from the exact
`desktop-vX.Y.Z` release they are installing. Forgejo's generated RPM repo file
does not advertise a Tilo key or enable package verification; the release
workflow publishes a repo definition with those settings already present:
`https://code.tilo.so/api/packages/tilo/rpm/fedora/44.repo` is the upstream
Forgejo-generated endpoint, retained for diagnostics and private-server
fallbacks only.

```bash
TILO_DESKTOP_VERSION=0.1.7
sudo install -d -m 0755 /etc/pki/rpm-gpg
sudo curl --fail --location \
  --output /etc/pki/rpm-gpg/tilo-rpm-signing-key.asc \
  "https://code.tilo.so/tilo/tilo/releases/download/desktop-v${TILO_DESKTOP_VERSION}/tilo-rpm-signing-key.asc"
sudo curl --fail --location \
  --output /etc/yum.repos.d/tilo.repo \
  "https://code.tilo.so/tilo/tilo/releases/download/desktop-v${TILO_DESKTOP_VERSION}/tilo-fedora-44.repo"
```

`gpgcheck=1` makes DNF/rpm-ostree verify every Tilo RPM. Forgejo's native RPM
registry's generated `.repo` does not publish `repomd.xml.asc`, so the
maintained definition uses `repo_gpgcheck=0`; HTTPS protects the unsigned
metadata and the package signature protects the payload. Compare the release
key's full fingerprint with the fingerprint published through an independent
Tilo channel before trusting it. No manual repo-file edit is needed on the
public path.

On a traditional mutable Fedora system, install from the configured repo with:

```bash
sudo dnf install tilo-session
```

On Fedora Atomic desktops such as Silverblue, Kinoite, or an rpm-ostree-based
derivative, use the same configured repo and layer the package into the next
deployment instead:

```bash
sudo rpm-ostree install tilo-session
```

Save your work and reboot into the prepared deployment. GDM will continue to
offer the existing desktop and will also offer Tilo in its session menu. The
layering transaction preserves other requested and locally installed packages;
applications installed system-wide or for your user, including Flatpaks, remain
installed and discoverable from Tilo under **Apps**. Select the existing desktop
in GDM whenever you want to switch back. Removing Tilo later uses
`sudo rpm-ostree uninstall tilo-session` and another reboot. Do not use the
development subpackage for an ordinary installation.

If a later host or Tilo deployment is unhealthy, choose the previous deployment
from the boot menu or run `sudo rpm-ostree rollback`, then reboot. Rollback and
uninstall change only the immutable deployment/package layer; the existing home
directory, layered applications, and Flatpak data remain untouched.

When this repository publishes a newer `tilo-session`, the normal
`sudo rpm-ostree update` command (an alias for `rpm-ostree upgrade`) resolves it
into the next deployment together with the host OS update. Users do not download
or reinstall release RPMs by hand.

The version-locked `tilo-session-apps` companion carries the `app` command, the
official Store registry owner pin under `/usr/share/tilo/trust`, and the signed
runtime publisher policy. Opening Store or running `app repo list` repairs an
older `registry = []` configuration from that packaged authority; it preserves
custom registries and explicitly disabled entries. The update must come from the
verified desktop package release. Do not copy a key from the catalogue into the
host's trust policy.

A host with Tilo RPMs listed under `requested-local-packages` has a local
package pin, which ordinary repository updates do not replace. For a local
source installation, `dist/rpm/host-update.sh --no-build` replaces the matching
session/apps packages in one transaction using RPMs already built for that
checkout. It also synchronizes the packaged runtime publisher policy into the
pending deployment. This operation changes trust policy; review the exact
packaged keys before applying it. Without `--apply-live`, the active desktop
continues running until a planned boot. Do not use this source-build helper as a
substitute for a signed public release.

If the Tilo Forgejo organization is still private, the registry requires
authenticated package-read access. Public Tilo organization/package visibility
is the preferred release setup: it avoids credentials at install time and lets
the public repository work with both `dnf` and `rpm-ostree` without any extra
configuration.

For a private registry, Forgejo requires authentication for both the generated
`.repo` file and every later metadata/RPM request. Do not assume that
`dnf config-manager` persists credentials for you. Fedora's DNF5/librepo
repository parser accepts `username=` and `password=` options, so put those
options in the repo stanza rather than embedding the token in `baseurl`. The
following fetches the repo definition with a temporary netrc (the token is not a
command-line argument or shell-history entry), installs it with root-only
permissions, and asks you to add the two options to the generated stanza (the
maintained release file already contains the Tilo verification settings):

```bash
read -r -p 'Forgejo username: ' TILO_FORGEJO_USER
read -r -s -p 'Forgejo read:package token: ' TILO_FORGEJO_TOKEN
printf '\n'
TILO_NETRC=$(mktemp)
TILO_REPO=$(mktemp)
TILO_KEY=$(mktemp)
trap 'rm -f -- "$TILO_NETRC" "$TILO_REPO" "$TILO_KEY"' EXIT
chmod 600 "$TILO_NETRC"
printf 'machine code.tilo.so login %s password %s\n' \
  "$TILO_FORGEJO_USER" "$TILO_FORGEJO_TOKEN" > "$TILO_NETRC"
TILO_DESKTOP_VERSION=0.1.7
curl --fail --location --netrc-file "$TILO_NETRC" \
  --output "$TILO_REPO" \
  "https://code.tilo.so/tilo/tilo/releases/download/desktop-v${TILO_DESKTOP_VERSION}/tilo-fedora-44.repo"
curl --fail --location --netrc-file "$TILO_NETRC" \
  --output "$TILO_KEY" \
  "https://code.tilo.so/tilo/tilo/releases/download/desktop-v${TILO_DESKTOP_VERSION}/tilo-rpm-signing-key.asc"
sudo install -o root -g root -m 0600 "$TILO_REPO" /etc/yum.repos.d/tilo.repo
sudo install -o root -g root -m 0644 "$TILO_KEY" \
  /etc/pki/rpm-gpg/tilo-rpm-signing-key.asc
unset TILO_FORGEJO_USER TILO_FORGEJO_TOKEN
sudoedit /etc/yum.repos.d/tilo.repo
```

Inside the repo stanza, add the actual values (not the placeholders) and save:

```ini
username=<Forgejo username>
password=<PAT with read:package>
gpgcheck=1
repo_gpgcheck=0
gpgkey=file:///etc/pki/rpm-gpg/tilo-rpm-signing-key.asc
```

Keep the file mode at `0600`; the token is necessarily stored there so future
metadata and RPM fetches can authenticate. After saving, use
`sudo dnf install tilo-session` on a mutable Fedora host or
`sudo rpm-ostree install tilo-session` on Fedora Atomic. Do not put a PAT in
this repository or paste it into a shared terminal. The Forgejo administrator
must enable the package registry, grant the user read access to the `tilo`
organization/packages, and keep the HTTPS reverse proxy's `/api/packages` routes
reachable. Without public organization visibility or those authenticated
settings, an install (and therefore a future `rpm-ostree update`) is expected to
fail with HTTP 401/403. The key URL is intentionally pinned to the release tag
used for bootstrap; keep using that key for ordinary updates. For a signing-key
rotation, bootstrap the replacement key and update `gpgkey=` before publishing
RPMs made only with the new key.

For Arch or Arch Linux ARM, add this repository to `/etc/pacman.conf`:

```ini
[tilo.stable.code.tilo.so]
SigLevel = Required
Server = https://code.tilo.so/api/packages/tilo/arch/stable/$arch
```

Import and locally trust the registry key once, then install normally:

```bash
curl -fsSL https://code.tilo.so/api/packages/tilo/arch/repository.key -o tilo-forgejo.gpg
sudo pacman-key --add tilo-forgejo.gpg
sudo pacman-key --lsign-key 'tilo@noreply.code.tilo.so'
sudo pacman -Sy tilo-session
```

While the organization remains private, use a Forgejo username and personal
access token in the repository URL. Do not put a token in this repository.

## NixOS

The flake exposes both a package and a module:

```nix
{
  inputs.tilo.url = "git+https://code.tilo.so/tilo/tilo";
  outputs = { nixpkgs, tilo, ... }: {
    nixosConfigurations.my-machine = nixpkgs.lib.nixosSystem {
      modules = [
        tilo.nixosModules.tilo-session
        { services.tilo-session.enable = true; }
      ];
    };
  };
}
```

The module registers the display-manager session, PAM policy, and portal. Using
the package alone is useful for nested development, but a real login session
should use the module so `/etc/pam.d/tilo-unlock` is activated declaratively.
Tagged builds also publish an exact Nix store closure as a generic Forgejo
package for disaster recovery; normal installations should use the flake so Nix
can build or substitute the package declaratively.

## CI and publishing

`.forgejo/workflows/release-desktop.yml` is the release gate. Pull requests that
touch packaging or the packaged binaries execute the current x86-64 matrix.
`desktop-v*` tags execute it regardless of changed paths, wait for every enabled
native-architecture install test, download the artifacts, require the complete
current output set, and only then publish the apt, RPM, pacman, and generic Nix
registries plus a checksummed Forgejo release. The five target userspaces run
sequentially in one capacity-bounded lane.

The workflow uses the existing `runner` Forgejo label. amd64 containers run
directly, and every target userspace is pinned to a verified OCI digest rather
than a mutable distribution tag. The `dist/ci` runner can execute other
architectures through a job-private PRoot and checksum-pinned QEMU without
registering a host binfmt handler, but that mechanism is not the release gate:
the Nix closure must be validated on a native ARM64 runner first. The package
matrix runs as one sequential x86-64 lane; native Cargo builds use at most four
compiler jobs. Ordinary changes allocate only that x86-64 lane; tags and manual
runs with `full_native` add the native ARM64 lane and verify its Nix closure
without QEMU-user. The Debian-family, Fedora, and Arch jobs reuse the runners'
optional remote compiler cache in an architecture namespace and fail open to an
ordinary build if that service is unavailable. Cache credentials are only
forwarded on trusted branch, tag, and manual runs; pull requests build cold. The
compiler, flags, system headers, and environment are hashed into each sccache
key, so incompatible distro objects miss while the common Rust graph can hit.
Nix builds remain pure.

The build is not a generic cross-compile: Cargo and the target package manager
both execute inside the selected Ubuntu, Debian, Fedora, Arch, or Nix userspace,
so dependency resolution, ELF linkage, package metadata, installation, and `ldd`
verification all exercise the target ABI. The runner must expose Docker and
allow ordinary Docker create/copy/export/start operations. The deferred
cross-architecture path additionally needs unprivileged `ptrace`. RPM tag
publication additionally requires an approved isolated broker and a protected
`TILO_RPM_SIGNING_FINGERPRINT` variable. The separate protected-tag signing job
uses native Forgejo OIDC; package build/install-test userspaces receive neither
OIDC nor a private key. Only the isolated service holds a dedicated compatible
OpenPGP authority. The runner receives signed copies and public provenance,
verifies their signatures and identical immutable headers/payloads, and retains
both signing and host-session gates. See
[the broker protocol and exact separate live approval boundary](desktop-rpm-broker.md).
No private key belongs in a repository-wide Actions secret. The existing
`REGISTRY_TOKEN` remains the package-upload credential and must have repository
and package write permission for tag releases.

Every distro job first checks the architecture visible inside its container,
builds inside its target distribution, and then installs both the main and
developer packages with that distribution's package manager. The Nix job builds
the flake, checks every installed path, immutable store substitution, native ELF
architecture and shared-library closure, and exports the complete store closure
as a `.nar.gz`. Tagged release assets therefore contain only artifacts that
passed their target-architecture install test.

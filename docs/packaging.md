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
| Ubuntu 24.04 | `tilo-session_*+ubuntu24.04*.deb`                                | `amd64`, plus `arm64` when both checks are dispatched |
| Debian 13    | `tilo-session_*+deb13*.deb`                                      | `amd64`, plus `arm64` when both checks are dispatched |
| Fedora 44    | `tilo-session-*.rpm`, `tilo-session-devel-*.rpm`                 | `x86_64`, plus `aarch64` when both checks are dispatched |
| Arch Linux   | `tilo-session-*.pkg.tar.zst`, `tilo-session-devel-*.pkg.tar.zst` | `x86_64`, plus `aarch64` when both checks are dispatched |
| NixOS        | flake package and NixOS module; tagged store-closure export      | `x86_64-linux`, plus `aarch64-linux` when both checks are dispatched |

Both architectures build and install-test when their checks are dispatched:
`desktop-package-amd64` on an x86_64 node and `desktop-package-arm64` on an
aarch64 node. There is no separate architecture switch; dispatching one check
builds and tests that architecture's userspaces.

Building and install-testing the ARM64 packages is not a release qualification:
`docs/install-contract.json` still lists ARM64 as a planned architecture for
every desktop-session distribution, and only x86-64 is the qualifying public
combination until that decision changes.

`tilo-session-devel` is optional and is only for machines developing Tilo. It
adds a second login entry that can start a user-selected local build after
checking its ownership, directory permissions, and embedded development marker.

## Local builds

Install the native dependencies ([CONTRIBUTING.md](../CONTRIBUTING.md)), then
run one of:

```bash
dist/debian/build-deb.sh
dist/rpm/build-rpm.sh
dist/arch/build-arch.sh       # makepkg must run as a non-root user
nix build .#tilo-session
```

All FHS packages share `dist/stage-payload.sh`; `dist/verify-payload.sh` asserts
the installed paths and the session/portal wiring. Every package, Nix included,
ships `tilo`, the command a terminal reaches the desktop's apps and actions with
(`tilo action list`, `tilo do`, `tilo open`). `dist/verify-cli-shims.sh` refuses a
package whose app command shim has no sidecar, at build and after install. Release builds exclude the
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
publishes a repo definition with those settings already present:
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

### What the package route is tested to do

Fedora 44 on x86-64 is the release-guarded combination
(`docs/install-contract.json`). ARM64 packages build and install-test in tagged
and full-native runs, but no ARM64 combination is release-qualified or publicly
supported.
The packaged session is exercised on that combination by
`dist/session/vm-e2e/run.py`: the signed-route RPMs from a release are installed with dnf
on a stock Fedora 44 Cloud image, a person logs in to Tilo through the display
manager's session menu, and the way back is taken as well — the same packages
installed over a running session, signing out to the desktop the host had
before Tilo, and `dnf remove` of only the Tilo packages. The run fails if
anything but those packages and their own session entry changes, if the previous
desktop stops being offered or startable, or if the login user's files differ
before and after. `dist/session/README.md`, "VM test", lists the individual
checks; the checks are the record of the tested distribution, architecture,
package build and session behaviour.

### When the layering is refused

An Atomic host owns most of `/usr`, so `rpm-ostree install` accepts the layer
only if every file in it is either new or already Tilo's. Re-run the same
command against a host that already layers an older release, or a host whose
packages ship a file at one of Tilo's paths, and the transaction is rejected,
but the reason appears as a file-exists error in the middle of a deployment
log. Answer it before the next transaction, without staging anything:

```bash
rpm-ostree status                              # is the session already layered?
sudo dist/rpm/ostree-package-conflicts.sh      # which path, and who owns it
```

`dist/rpm/ostree-package-conflicts.sh` lists each payload path whose current
owner is not a Tilo package, prints the owner, and exits non-zero. It installs
nothing and works before Tilo is present at all, reading an unpacked payload
from `dist/rpm/build/RPMS` when this checkout has built one. It gives two
answers:

- **`tilo-session` is already layered.** Layering is not an upgrade path. The
  next release arrives with `sudo rpm-ostree update`, which refreshes the
  existing layering instead of adding a second one. Moving to a specific
  release from an older layering is `sudo rpm-ostree uninstall tilo-session &&
  sudo rpm-ostree install tilo-session`.
- **Another package owns the path.** A host package that installs its own copy
  of a Tilo file (`/usr/lib/systemd/user/tilo-graphical-session.target` is the
  usual one) cannot coexist with the package. Remove that package, or use the
  complete Tilo image, which owns the path itself.

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

`desktop-preflight` runs the packaging preflight contracts. `desktop-package-{amd64,aarch64}`
builds and install-tests every distribution userspace, one after another in a
single job per architecture, and is dispatched rather than run on an ordinary
change. The Fedora job also emits the small deterministic OS payload the OS
composition downloads; `publish-desktop-payload` publishes it and advances the
`TILO_DESKTOP_PAYLOAD_*` channel variables. Distribution packages are built and
validated on t; t does not publish them to a package registry.

`package-arch` and `package-nix` build one distribution's package each, directly
in an image of that distribution: `makepkg` reads `dist/arch/PKGBUILD` in an Arch
image, and `nix build` reads `dist/nix/package.nix` in a Nix image, so neither
needs a nested container engine. Each check runs the same install test as the
full job and keeps what it built in its outputs, with a `SHA256SUMS` beside it:
`t dispatch package-arch` then `t outputs package-arch` gives the
`.pkg.tar.zst` files, and `package-nix` the store closure as
`tilo-session-amd64.nar.gz`.

The check runs on a native node for its architecture. amd64 containers run
directly, and every target userspace is pinned to a verified OCI digest rather
than a mutable distribution tag. `desktop-package-{arch}` runs all five
distribution targets for that architecture as one sequential job on a native
node; `desktop-package-arm64` runs on an aarch64 node, and its Nix closure is
validated there rather than under QEMU-user. A payload that can be published
never compiles through a shared compiler cache, so the job builds cold with the
compiler, flags, system headers and environment its own userspace fixes. Nix
builds remain pure.

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
both signing and host-session checks. See
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

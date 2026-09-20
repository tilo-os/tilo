# Frozen Windows guest filesystem

This is a build contract, not a promoted release artifact. No guest snapshot or
buildroot digest has been selected here, and these new recipes have not yet been
built. DXVK prefix activation/native DLL overrides are still required before OS
integration. Full Wine/Proton, graphics, sandbox and hardware acceptance remain
separate gates.

## Complete transaction, empty filesystem

`os/dev/windows-guest-packages.txt` declares the runtime package roots. Wine and
Mesa/Vulkan have explicit x86-64 and i686 roots. Both Wine metapackages preserve
Gecko, Mono, fonts, audio and other packaged functionality through normal
dependencies; ALSA is explicit as well. Python, shell tools, locale sources and
the English language pack belong to the runtime rather than a diagnostic SDK.

The OS `RPMS_REF` is a transaction delta against its bootc base. It lacks i686
packages and can omit Python or shell tools that already exist in the base.
It must not be reused as a complete guest repository.

The guest resolver starts with an **empty installroot**. DNF resolves every
dependency, including packages already present in its native buildroot. The
scratch snapshot contains the downloaded RPMs, SHA-256 inventory, package/license
inventory, exact roots, Fedora version, resolver buildroot digest and transaction
model. Snapshot creation verifies Fedora RPM signatures using the Fedora 44 key
provided by the pinned buildroot. The installed guest retains packaged license
files; `tsflags` is explicitly cleared so inherited `nodocs` cannot remove them.

## Native buildroot and immutable inputs

The download-only resolver runs natively on x86-64 or ARM64 with explicit
`--forcearch=x86_64`. It executes no guest scriptlets. The consumer runs natively
on x86-64 and executes normal guest RPM scriptlets. The pinned Fedora 44 buildroot must
already provide DNF5, RPM/rpmkeys, createrepo_c, Python 3, coreutils and Fedora
44 repository configuration and signing keys. Creating and reviewing that
toolchain image is a separate preparation step. Every build reference must
include a complete OCI SHA-256 digest. Use the same immutable multiarchitecture
buildroot index for both phases, with reviewed native ARM64 and x86-64 children
when resolving on ARM. The contract records that index digest. The repo-only
scratch output is explicitly `linux/amd64`, as are both consumer input stages
and its output. The resolver's final stage executes nothing. Tags are rejected;
neither job registers host binfmt handlers.

Resolution is the only network-enabled phase:

```sh
podman build \
  --build-arg WINDOWS_GUEST_BUILDROOT_REF="$WINDOWS_GUEST_BUILDROOT_REF" \
  -f os/Containerfile.windows-guest-rpms -t localhost/tilo-windows-guest-rpms .
```

Record and authenticate the resulting immutable snapshot digest before using
it as `WINDOWS_GUEST_RPMS_REF`. Do not invent a digest, overwrite the OS snapshot
pin, or promote this result merely because dependency download succeeded.

The consumer imports no filesystem from the buildroot. It installs the frozen
transaction into a new empty root with normal RPM scriptlets. Its RUN instruction
uses `--network=none`, and `reposdir=/repo` hides the host's network repositories:

```sh
# Before entering the offline build phase, materialize only these pinned assets.
bash os/dev/cache-github-assets.sh dxvk-2.7.1.tar.gz dxvk-c3dd74b-LICENSE

podman build --platform=linux/amd64 --network=none \
  --build-arg WINDOWS_GUEST_BUILDROOT_REF="$WINDOWS_GUEST_BUILDROOT_REF" \
  --build-arg WINDOWS_GUEST_RPMS_REF="$WINDOWS_GUEST_RPMS_REF" \
  --build-context github-assets=os/external-assets/cache \
  -f os/Containerfile.windows-guest -t localhost/tilo-windows-guest .
```

OCI inputs and checksum-verified external assets must be available before the offline build. The contract checks reject
a changed root list, changed buildroot digest, missing/extra/modified RPMs or a
base-relative transaction. RPM signatures are checked again during installation.
The installed RPM database and NEVRA/license inventory remain in the result.
RPM headers, rather than just filenames, must identify x86-64, i686 or noarch
binary packages. Duplicate NEVRAs, contradictory filenames and differing RPMs
colliding at one cache filename are rejected. Equal copies in the two potential
DNF cache locations are harmless and deduplicated by verified content.

## Required validation and DXVK activation gap

### Development traditional Wine inputs

The ARM provider requires the separate traditional Wine pair. Its locally built
RPMs are unsigned; a digest-pinned artifact alone does not satisfy DNF package
signature checking. For development, `development-wine-repository.py sign`
checks the pinned pair's package bytes, RPM identities and payload digests,
then signs copies with a fresh development-only GPG key. The private key lives
in a temporary mode-0700 directory outside the exported artifact and is destroyed
on success or failure. The original pair remains unchanged.

```sh
python3 os/dev/development-wine-repository.py sign \
  --pair /wine-paired --pair-reference "$WINE_PAIR_REF" \
  --pin os/dev/wine-source.json --output /signed-wine
```

The output directory must not exist. It contains only signed RPMs, the public
key, the original paired contract and a development trust receipt. Publish it
at `/signed-wine` in an immutable artifact and independently record the receipt's
`public_key_sha256`. Pass both `DEVELOPMENT_WINE_REF` and
`DEVELOPMENT_WINE_KEY_SHA256` to the resolver and offline guest Containerfiles.
The signer needs GnuPG and `rpm-sign`; it neither compiles Wine nor executes it.

The resolver exposes the verified pair as a preferred local repository and
retains its complete RPM inventory even when DNF consumes local files in place.
Normal package roots still determine what gets installed. Both resolver and
consumer verify development Wine RPMs against an isolated development keyring
and every remaining package against an isolated Fedora keyring. Stock packages
cannot replace the pinned traditional Wine identities. `gpgcheck=True` remains
enabled throughout; unsigned digest-only packages are rejected.

The frozen snapshot binds the immutable signed-pair reference and hashes of all
public trust records. The guest preserves those records for audit. These are
development signing identities, not release trust or execution acceptance.

After installation, the build executes the guest's native x86-64 Python standard
library import check and `localedef` generation of `en_US.UTF-8`. It then
runs `validate-windows-guest.py`. That gate requires both Wine/server ELF widths,
Python, locale helpers and sources, both graphics loader/driver widths, and
D3D8/D3D9/D3D10Core/D3D11/DXGI DXVK DLLs as correctly matched PE32 and PE32+ files.

Fedora's currently inspected DXVK 2.7.1-6.fc44 packages are x86-64 only. Inspection
of the installed DLL headers confirmed that the D3D11/DXGI payloads are PE32+;
the x86-64 package designation does not itself prove payload bitness. No fictional
`wine-dxvk.i686` dependency is added. Fedora DXVK packages may still appear via
Wine's weak dependencies; those normal package relationships are preserved.

The separately pinned upstream `dxvk-2.7.1.tar.gz` supplies both PE architectures
from one release. Its exact-commit license is independently pinned in
`github-assets.tsv`. `install-windows-guest-dxvk.py` verifies both hashes,
rejects unexpected/duplicate/linked archive members, checks all ten DLL headers,
and imports them under `/usr/share/tilo/dxvk/{x32,x64}`. It records source URLs,
hashes and DLL metadata and preserves the license under
`/usr/share/licenses/tilo-dxvk`. It never writes Wine's builtin paths or changes
alternatives: upstream native DLLs lack Fedora's Wine-builtin marker. Runtime
prefix installation and native DLL overrides must be implemented and tested
before this provider is enabled. Artifact presence alone does not activate DXVK.

The output preserves the **complete validated installroot**, including `/opt`,
RPM state, license files and package-created paths. There is no generic pruning
after validation. Any required FEX-specific runtime overlay belongs to a separate
assembly policy with its own explicit exclusions and acceptance tests. The
scratch artifact exposes `/rootfs`; final ARM OS wiring and artifact promotion
remain pending. The guest SDK is a separate build input and is never exported
as the guest runtime.

## Local checks

The native ARM resolver was exercised against Fedora 44 metadata using the
declared roots, an empty installroot and `--assumeno`: DNF resolved 885 packages
(approximately 1 GiB compressed). This proves dependency resolution, not an
offline installation. The pinned DXVK archive was imported separately:
all ten DLL headers passed (five per architecture), with its 1,074-byte license
preserved. The 31 contract, payload and importer tests passed on Linux.

```sh
python3 os/tests/test_windows_guest_contract.py
python3 os/tests/test_windows_guest_payload.py
python3 os/tests/test_windows_guest_dxvk.py
bash -n os/dev/resolve-windows-guest-rpms.sh os/dev/build-windows-guest.sh
```

Contract tests check stale/mutated snapshots and package-list errors without
pretending synthetic RPM bytes constitute a dependency-resolution test. Real
acceptance requires successful offline installation from the frozen snapshot,
both guest architectures under native ARM FEX, and complete generated capsule
execution with normal network and filesystem restrictions.

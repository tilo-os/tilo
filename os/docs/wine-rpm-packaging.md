# Traditional Wine RPM inputs for Linux FEX

This is a build contract, not a released or execution-validated Wine runtime.
No frozen Wine buildroot digests or completed RPM artifacts exist yet.

## Why a separate build

The signed Fedora `wine-11.0-3.fc44.src.rpm` defaults `new_wow64` on for
Fedora 43 and later. Its x86-64 build uses `--enable-archs=i386,x86_64` and
`/usr/lib64/wine-wow64`. The spec's `--without new_wow64` branch instead
builds the traditional layout and creates reciprocal 32/64 Unix and PE
directory links. Tilo uses that existing option; it does not patch upstream
source or add substitute loader links after building.

Wine 11's [loader.c](https://raw.githubusercontent.com/wine-mirror/wine/wine-11.0/dlls/ntdll/unix/loader.c)
looks for the alternate Unix loader beneath the same Wine library directory.
Its [configure.ac](https://raw.githubusercontent.com/wine-mirror/wine/wine-11.0/configure.ac)
also supports paired `--enable-win64` / `--with-wine64` builds and requires
the same library directory. This contract retains Fedora's traditional
multilib packaging strategy with one hash-pinned file-list correction.

### Wine 11 TWAIN packaging correction

Wine 11 `configure.ac` defaults `enable_twain_32` to `i386`; `twaindsm` and
SANE build independently. Fedora's pinned spec nevertheless lists
`twain_32.dll` for every PE architecture, which makes traditional x86-64 RPM
packaging fail after compilation. `prepare_spec` changes only this file-list
entry: i686 retains it, traditional x86-64 omits it, and new-WoW64 x86-64
lists its actual i386 path. No module, source patch, BuildRequires or build
option is removed.

`wine-source.json` separately pins the authenticated upstream spec and the
derived spec SHA-256. Resolution retains both specs, a derivation record and
the derived source RPM; dependency resolution and the offline build consume
that same derived RPM. The signed Fedora source remains intact and its
signature does not extend to our derived RPM. The adapter verifies both spec
hashes, the derivation binding, source RPM bytes and embedded derived spec,
then checks the rebuilt output source RPM's spec again. Failed runs retain
the preparation evidence. The derived SRPM hash is recorded per resolution;
only the spec transformation is claimed deterministic.

FEX commit `91bbce5a96b9e1eaa32f95f440ea8b46bbfbdf20` fixes decoder execution
width within a process. The existing Fedora new-WoW64 runtime passed matching
Wine32/PE32 and Wine64/PE64 DXVK loads, but failed PE32 startup inside Win64,
even for a console executable without DXVK. Forcing `WINEARCH=wow64` crashed.
The required candidate therefore uses Wine's separate Unix loader processes.
This reasoning does not replace actual cross-width execution tests.

## Resolve and freeze buildroots

Acquire the pinned source in disposable native x86-64 Fedora 44 tooling with
`python3 os/dev/fetch-wine-source.py NEW_DIRECTORY`. DNF resolves the exact
source NEVRA through the configured Fedora source repositories. The helper
checks the installed Fedora key against its pinned hash, then verifies the
downloaded source size, hash and RPM signature in an isolated key database.
It retains acquisition/signature logs and publishes only verified inputs to
a new directory. Failed attempts retain their staging directory and logs.
This acquisition step does not resolve or build either target environment.

t's `wine-source` check runs this stage (`tools/ci/publish/wine.sh`), on the
`image-wine-tools` image, and keeps the signed source and acquisition evidence as its
outputs.

### CI producer: locked Mock builds

The workflow builds both targets on native x86-64 hardware, one at a time.
`wine-mock-producer.py resolve` uses Mock's supported
`--calculate-build-dependencies` and `mock-hermetic-repo` lifecycle to retain
the full dependency repository, bootstrap image, configuration and lockfile.
The i686 target selects Fedora's `fedora-44-i386` configuration rather than
trying to turn an x86-64 dependency root into i686 with RPM macros alone.

`run-wine-ci.sh` publishes that complete resolution bundle under a run-specific
candidate OCI reference. It then starts a fresh privileged build container
with networking disabled, separate Mock/container storage, and the resolved
bundle mounted read-only. Mock's supported `--hermetic-build` creates the
target root from that bundle and performs the normal build. Source, target,
macros, concurrency, runtime image, tool hashes and installed tool packages
must match resolution. Both phases retain the traditional Wine option and
the Tilo release suffix. No feature, dependency, debug package or scriptlet is
removed to make the build pass.

The artifact records `mock-hermetic` provenance separately from the older
prepared-buildroot OCI producer below: the tooling image is not described as
the target buildroot. The adapter checks the actual output RPM identities,
loader bytes and layout, preserves both source RPMs and all logs, and binds
the output to the resolution manifest and lock. The complete dependency
snapshot remains available by its immutable OCI reference. Both references
are retained in CI evidence; production pins and release signatures remain
separate promotion steps. Passing producer tests alone does not prove that
Mock integration, compilation or runtime acceptance succeeded.

The final CI job independently revalidates both architecture artifacts and
merges their shared noarch packages only after semantic equivalence checks.
It publishes a run-specific paired candidate and retains its immutable
reference and contract. This does not sign the RPMs or select a production
Windows guest image.

### Alternative: prepared buildroot OCI inputs

Use native x86 hardware. Prepare separate Fedora 44 target buildroots for
`x86_64` and `i686`, retaining the complete spec-derived BuildRequires.
An i686 target can run on an x86-64 host; an OCI `linux/386` label is not
required. The contract compiles a small native program and checks its ELF
class/machine with the selected compiler and RPM flags.

Fedora's [i686 Mock configuration](https://raw.githubusercontent.com/rpm-software-management/mock/main/mock-core-configs/etc/mock/fedora-44-i386.cfg)
permits x86-64 hosts. Its included `fedora-branched.tpl` uses the Koji build
repository for i686, because Fedora's release mirrors do not carry its full
build environment. Use that target repository policy, not a made-up i386
release URL or an ARM root with only `--forcearch` changed. Freeze the exact
repository metadata/configuration, RPM files and hashes used to create each
root. Koji build RPMs may be unsigned: authenticate and retain their snapshot
identity separately, and do not claim Fedora release-signature coverage for
them. Publish each complete prepared root by OCI digest.

Inside each disposable root, provide Python, RPM build tools, DNF5 builddep,
Fedora RPM macros, gcc/g++, and the compiler/development environment for its
target. Mount `/tilo` containing `wine-source.json`, `wine_rpm_contract.py`,
and `prepare-wine-buildroot.sh`. Mount `/wine-source` containing the exact
source RPM and the Fedora 44 key as `fedora-44.asc`. Both byte identities
are pinned. Set `WINE_TARGET`, then run the preparation script. It checks
the signature using an isolated key database, verifies the unchanged spec,
resolves conditional BuildRequires from that spec, and records all installed
RPM identities, compiler identity, target flags and BuildRequires digest.
Builddep conditions do not apply to SRPM inputs; the script deliberately
passes the extracted spec. Preserve the signed source and resolution logs.

Do not start a local build with only 18 GiB free. Source members alone total
319,673,544 bytes; there are roughly 75 declared build requirements, including
both MinGW toolchains and graphics/audio/media development libraries. A
15–25 GiB planning allowance for both expanded buildroots, objects, debug RPMs
and container layers is provisional. Measure the resolved dependency set and
available scratch capacity before scheduling builds, preferably on CI.

## Offline consumption

For each target, pass its full immutable `WINE_BUILDROOT_REF`, `WINE_TARGET`,
and a `wine-source` named context to `os/Containerfile.wine-rpms`. The only
build RUN has network disabled. The consumer compares the installed package
and compiler inventory with the frozen buildroot contract, checks the source
again, then invokes normal `rpmbuild -ba --without new_wow64` on the unchanged
extracted spec. Its existing `Release: 3%{?dist}` receives the pinned
`dist=.fc44.tilo_traditional1` macro, yielding `3.fc44.tilo_traditional1` for
every subpackage and its exact-version internal dependencies. The source
timestamp comes from the same pin. A matching rebuilt source RPM accompanies
the binaries; the original Fedora-signed source remains separate provenance.
Dependency checking, staging patches, scriptlets, documentation, licenses and
debug artifacts remain enabled. `BUILD_JOBS` limits concurrency only.

Each output contains every binary RPM, the original signed SRPM, complete
build log and per-target hash/identity metadata. Core RPM headers must expose
the expected normal loader files and Fedora's exact alternate-architecture
links; new-WoW64 paths fail validation. The actual loader bytes must also have
the correct ELF class/machine. These checks do not prove full runtime
dependency closure, successful execution or scriptlet success.

The Tilo release suffix distinguishes both outputs from Fedora's new-mode
NEVRAs. The complete guest transaction must still select this pinned provider
explicitly, including all matching exact-version dependencies. Publication
and signing policy is still needed for these locally rebuilt RPMs; no signature
bypass is authorized for unrelated Fedora runtime packages.

## Pairing noarch outputs

After publishing both architecture outputs immutably, run
`python3 os/dev/merge-wine-rpms.py --x64 X64_ARTIFACT --x86 I686_ARTIFACT
--x64-ref REGISTRY/X64@sha256:DIGEST --x86-ref REGISTRY/I686@sha256:DIGEST
--pin os/dev/wine-source.json --output NEW_DIRECTORY`.
Both extracted inputs are revalidated against their inventories and the same
source/build identity before any output is written.

Both builds must contain the same complete noarch package set. The merger
checks RPM integrity and compares all semantic header fields, including file
digests, modes, owners, flags, dependencies, scriptlets and triggers. Only
explicit RPM encoding/signing/build-machine bookkeeping is ignored. Unknown
fields are compared by default. Differences fail the merge for investigation;
there is no architecture-based feature pruning or preferred-package fallback.
After equivalence is established, x86-64 owns the shared noarch RPM bytes.
All architecture-specific packages and both complete provenance directories
are retained. `paired-contract.json` records both input digests and the selected
bytes. This stage does not sign RPMs or publish repository metadata.

## Required acceptance before integration

Install the complete paired RPM set with normal scriptlets in the frozen
guest build. Validate both loader ELF architectures and reciprocal link
resolution within that guest, licenses, package inventory and all existing
graphics/audio interfaces. Run Wine32/PE32, Wine64/PE64, and Wine64/PE32 in
the same Win64 prefix using the managed helper. Require all five native DXVK
DLLs, matching hashes and exports, with PE32 in Win64 loading from syswow64.
Also run 32-to-64 and 64-to-32 child creation in that same prefix under Linux
FEX and the generated capsule. Keep existing sandbox restrictions. GPU,
audio, Proton and full Landlock acceptance remain independent requirements.

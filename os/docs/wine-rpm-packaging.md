# Traditional Wine RPM inputs for Linux FEX

This is a build contract, not a released or execution-validated Wine runtime.
No frozen Wine buildroot digests or completed RPM artifacts exist yet.

## Why a separate build

The signed Fedora `wine-11.0-3.fc44.src.rpm` defaults `new_wow64` on for
Fedora 43 and later. Its x86-64 build uses `--enable-archs=i386,x86_64` and
`/usr/lib64/wine-wow64`. The spec's `--without new_wow64` branch instead
builds the traditional layout and creates reciprocal 32/64 Unix and PE
directory links. qOS uses that existing option; it does not patch upstream
source or add substitute loader links after building.

Wine 11's [loader.c](https://raw.githubusercontent.com/wine-mirror/wine/wine-11.0/dlls/ntdll/unix/loader.c)
looks for the alternate Unix loader beneath the same Wine library directory.
Its [configure.ac](https://raw.githubusercontent.com/wine-mirror/wine/wine-11.0/configure.ac)
also supports paired `--enable-win64` / `--with-wine64` builds and requires
the same library directory. For this contract we retain Fedora's spec and
its existing traditional multilib packaging strategy.

FEX commit `91bbce5a96b9e1eaa32f95f440ea8b46bbfbdf20` fixes decoder execution
width within a process. The existing Fedora new-WoW64 runtime passed matching
Wine32/PE32 and Wine64/PE64 DXVK loads, but failed PE32 startup inside Win64,
even for a console executable without DXVK. Forcing `WINEARCH=wow64` crashed.
The required candidate therefore uses Wine's separate Unix loader processes.
This reasoning does not replace actual cross-width execution tests.

## Resolve and freeze buildroots

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
target. Mount `/qos` containing `wine-source.json`, `wine_rpm_contract.py`,
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
again, then invokes normal `rpmbuild --rebuild --without new_wow64`.
Dependency checking, staging patches, scriptlets, documentation, licenses and
debug artifacts remain enabled. `BUILD_JOBS` limits concurrency only.

Each output contains every binary RPM, the original signed SRPM, complete
build log and per-target hash/identity metadata. Core RPM headers must expose
the expected normal loader files and Fedora's exact alternate-architecture
links; new-WoW64 paths fail validation. The actual loader bytes must also have
the correct ELF class/machine. These checks do not prove full runtime
dependency closure, successful execution or scriptlet success.

Both outputs retain upstream NEVRAs. Never mix them with stock same-NEVRA Wine
RPMs, silently choose duplicate noarch packages, or use metadata alone to
select a provider. A later reviewed merge must compare shared RPM payloads,
build one explicitly pinned repository, and resolve the complete guest
transaction against that provider. No signature bypass is authorized for
unrelated Fedora runtime packages. Publication/signing policy is still needed
for these locally rebuilt RPMs.

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

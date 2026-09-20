# Windows providers in the OS image

The OS chooses a complete provider for its native architecture. On x86_64,
Fedora Wine and DXVK continue to come from the frozen OS RPM repository. On
aarch64, native FEX and a complete x86 guest filesystem are required build
inputs. Missing ARM inputs fail the build; there is no Windows feature opt-out.

## ARM build inputs

Set `FEX_PAYLOAD_REF` and `WINDOWS_GUEST_REF` to published OCI references of the
form `registry/repository@sha256:<64 hexadecimal digits>` before running
`os/dev/build-image.sh`. The script checks these before asset downloads. Direct
Containerfile builds also require `TILO_WINDOWS_ARCH=aarch64`; the selected
stage and final runtime installation verify the native architecture.
The x86 guest input is selected as `linux/amd64` and never executed on ARM.

No production references are supplied in this repository yet.
The FEX producer is `os/Containerfile.fex-payload`. The guest producer is
`os/Containerfile.windows-guest`; its complete RPM snapshot must include the
paired Tilo traditional Wine packages described in [Wine RPM packaging](wine-rpm-packaging.md).
Publish/sign those packages and resolve a fresh complete guest transaction
before attempting this OS assembly. The older Fedora new-WoW64 diagnostic root
does not satisfy this contract.

The consumer reruns both payload validators and compares their inventories to
the producers' inventories. It checks the pinned FEX source and portable CPU
settings, native ELF architecture, build provenance, empty-installroot guest
snapshot metadata, and both pinned traditional Wine RPM identities. Wine's
upstream-supported reciprocal Unix loader paths must resolve to executable
i386 and x86_64 loaders matching their public entrypoints. The OS never creates
replacement loader links or patches the upstream Wine layout.

## Layout and native dependencies

FEX's complete `/usr` payload, including thunks, metadata, tools and licenses,
is copied into the native OS. The complete guest root, including its RPM
database and licenses, is copied unchanged into
`/usr/lib/tilo/windows-runners/x86`. Capsules bind that directory at
`/opt/tilo/x86`; the FEX and Steam graphics-provider manifests already use this
sandbox path. ARM Wine capsules expose the guest's `/usr/lib/wine`,
`/usr/lib64/wine` and `/usr/share/wine` resource directories. Native x86 Wine
selection is unchanged.

Architecture annotations in `os/Containerfile` feed the frozen RPM package
derivation. ARM includes FEX's C/C++ runtime, Qt frontend, GL/EGL, Vulkan,
Wayland, DRM and ALSA libraries. After copying the payload, the native dynamic
linker checks every native ELF shipped in its `usr/bin` and `usr/lib64` trees
against the final OS library set. This catches missing libraries and symbol
version requirements; it does not establish optional driver or GPU behavior.
Refresh the ARM OS RPM snapshot after changing these roots.

The assembly record is `/usr/share/tilo/windows/provider.json`. It records both
input digests and validated payload inventories. No global binfmt registration
is introduced. VM fallback, Proton, Steam runtime and UMU assets remain included.

## Acceptance still required

These are packaging gates. A successful build is not proof of mixed-width
Windows execution, Proton namespace startup, graphics rendering or hardware
support. Run Wine32/PE32, Wine64/PE64 and Wine64/PE32 in the same Win64 prefix,
including the managed native DXVK DLL checks, in the generated production
capsules. Validate Proton and native/translated graphics on each target GPU.
The prior diagnostic matching-width successes do not substitute for these
traditional-provider acceptance results. The pinned FEX version also has no
i386 Vulkan guest thunk; the guest's full 32-bit Vulkan/Mesa runtime remains
present and requires its own execution validation.

# Internal installer feasibility evidence and next protocol

Date: 2026-09-16
Status: Read-only investigation; proposed architecture, not validated support.

## Findings

The existing target installer is reusable. USB-free installation still needs a
new boot/storage protocol; mounting the downloaded image does not establish a
boot path.

- `os/dev/build-custom-installer.sh` builds a bootc live whole-disk qcow2,
  with a minimum 40 GiB root. The root accommodates both the running composefs
  tree and a second unpacked image in container storage. The Anaconda ISO path
  was removed. `apps/imager/packaging/package-install-medium.sh` converts the
  qcow2 to a compressed raw whole-disk image, `.img.zst`.
- `os/Containerfile.installer` embeds `qos.install` in the UKI command line.
  `os/dev/disk-image.sh` constructs the partitioned image using systemd-repart
  and bootc. Moving its files or partitions requires proving the root/UKI
  identity bindings; image mounting or extraction alone is insufficient.
- `apps/imager/src/release.rs` authenticates bundled release selection through
  the Imager build signature and verifies compressed and expanded SHA-256
  digests. It already handles HTTP Range resume and exposes `spawn_download`.
  Reuse this transport and trust boundary; add explicit internal-bundle purpose,
  layout and compatibility metadata rather than treating any raw image as an
  internal installation payload.
- `os/usr/libexec/qos-install` validates disk size, device number, optional
  serial/WWN, partition identity and available capacity. Prepared alongside
  requires a clean FAT ESP of at least 2 GiB with 1 GiB available and a root
  partition of at least 32 GiB without an existing filesystem signature. Guided
  alongside creates partitions only in reviewed unallocated space.
- The installer currently rejects **every target on the live medium's physical
  disk**, in `qos-install` near `LIVE_MAJ_MIN` (lines 514–535 at investigation).
  `crates/session/installer/src/disks.rs` also disables live-system disk ancestors
  through `device_tree_has_live_system_mount`. Single-disk internal installation
  needs a narrow protected-partition/non-overlap protocol in both layers. Keep
  the current whole-disk refusal for ordinary removable-media installations.
- `crates/session/installer/src/media_profile.rs` accepts bounded secret-free
  localization and account-name fields. It explicitly rejects unknown fields,
  including destructive target choices. It is useful validation scaffolding,
  not an authorization channel for a reviewed disk plan.
- `os/usr/libexec/qos-install-boot-menu` snapshots/restores BootOrder and BootNext.
  It does not create the proposed one-time internal-installer boot entry.
  Its systemd-boot shortcuts address foreign EFI loaders on the same ESP only;
  installations using a dedicated qOS ESP retain foreign systems in firmware's
  picker. A consistent cross-ESP startup choice remains work to design and test.
- The recovery receipt in `qos-install`, near `write_install_marker` (line 935),
  is published after root formatting and resides in the destination. It cannot
  authenticate or resume earlier partition mutations. An external durable
  transaction and owner recovery mechanism are still missing.
- `docs/security/boot-chain.md` documents unsigned systemd-boot and a locally
  rebuilt kernel: the existing qOS chain does not boot under factory Secure Boot
  keys. Owner enrollment requires firmware setup/key changes. Do not advertise
  generic Secure Boot compatibility or silently change firmware key policy.

## Proposed first proof configuration

Prove one configuration first: UEFI/GPT Linux, Secure Boot already disabled,
adequate existing unallocated space, dedicated internal staging partitions, and
a real retained Linux installation. This is a test target, not an advertised
support row. Do not mutate the active developer VM to construct it.

Use a dedicated staging root and staging EFI area outside the retained OS and
future qOS destination partitions. Stage a verified bootc live root and matching
EFI boot artifacts. Retain the running bootc image and `bootc image
copy-to-storage` installation approach. This initially costs approximately the
live image's 40 GiB root allocation in addition to target space; sparse download
size does not remove the host allocation requirement. A smaller dedicated
installer image can be a later optimization with its own installation-source
contract.

Create an owned UEFI boot option and select it with BootNext, leaving persistent
BootOrder unchanged. Reject an unexpected existing one-time boot request rather
than silently replacing it. UEFI specifies deletion of BootNext before control
transfers and subsequent use of normal boot order; firmware behavior still
requires measurement. Linux and Windows need separate privileged adapters and
readback validation. Windows firmware-variable writes require the documented
system-environment privilege and are unavailable for legacy BIOS installations.

After the boot proof, allow same-disk target work only through a separately
reviewed internal-install mode that revalidates all protected source, staging,
recovery and destination partition identities and extents. An editable setup
JSON file cannot itself authorize these operations. Existing major/minor numbers
are session-local facts and must be reacquired after reboot, not used as durable
disk identifiers.

Secure Boot, encrypted/hibernated Windows storage, storage shrinking and Windows
staging writes are separate validation requirements. An unsupported result must
stop before writes. Do not infer Windows support from a successful Linux UEFI
proof or CPU architecture.

## Next implementation task: boot bundle and round-trip proof

Deliver a versioned internal-boot bundle contract, build/export tooling and a
disposable VM round-trip harness. The acceptance test must start in an actual
retained OS, boot the staged real qOS installer from the same virtual disk, and
then cold-boot the retained OS with unchanged persistent default. It need not
perform target formatting yet.

The authenticated release manifest should bind at least:

- Schema version, bundle purpose, release identity, architecture and required
  firmware mode; explicit Secure Boot compatibility requirements.
- Each component's purpose, exact byte size and digest: staging-root partition
  image, EFI artifacts, and any boot metadata required to locate the root.
- Filesystem and partition requirements, minimum capacities, expected root/UKI
  identity binding, and the owned EFI entry path.
- Supported installer/handoff protocol versions and minimum staging/recovery
  storage requirements. No account or encryption secrets.

The host transaction record is distinct from that release manifest. Design it
to bind operation ID, approved plan digest/revision, durable host/disk identities,
complete reviewed partition geometry, staging identities and component digests,
original firmware entries/order, owned EFI entry identity, and explicit operation
outcomes. Define authentication and the owner-accessible recovery mechanism
before using the record to authorize or resume mutations. Store it outside the
destination and do not trust labels or a progress boolean as proof of ownership.

Implement/test the first lifecycle as stage, verify, arm once, observe installer,
return to retained OS, and collect evidence. Before arming, revalidate payload,
layout and firmware facts. Keep staging and recovery material until both boot
paths have been exercised. Cancellation before mutation may clean up verified
owned work; later failure reports the actual completed boundary.

Required proof artifacts are machine-readable before/after partition maps and
identities, hashes of preserved EFI files, before/after firmware state, payload
manifest identity, installer boot evidence, retained-OS boot evidence and failure
outcomes. Inject interruption before and after arming, invalid payload/digest,
changed partition layout, unexpected firmware changes and missing staging data.
Do not report safe automatic disk-mutation resume from these boot-only tests.

Follow-on work is the protected-partition installer mode, authenticated reviewed
handoff, target onboarding BootNext, persistent-default switching and a Windows
adapter/proof matrix. This sequencing supplies the missing protocol to the shared
setup model without reducing the intended complete migration application scope.

## Available tests and exact evidence limits

- `os/dev/test-installer-cycle.sh` supplies QEMU framebuffer/OCR-driven real
  installation and encrypted first-login testing, with prepared/guided alongside
  variants. `os/dev/vm.sh` supplies isolated installer work disks and target boot
  modes.
- The alongside fixture uses Windows/Linux filesystem sentinels and **fake EFI
  loader contents**, created in `prepare_alongside_target` near line 109. This
  proves preservation properties; it does not prove a retained Windows or Linux
  operating system boots afterward.
- Relevant existing shell suites include
  `os/tests/installer_guided_alongside.sh`,
  `os/tests/installer_target_invariants.sh`,
  `os/tests/installer_boot_menu.sh`,
  `os/tests/installer_profile_transport.sh`,
  `os/tests/boot_chain_invariants.sh`, and `os/tests/esp_capacity.sh`.
- Read-only `prlctl list -a` during this investigation listed only the running
  **Fedora 44 ARM64** VM. Its reported configuration has nested virtualization
  disabled and TPM disabled. No Windows VM was listed.
- There was no local `os/output` directory. No local boot artifact was examined
  or booted. No installer test, disk mutation, firmware mutation, VM mutation or
  image build was performed in this investigation. Repository code and comments
  establish available mechanisms, not successful validation of this proposal.
- No USB-free configuration has been proven by this report. Physical firmware,
  real retained Windows boot, Secure Boot, encryption and recovery behavior need
  explicit test evidence before expanding product support claims.

## Primary references

- [UEFI 2.11 Boot Manager](https://uefi.org/specs/UEFI/2.11/03_Boot_Manager.html):
  BootNext consumption, BootOrder and boot-option semantics.
- [Microsoft SetFirmwareEnvironmentVariableW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-setfirmwareenvironmentvariablew):
  required privilege and legacy-BIOS limitations.
- [Unified Kernel Image specification](https://uapi-group.org/specifications/specs/unified_kernel_image/):
  EFI executable/kernel/initrd packaging; packaging does not by itself establish
  the proposed root discovery or trust chain.
- [GNU GRUB loopback booting](https://www.gnu.org/software/grub/manual/grub/html_node/Loopback-booting.html):
  bootloader image access is distinct from operating-system image boot support.

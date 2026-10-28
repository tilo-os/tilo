# Secure Boot and disk encryption on Tilo

Tilo boots UEFI → systemd-boot → a **unified kernel image** → systemd. There is no
shim and no GRUB: the image removes both, and the kernel is built here rather
than taken from Fedora. Every one of those facts changes what Secure Boot can
mean, so this document describes the chain as it is, not as a stock Fedora bootc
image's would be.

**One correction to note if you have read an earlier version of this file.** It
used to describe a bare kernel plus a separate initramfs, and before that a
shim → GRUB chain the image had already deleted. Both were wrong at the time of
writing. The chain is now a UKI on bootc's composefs backend, which changes three
things that matter:

- **The command line is inside the signature.** It used to sit beside the kernel
  as an editable file on the EFI system partition, so any hardening option could
  be stripped by anything that could write there. Sealing it into the UKI is what
  makes `tilo.generation=` trustworthy enough to bind an unlock policy to — see
  [rollback.md](rollback.md).
- **The initramfs is inside the signature too.** Previously it was a plain archive
  the loader handed to the kernel *after* verification, and it is where the
  encrypted root gets unlocked — so anyone able to write the boot partition ran
  code before the root opened, with Secure Boot on and every signature valid.
  That gap is closed: secure boot here now buys a verified *boot*, not merely a
  verified kernel.
- **There is no bootupd.** A sealed image must not carry it, and it is not needed:
  signing is the narrow Rust-native `tilo-crypto sign-efi` operation over one artefact.

Why composefs rather than ostree at all is recorded in
[composefs-decision.md](composefs-decision.md).

## The chain has no vendor signature, and cannot get one

A stock Fedora bootc image inherits a chain that verifies out of the box on a
factory machine: shim is signed by the Microsoft UEFI CA, GRUB by Fedora, the
kernel by Fedora. Tilo ships none of those three. `systemd-boot-unsigned` is, as
the name says, unsigned, and the kernel is a local rebuild
(`os/kernel/Containerfile`) that no distribution key has ever touched.

So on a machine with the factory keys enrolled and Secure Boot on, Tilo does not
boot. That is not a packaging oversight to be fixed by re-adding shim — a
Tilo-signed chain would only move the problem. A vendor key means images *we*
signed are attestable and an image the owner built is not, which is the property
this system is specifically designed to keep.

The answer is that the machine's own key is the root: the owner clears the
factory keys, enrols a Platform Key generated on that machine, and signs the boot
artefacts with it. A collaborator who chooses to trust that key gets an
attestable machine, whether the image came from CI or from a laptop.

`/usr/libexec/tilo-secureboot` implements this. Nothing about it runs during the
image build or automatically on a running machine.

### Enrollment order

Enrolling keys before anything signs the kernel produces a machine whose firmware
refuses its own boot artefacts. `tilo-secureboot enroll` therefore refuses unless

1. the keys exist,
2. **every** artefact the firmware will check already verifies against `db`,
3. `tilo-secureboot-sign.path`, `tilo-secureboot-sign.timer` and
   `tilo-secureboot-sign-shutdown.service` are enabled, so the next OS update
   cannot stage a kernel that nothing signs, and
4. the firmware is in **setup mode** — which is the only state that accepts these
   writes, and a state the owner has to reach deliberately.

`sign` is safe at any time: a signature on an artefact the firmware is not
checking is inert. That asymmetry is why the signing side ships enabled and the
enrolling side does not. A signing pass also requires at least one managed
loader, UKI, or deployment kernel; an absent/unmounted ESP or unknown layout is
a hard failure, never a successful zero-artifact pass.

Three things keep the artefacts signed, in decreasing order of how much they are
relied on:

- **`tilo-secureboot-sign.path`** watches `/boot/EFI/Linux/bootc`, where the
  composefs backend's `bootc upgrade` writes the incoming generation's UKI, so it
  gets signed immediately rather than at the next boot — which would be the boot
  the firmware refuses. A path watch is not recursive, so the unit names that
  subdirectory itself as well as `/boot/EFI/Linux`, where the image build's UKI
  sits; `tilo-secureboot sign` scans both. It also watches
  `/boot/EFI/systemd` and `/boot/EFI/BOOT` for whatever replaces the loaders there,
  a change with no natural second chance. Both mount points are watched
  (`/boot/EFI/…` and `/boot/efi/EFI/…`) because the ESP is at `/boot` on the sealed
  layout and under `/boot/efi` on the ostree one, and `/boot/ostree` still is too,
  for a machine that predates the move.
- **`tilo-secureboot-sign-shutdown.service`** signs once more on the way down.
  The path unit depends on inotify firing and on the change happening somewhere
  we thought to watch; this one needs neither. Whatever touched the boot
  artefacts and however it did it, the last thing before the machine goes down
  is a signing pass — while there is still a running system to do it with. A
  TPM-sealed db key is unsealed only inside this `ExecStop` pass and deleted
  immediately afterward; it is not a long-lived systemd credential.
- **`tilo-secureboot-sign.timer`** schedules a fallback scan 30 seconds after
  boot, keeping PE parsing and writes out of the login I/O burst. The path and
  update hooks still react promptly to changed artefacts. It cannot rescue the
  boot it runs in but does converge a machine that got here some other way.

`enroll` requires all three.

```bash
# On the machine, before touching firmware setup:
sudo tilo-secureboot capture-vendor-keys   # save the factory db/KEK while readable
sudo tilo-secureboot generate-keys         # PK, KEK, db -> /var/lib/tilo/secureboot
sudo tilo-secureboot sign                  # loaders + every deployment's kernel
sudo tilo-secureboot status                # read this before going further
# Copy PK.key and KEK.key to encrypted offline recovery media now.

# Reboot into firmware setup, CLEAR the Secure Boot keys, boot back, then:
sudo tilo-secureboot enroll --yes --owner-only-db
# Reboot into firmware setup, turn Secure Boot ON.
# After that verified Secure Boot boot:
sudo tilo-secureboot retire-enrollment-keys --yes
sudo tilo-secureboot seal-db-key --yes
```

`capture-vendor-keys` matters: ordinary `enroll` carries the factory `db`/`KEK`
alongside the owner's, so firmware capsule updates and signed option ROMs keep
verifying. The hardened `--owner-only-db` profile keeps the captured vendor KEK
but excludes vendor boot certificates from `db`; the capture remains available
offline for recovery. Signed option ROMs may no longer verify under that profile.
Once cleared, those keys cannot be read back, and proceeding without them takes
`--no-vendor-keys`. A capture containing only one of `db` or `KEK` is not
reported as complete; enrollment requires the same explicit override for that
partial-backup case and preserves the list that was available.

Enrollment does not trust a successful efivarfs write alone. It
reads `db`, `KEK`, and `PK` back from firmware, parses their bounded EFI
Signature Lists with `tilo-crypto`, and requires the exact owner certificate in
each variable. Read-back also requires the exact nonvolatile, boot-service,
runtime-service, and time-based-authenticated attribute set (`0x27`); an ESL
stored as an unauthenticated variable is not accepted as firmware trust.
`db` is addressed through the EFI image-security database GUID;
`KEK` and `PK` use the EFI global-variable GUID. Each update carries an
`EFI_VARIABLE_AUTHENTICATION_2` timestamp using current UTC, never a
future-dated replay floor. Installing `PK` uses a Rust-native detached
RSA/SHA-256 PKCS#7 self-signature and must make firmware leave Setup Mode.
Enrollment durably records the current kernel boot ID. Only after Secure Boot
is enabled and a later boot ID is observed does `retire-enrollment-keys`
durably unlink the online PK and KEK private keys. Their certificates and ESLs
remain for inspection and the db key remains available for routine kernel/UKI
signing. The retirement
step refuses while firmware is still in Setup Mode, if the enrolled PK/KEK do
not read back, or if db signing authority is missing or mismatched. It is also
available later as `tilo-secureboot retire-enrollment-keys --yes`; the explicit
confirmation exists because offline recovery copies are the only way to change
the enrolled hierarchy without clearing it in firmware setup again.

The delayed boot security audit repeats the public half of this proof on every
boot. It validates the retained db/KEK/PK certificates and exclusive local ESLs,
reads the bounded live firmware variables through `tilo-crypto`, and requires
each variable to still contain its corresponding owner certificate. It also
rejects unsafe, partial, hard-linked, or ambiguous plaintext/sealed/retired key
state. Firmware reset or replacement therefore becomes a failed health
invariant instead of being inferred only when the next signing operation fails.

The remaining db key can be removed from persistent plaintext storage with
`seal-db-key`. The command requires a TPM2 character device, binds a systemd
encrypted credential to PCR 7, immediately unseals it and checks it against
`db.crt`, publishes credential-loading drop-ins durably, reloads PID 1, and
only then durably unlinks `db.key`. Ordinary signing activations receive the
plaintext only as a kernel-managed credential under `/run` and have no TPM
device access. The always-active shutdown hook deliberately receives no such
credential: its `ExecStop` gets access only to `/dev/tpmrm0` or `/dev/tpm0`,
unseals into its private mode-0700 runtime directory, verifies the key against
`db.crt`, signs, and durably removes the plaintext before returning. The
runtime directory is not preserved by systemd, so abnormal termination or a
timeout also removes transient plaintext when the last signing unit releases
it. Manual
`tilo-secureboot sign` delegates to the ordinary credential-bearing service.

Because PCR 7 identifies the enrolled policy rather than the selected UKI,
`seal-db-key` also reads firmware `db` back and requires the owner certificate
to be its only entry. Otherwise any vendor-signed alternate OS admitted by the
same `db` could reproduce PCR 7 and decrypt Tilo's signing key. Machines that
must preserve vendor boot certificates may keep `db.key` offline, but they
cannot label a PCR7-only credential as isolated to Tilo.

Seal only after booting with the final owner Secure Boot policy. PCR 7 changes
when that policy changes, deliberately making unattended signing unavailable;
the encrypted offline db recovery copy is then required to reseal. The command
refuses until PK/KEK have already been retired, preventing the more powerful
enrollment authority from being mistaken for routine signing state.

If an enrolled machine does not boot, clearing the Secure Boot keys in firmware
setup returns it to setup mode and it boots exactly as before. That is the escape
hatch, and it is why enrolment is gated on setup mode rather than attempting to
displace a live Platform Key.

### Prerequisite: the signing tools

`tilo-secureboot` uses Rust-native `tilo-crypto` for Authenticode signing and
verification, signature-list creation, and bounded no-follow efivarfs writes
and read-back. It is in the image and in the frozen package snapshot.
`systemd-ukify` exists only in the throwaway UKI builder stage and is explicitly
removed from the runtime rootfs; each runtime subcommand still refuses with the
exact remedy if a required tool is missing, rather than half-signing a boot chain.

The online db key is not a general EFI signing oracle. The signer considers only
the fixed systemd-boot, bootc UKI, and OSTree kernel layouts; every candidate must
be a nonempty, bounded, caller-owned, single-link file that is not writable by
another account. Every directory between that file and the locked boot-store
root must satisfy the same ownership and non-writability boundary. Signing and
pre-enrollment verification both fail closed if those metadata checks do not
hold, preventing a writable parent directory from rename-swapping arbitrary PE
content into the managed set.
PK, KEK, and db signature lists are also verified as exact, owner-exclusive
encodings of their corresponding certificates before any firmware write;
captured vendor authorities are merged only by the explicit enrollment policy.
The complete merged db and KEK lists are then size-bounded and structurally
parsed again, with the owner certificate required, before the first firmware
variable is changed. The Rust `firmware-write-esl` primitive independently
rejects empty, truncated, dimensionally invalid, or partially framed ESL data,
so this boundary also holds for callers outside the enrollment script.

The final image explicitly removes inherited `openssl`, `sbsigntools`,
`efitools`, and `mokutil` command-line packages. It also removes the redundant
OpenSSL-linked `systemd-resolved` subpackage; NetworkManager exclusively owns
Tilo DNS state. Fedora core components including the remaining systemd binaries,
cryptsetup, kmod, composefs, and the TPM stack still link the distribution's
`libcrypto`; removing that ABI requires rebuilding those foundational packages.
The finished-rootfs build enumerates every installed RPM's direct `libssl.so`
and `libcrypto.so` requirements. It accepts only the reviewed compatibility
sets in `os/scripts/verify-runtime-crypto-packages.sh` and fails when any new
consumer appears; the verifier itself is deleted before the image is sealed.
Tilo-owned ELF outputs are independently rejected for either ABI, and Tilo-owned
TLS and signing remain Rust-native. The sealed root also deletes OpenSSL's
optional legacy-provider DSO and loadable ENGINE modules, plus GStreamer's
OpenSSL-backed DTLS, curl, and Icecast/Shoutcast plugins (Tilo has none of
those network media paths), the now-unusable WebRTC/ICE/SRTP companion
plugins, and the offline ARM compatibility frontend's Qt OpenSSL TLS backend.
The orphaned `libshout` package is erased after its plugin is removed, so its
unused direct `libssl` dependency is absent from the finished RPM database too.
Fedora's x86 QEMU links Intel's QATzip compression library, which links
`qatlib`, so `qatlib` stays as a reviewed `libcrypto` consumer. Tilo does not
use QuickAssist hardware, so the build erases the `qatlib-service` daemon and
rejects its presence in the finished rootfs.
The projector's required RTP/H.264 and HLS plugins remain installed.
Foundational Fedora consumers retain only the built-in provider surface they
require; the recurring security audit fails if any removed module reappears.

The immutable host also excludes the Git CLI and NetworkManager's OpenVPN
plugin. Both add avoidable OpenSSL-linked client stacks: Tilo's sandboxed Git
application supplies repository workflows, development containers may install
their own CLI, and the kernel/NetworkManager WireGuard path remains available
without OpenVPN's general-purpose TLS implementation.
The OpenSSH server remains forbidden. Fedora's `virt-viewer` package has a
strong dependency on `openssh-clients` and its `openssh` base package. Those two
packages are admitted only while `virt-viewer` is installed; Tilo's viewer uses
`--connect qemu:///system --attach`, and its Remote application still uses Rust
SSH. The retained client binaries are a documented distribution dependency,
not evidence that Tilo uses SSH to display its local Windows VM.

Fedora's `swtpm-tools` requires `gnutls-utils`. Normal fresh libvirt TPM 2.0
provisioning requests endorsement and platform certificates; the shipped
`swtpm_setup` configuration invokes `swtpm_localca`, whose initial CA setup uses
`certtool`. Removing that package breaks provisioning. It is admitted only with
`swtpm-tools`, including the companion GnuTLS utilities packaged alongside
`certtool`. The runtime audit reports this residual surface explicitly.
These are third-party VM dependencies; Tilo application TLS and all Tilo-owned
signing paths remain Rust-native. General OpenSSL, EFI signing and NSS tools
remain forbidden, as do Git, OpenVPN, and the OpenSSH server.

The named ABI sets also cover the pinned Fedora base's bootc/OSTree/container
transport, initramfs, authentication, storage, networking and TPM libraries,
and the rootfs requests' desktop/VM dependency closure (including Fedora
recommendations): Qt, Wi-Fi, PipeWire, GStreamer, FreeRDP/SPICE, software TPM, Xwayland, and the Fedora libvirt/QEMU
storage backends, mobile-device access and archive/DNS helpers. The review used
base digest
`69944835958dda565f8d613587535a3d90e3256cf0a0f0e29bca5ca3d2360d06`
and RPM snapshot digest
`912255637bab849ee5c3f62a963481f0b994576ef08cec8ff4fe4bc4cfb9e02f`.
Build-only compiler, Cargo and signing packages are not admitted by copying the
snapshot inventory. Package metadata can conservatively retain an ABI edge even
when an unused plugin was removed; the separate ELF gate continues to check the
actual Tilo outputs. The final installed inventory must still pass the build
gate: dependency-graph review alone does not prove a finished image or a boot.

The workspace also patches `rustls-native-certs` with the small
`tilo-native-certs` loader: on Tilo it reads only
`/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem`, ignores `SSL_CERT_FILE` and certificate
directory overrides, and therefore removes the final `openssl-probe` crate as
well as its heuristic trust-path discovery.

### What owner-enrolled Secure Boot buys

`lockdown=integrity` (`os/usr/lib/bootc/kargs.d/01-hardening.toml`) only engages
when the kernel is told by firmware that it booted verified. Without Secure Boot
that lockdown policy is inert. Tilo independently enforces module signatures,
strict `/dev/mem`, and compiles kexec out entirely, but Secure Boot remains what
authenticates the kernel containing those controls and closes the pre-kernel
replacement boundary.

### No systemd credentials from firmware or the boot loader

systemd imports system credentials from four places: the kernel command line,
SMBIOS type 11 OEM strings, qemu `fw_cfg`, and the credential files
systemd-stub passes on from the ESP. It skips them only in containers and
confidential VMs (systemd 259, `src/core/import-creds.c`). **Physical machines
are not excluded.** Firmware OEM strings are not in PCR 11, the register disk
unlock binds to. So whoever can set them, by reflashing firmware or with a
vendor tool, could hand a sealed boot `systemd.extra-unit.*`, `tmpfiles.extra`
or `ssh.authorized_keys.root`. The generation still unlocks, and their code runs
as root.

The sealed command line carries `systemd.import_credentials=no`
(`os/usr/lib/bootc/kargs.d/05-credentials.toml`). With it, PID 1 imports no
credential from any of those sources, in the initrd or after it, and
`tilo-security-audit` checks that it is live. Nothing in a sealed boot needs
imported credentials. Service-local credentials are unaffected, such as
`LoadCredentialEncrypted=` for the Secure Boot signing key. The unsealed dev
image deletes the file, because `tilo-vm` passes a dev VM its SSH key as an
SMBIOS credential.

### systemd filesystem restrictions

Tilo's kernel enables `CONFIG_BPF_LSM=y`, `CONFIG_DEBUG_INFO_BTF=y`, and has
`bpf` in `CONFIG_LSM`. These are a boot requirement for the systemd version in
the image: PID 1 installs its BPF LSM program for `RestrictFileSystems=` before
starting units. Leaving BPF LSM out makes systemd exit during early userspace,
before recovery or installer services can run. This does not enable
unprivileged BPF: `CONFIG_BPF_UNPRIV_DEFAULT_OFF=y` and the image policy keep
that disabled. `tilo-security-audit` verifies both the compiled options and
that `bpf` is present in the live LSM stack.

### No boot-time system or configuration extensions

The composefs digest in the UKI command line seals `/usr`. A systemd system
extension would overlay it after that check. Fedora presets
`systemd-sysext.service` and `systemd-confext.service`. At every boot they merge
each image or plain directory under `/etc`, `/run` and
`/var/lib/{extensions,confexts}`, and systemd's default image policy accepts
unsigned images. A file written to `/var` would then replace sealed `/usr`, or
`/etc`, content on the next boot.

systemd has no setting that refuses directory extensions or unsigned images for
those services. The image therefore masks both, and the `systemd-sysext.socket`
Varlink endpoint, so it merges nothing by itself. A caller who merges an
extension on purpose runs `systemd-sysext` with its own `--image-policy`. Doing
that already requires root, which can change the system in other ways too.
Only the unsealed development image (`os/Containerfile.dev`) unmasks
`systemd-sysext.service`, for its development layers.

The one caller that merges on purpose is `tilo-extension-activate.service`,
for the downloaded extensions the owner turned on (the media codecs, say). At
every boot and after every change it verifies each kept manifest against the
image's extension-manifest key (`/usr/share/tilo/extensions.pub`, a KMS key
separate from the OS release key), each kept artifact against
the manifest's SHA-256, rebuilds every part from those bytes alone, and merges
nothing if anything else is in the sysext search path. A file written to `/var`
therefore still never reaches `/usr` across a reboot: it is replaced, or the
merge is refused. See `os/usr/share/tilo/system-extensions/README.md`.

PCR 7 measures the Secure Boot policy. A former LUKS TPM design bound only to
that register; it has been removed because PCR 7 does not identify the selected
Tilo UKI. With Secure Boot off, such a binding would merely record "Secure Boot
is off" — a consistent measurement, but not a meaningful verified-boot boundary.

### The gap this used to have, and what closing it changed

**Everything from here to the end of this section describes the *ostree* backend,
which Tilo has left.** It is kept because it is the clearest account of why the UKI
matters, and because the reasoning is what justified moving backends at all — but
read it as history, not as a description of the current image. On composefs with a
UKI: the initramfs and the command line are both inside the signature, `PCR 11`
carries `tilo.generation=`, and `os/tests/rollback-boot-check.sh` passes on a VM
booted from an image `os/dev/build-image.sh` built.

`os/Containerfile` builds that UKI. It used to install `systemd-ukify` and never
call it, which is why this check failed on a real image for entirely correct
reasons; the stages that close it are described under [Building the
UKI](#building-the-uki) below.

#### What signing covered on the ostree backend

The firmware verifies what it `LoadImage()`s. On this backend that is
systemd-boot, and then the kernel — both PE binaries, both signable, both signed
by `tilo-secureboot sign`. What it does **not** cover:

- **The initramfs.** ostree deploys it as a plain cpio blob beside the kernel and
  the loader hands it to the kernel after the verification step, so it is not
  part of any signature check. That matters more here than it would elsewhere,
  because the initramfs is exactly where the disk gets unlocked: it runs
  `systemd-cryptsetup`, it holds the TPM unseal, it sees the passphrase. Someone
  who can write `/boot` can replace it and get code execution before the root is
  open, with Secure Boot on and every signature valid. I have not tested whether
  the kernel applies any check of its own to an initrd received this way; the
  firmware certainly does not.
- **The command line.** It lives in a Boot Loader Specification entry, an
  editable text file next to the kernel. Every hardening karg
  (`lockdown=integrity`, `init_on_alloc=1`, …) can be removed by anything that can
  write the ESP or `/boot`.

So owner-enrolled Secure Boot here buys a verified *kernel*, and that is worth
having — it is what makes `lockdown=integrity` engage — but it does not buy a
verified *boot*. Nobody should read "Secure Boot is on" on this backend as
meaning the early boot is trustworthy.

#### Which PCR distinguishes one generation from another

Not PCR 11. `systemd-stub` extends PCR 11 with the UKI's sections, and the stub
only runs when the firmware loads a UKI. Here the firmware loads the kernel
directly, no stub is involved, and PCR 11 stays at its reset value — so a
per-generation prediction for PCR 11 would predict a constant that every
generation satisfies, including the rolled-back one it exists to reject. PCR 12,
which the stub uses for the command line it was passed, is empty for the same
reason.

**PCR 4 is the one that does distinguish them.** The firmware measures every
image it loads into PCR 4, and each generation deploys its own kernel file at its
own path, so PCR 4 already separates generations without any UKI. That reframes
the earlier advice in this document: PCR 4 was called unusable because it changes
on every OS update and would send the user to the recovery key each time — but
that is only true for a *fixed* policy. A per-generation **signed** PCR policy is
precisely the mechanism that makes a changing PCR usable, and it is what
`keys/tpm2-pcr-signing.public.pem` was reserved for. If generation-bound unlock is
being built, PCR 4 + PCR 7 with signed per-generation predictions is the shape
that works on the backend we actually have.

One thing it still does not give you: **anti-rollback**. A signed prediction for
generation N-1 stays valid, so an attacker who puts back the older signed kernel
gets a PCR 4 value that matches a prediction the TPM will honour. Refusing that
needs monotonicity the PCRs cannot express — a TPM NV counter that only moves
forward, with the policy bound to its current value. `systemd-cryptenroll` does
not expose that today, so anti-rollback is a gap in any PCR-only design, UKI or
not. Worth knowing before PCR 11 gets treated as the thing standing between the
system and a rollback attack.

#### Is `bootc --composefs-backend` close enough to depend on?

Evidence from bootc 1.15.2 as shipped, rather than from upstream's roadmap:

- `--composefs-backend` is an ordinary documented flag on `install to-disk` and
  `install to-filesystem`. It is not hidden, and no `BOOTC_*` environment
  variable gates it — the only `BOOTC_*` knobs in the binary are for debugging,
  loopback cleanup and SELinux.
- The lifecycle verbs exist, not just install: the binary carries
  `bootc_composefs/{boot,update,switch,status,delete,finalize,export,repo,selinux}.rs`,
  and there is a top-level `composefs-finalize-staged` subcommand.
- `--uki-addon` exists alongside it, which is the UKI-shaped half.
- `bootc upgrade` has no composefs flag, consistent with the backend being
  recorded at install time and detected afterwards.

The implementation includes the lifecycle above. This evidence cannot establish
whether upstream considers it stable, and the
switch is not additive: it replaces the root filesystem model with a
content-addressed, fs-verity-backed one, so it cannot be trialled on the side of
an existing install. It also changes what `--allow-missing-verity` has to be set
to depending on the filesystem underneath.

It is the only path to a signature-covered initramfs and command line on this
stack. UKI work should target this backend and include a verified-boot campaign
with the recovery path proven before anything depends on it. It should not block
generation-bound unlock, because PCR 4 provides that today.

## Building the UKI

Three stages at the end of `os/Containerfile`, after the rootfs is finished and
`bootc container lint` has run over it:

```
rootfs ──▶ split ──▶ uki ──▶ final
           kernel     bootc container    split rootfs
           moves out  ukify over the     + the UKI under
           of the     split rootfs       /boot
           rootfs
```

The order is forced. bootc refuses to seal a rootfs that still carries
`vmlinuz`/`initramfs.img` under `/usr/lib/modules/<kver>`, and the digest ukify
seals has to be the digest of what will be mounted — so the split comes
first and nothing may be added to the rootfs afterwards. `/boot` is the one tree
excluded from the digest, which is what lets the UKI live inside the image it
describes.

`bootc container ukify` is the piece that makes this cheap: it computes the
composefs digest itself and reads `/usr/lib/bootc/kargs.d`, so the hardening set
and `tilo.generation=` reach the sealed command line through the same mechanism
that already existed for the unsealed one. `os/dev/uki.sh` is the script it runs,
and it fails the build if the generation did not make it into the seal.

The board images (`os/Containerfile.board`) and the dev installer base
(`os/Containerfile.dev-installer-base`) seal the same way, with the same
`uki-tools` and `uki` stages. The tools come from one list,
`os/dev/uki-tools.packages`, and every `uki` stage declares the same seal inputs
(`TILO_CFS_DIGEST`, `TILO_SEAL_GENERATION`). `tools/policy/check_uki_pipeline.py`
fails when one copy changes alone, and when a `final` stage adds anything but
the UKI. The composefs probe under `os/dev/composefs/` is deliberately separate,
and its header says why.

Two details matter at the release boundary:

- **An ordinary development build does not sign the UKI.** Published release
  images and tagged release media instead require the protected
  `TILO_PCR_PREDICTION_SIGNER`: it appends an Authenticode signature from the
  Tilo release EFI certificate (`keys/efi-signing.crt`, key held in AWS KMS) to
  both the live and deterministic target UKIs and signs the target's PCR 11
  prediction without exporting either private key. Firmware trusts that
  certificate once its owner enrolls it in `db`; see
  [the release certificate](#the-user-enrolled-tilo-release-certificate). The release fails closed if the signer, certificate pin, or returned
  signature is missing or wrong. Development/custom media keeps the strong
  passphrase fallback. Measurement itself does not depend on PE signing:
  `systemd-stub` extends PCR 11 with the command line in either case.
  The build helpers ignore a merely present signer path: CI must set
  `TILO_REQUIRE_PCR_PREDICTION=1`. A developer who intentionally exercises a
  root-owned local broker can instead set `TILO_ALLOW_LOCAL_PCR_SIGNING=1`; this
  explicit opt-in is never set by CI rehearsals.
- **It does not get the digest right first time.** `bootc container ukify` and
  `bootc install` compute different composefs digests for the same image, so
  install refuses the UKI:

  ```
  The UKI has the wrong composefs= parameter (is 'sha512:<A>', should be sha512:<B>)
  ```

  That is [bootc#2334](https://github.com/bootc-dev/bootc/issues/2334). The
  recovery is to seal the value install asked for and install again, and it
  converges because the digest covers the rootfs — rebuilding the UKI changes only
  `/boot`, so install's expected value does not move.
  `os/dev/build-image.sh` runs that loop; `os/dev/disk-image.sh` recognises the
  refusal and prints the digest to seal rather than the raw error. If the expected
  value ever *does* move between passes, the build says so and stops, because then
  the seal is circular and needs the upstream fix.

## Interface for generation-bound unlock

`crates/system/tilo-rollback` wants four things from the boot chain, and the sealed
layout supplies all four.

**PCR 7.** The firmware measures the Secure Boot policy into it on every boot,
UKI or not. A verified tagged release combines that device-specific platform
state directly with the authorized artifact/generation binding below. Media
without the complete release chain deliberately enrolls no TPM slot.

**PCR 11.** Extended by `systemd-stub`, which runs because the firmware loads a
UKI. It carries the command line, so it distinguishes one generation from another
— which is exactly what a policy bound to it needs, and what PCR 11 could not do
on the ostree backend, where no stub was involved and the register stayed at its
reset value.

Verifying that claim took replaying the firmware's own event log: twelve firmware
measurements plus systemd's four runtime phases, reproducing the live register
exactly. Reading a non-zero PCR 11 is not evidence that a *particular* string was
measured into it.

**`tilo.generation=<N>` inside the signature.** Written into
`/usr/lib/bootc/kargs.d/02-generation.toml` at image build from the
`TILO_GENERATION` build argument, which is where `bootc container ukify` reads it.
The alternative — a Boot Loader Specification entry under `/boot`, assembled by the
bootloader — was available all along and deliberately not taken: a file on the ESP,
outside any signature, editable by anything that can write there, would look like
compliance and provide none of it.

**The unlock hook.** The sealed command line carries no `rd.luks.uuid=`; it
carries `root=tilo-auto`. In the initramfs, `tilo-root-generator` starts
`tilo-root-discover`, which finds the one Tilo root on the boot ESP's disk and
attaches it as the fixed mapping `root` (see "Target-independent enrollment"
below). Before attaching, it runs `tilo-rollback-initrd-unlock` for that
volume, so the key file the hook writes is:

```
/run/cryptsetup-keys.d/root.key
```

`systemd-cryptsetup` consults that path before it tries any token. A root whose
header carries `tilo-signin` tokens asks first for a person's own PIN or password
instead ([sign-in.md](sign-in.md) §3.2): `tilo-boot-signin` takes it, and `tilo-auth
boot-unlock` writes the same key file from that person's keyslot and leaves the
one-time handoff to their session. The greeter asks `tilo-authd` for that person
when it starts and starts their session without a screen of its own; `pam_tilo`
accepts the handoff once, and if PAM asks anything instead the greeter cancels
and shows its ordinary screen. "Can't sign in" (Enter on an empty field) ends
it, and everything below runs as on any other root, with one difference: while
`systemd-cryptsetup` attaches the volume, Plymouth's password agent is stopped
and `tilo-boot-signin other-ways` asks its questions on the boot screen instead,
word for word. An empty answer there, which Plymouth's agent would simply ask
again, stops that attempt and shows the sign-in screen again. Without Plymouth
that agent steps aside, and the console prompts offer no way back. The key file is removed once
the volume is attached, because `/run` is carried into the booted system. The
`tilo-rollback` dracut module installs the binary, its TPM tools and public policy
key, the root generator and the discovery helper. The hook asks for the PIN only
when the LUKS header contains a `tilo-rollback` token that carries one: a token
sealed without a PIN is the automatic unlock, and prompting for a secret nothing
checks would defeat it. A command line
that still names the volume with `rd.luks.uuid=` (an older layout) is served by
`tilo-rollback-unlock-generator`, which writes
`/run/cryptsetup-keys.d/luks-<LUKS-UUID>.key` instead. A cancelled or failed
unseal removes partial key material and returns control to the independent
recovery-key, passphrase, or FIDO2 prompt instead of making boot an emergency.

The rollback crate, early-unlock consumer, late-boot confirmation services, and
target-independent installer integration ship together. The graphical installer
offers the short PIN only after `tilo-verified-boot-install-preflight` verifies the
exact deterministic target UKI, its EFI certificate, its generation-bound PCR 11
prediction, and the prediction authority. The privileged backend repeats that
proof immediately before enrollment. Otherwise it requires the strong
passphrase, mandatory recovery key, and optional FIDO2 slot; TPM presence alone
never enables the PIN.

### Target-independent enrollment

The implementation does not put a prediction private key on the machine. It
makes the target measurement a release artefact while keeping the platform
measurement device-bound. These pieces form one atomic security boundary:

1. **Discover the target without a UUID in the UKI.** Give the Tilo root an
   architecture-neutral, Tilo-owned GPT type GUID. An initramfs generator must
   resolve `LoaderDevicePartUUID` to the ESP from which systemd-stub booted,
   inspect only that ESP's physical disk, and require exactly one partition of
   the Tilo type. Zero or multiple matches are a recovery-key failure, never a
   guess. This is deliberately not generic `root=gpt-auto` or a filesystem/GPT
   label: a dual-boot Linux root or a removable disk can legitimately collide
   with either. The generator creates the fixed `root` crypt mapping and
   `sysroot.mount`; the rollback hook consequently writes
   `/run/cryptsetup-keys.d/root.key`. The UKI can then carry only constant
   discovery options.
2. **Build a target UKI, do not edit one on the target.** The installer image
   needs two deterministic UKIs over the same installer composefs tree: the live
   one containing `tilo.install`, and an installed-target variant containing the
   fixed discovery command line. The latter stays outside bootc's scan path
   while the live medium boots, and installation atomically promotes its bytes
   into `EFI/Linux`. Owner Secure Boot signing may append an Authenticode
   certificate; that certificate table is excluded from PCR 11, so it must not
   alter the predicted section measurements.
3. **Keep predictions outside the composefs digest.** Populating
   `/usr/share/tilo/pcr-predictions` after calculating PCR 11 changes the root
   digest, which changes the UKI command line, which changes PCR 11 again. There
   is no usable hash fixed point. Release-signed prediction sidecars therefore
   belong under the ESP's Tilo-owned directory (or in a UKI section explicitly
   excluded from its own measurement), are copied into the LUKS token at
   enrollment, and are verified by the public prediction key already in the
   image.
4. **Authorize PCR 11; bind PCR 7 directly.** A release can predict PCR 11 for
   the deterministic target UKI, but cannot predict a particular machine's PCR
   7. The TPM policy order must be
   `PolicyPCR(PCR11) -> PolicyAuthorize(release key, generation) ->
   PolicyPCR(PCR7) -> PolicyNV -> PolicyAuthValue(PIN)`. The first PCR is
   replaceable by a release-signed update prediction; the second is read on the
   device during enrollment and is part of the sealed object's final policy.
   Putting PCR 7 before `PolicyAuthorize` would be ineffective because
   `PolicyAuthorize` resets the running policy digest. Removing either PCR is
   not an acceptable shortcut.
5. **Stage before reboot.** The update transaction must obtain the startup PIN,
   verify the currently booted generation still unseals, load the next
   generation's release-signed PCR 11 sidecar, create and verify its keyslot,
   and only then make that deployment bootable. The PIN is not retained for the
   desktop session, so an unattended update cannot silently mint a new
   PIN-authorized object: the update UI must ask for it when staging needs a new
   generation. Cancellation leaves the current deployment and floor untouched.

The release gate for enabling the installer's short-PIN choice is correspondingly
end-to-end, not a source grep:

- an encrypted full-disk VM installs without any target UKI byte rewrite, boots
  by the boot-ESP/same-disk Tilo-type discovery path, and unlocks with the PIN;
- Windows+Tilo and Linux+Tilo layouts with other root-like partitions prove that
  only the Tilo type on the boot ESP's disk is selected, while duplicate Tilo
  types and a spoofed removable disk fail closed;
- changing a measured target-UKI section, PCR 7, the generation-bound
  prediction signature, or the rollback floor independently refuses TPM
  unlock and still offers the recovery key;
- a signed next generation is enrolled and verified before reboot, a missing or
  corrupt prediction cannot be staged, and cancelling the PIN prompt leaves the
  previous generation bootable;
- owner re-signing changes only the PE certificate table and leaves the
  calculated and boot-event-log-replayed PCR 11 value unchanged on both amd64
  and arm64.

## Disk encryption

`tilo-install --encrypt` produces a LUKS2 root. The block work is
`/usr/libexec/tilo-disk-encrypt`.

**It does not require a TPM.** `bootc install --block-setup tpm2-luks` seals the
one and only key slot to a TPM and refuses to run without one, which turns
hardware absence into no encryption at all — the opposite of what absence should
do. So Tilo builds the layout itself (ESP + `/boot` + LUKS2 root) and installs
`bootc ... to-filesystem` into the opened mapping.

The volume is always created with a **recovery key**, shown once during install
and verified against the finished LUKS header before the temporary install key
may be removed. If that value cannot be shown and verified, installation stops
while the temporary key still exists. Everything else is a slot layered on top
of it:

| Machine | Slots after install | Boot experience |
|---|---|---|
| Verified release chain + TPM2 | recovery key, TPM2+PIN generation slots | type the short PIN |
| TPM without verified release chain | recovery key, passphrase | type the passphrase |
| No TPM | recovery key, passphrase | type the passphrase |

When the installer's startup secret is the account password (the wizard's one
secret), `tilo-disk-encrypt enrol --pin-is-account-password` adds a
`tilo-startup-secret` token naming that slot. It holds no secret. Before the
volume opens, `tilo-boot-signin unlock-prompt` reads the header and the boot screen
asks for "your password" where that token has a slot, "your disk passphrase" where
a slot no token claims exists, and offers the recovery key only where a
`systemd-recovery` token has a slot.

An optional FIDO2 security key adds another slot to any row. Selecting it is a
requirement: enrollment failure stops installation with an actionable retry
message rather than silently finishing without the requested factor. The sealed
target UKI carries both `tpm2-device=auto` and `fido2-device=auto` in its
`rd.luks.options=` argument, which names no volume and so applies to the root
(`os/usr/lib/bootc/kargs.d/00-root.toml`); `tilo-root-discover` passes the same
options when it attaches the root. LUKS2 metadata tells systemd-cryptsetup
which of those handlers has enrolled material; without the FIDO2 boot option a
security key could be successfully enrolled yet never tried by the initramfs.

The installer and privileged backend both require a startup secret. Verified
tagged release media uses a nontrivial short PIN only after proving the exact
signed target UKI and its authorized PCR 11 prediction, then binds that policy
to the machine's current PCR 7 and rollback floor. Development/custom media, or
any failed prerequisite, uses the strong-passphrase path. The removed older TPM
design sealed only to PCR 7; it is not shipped because PCR 7 authenticates the
firmware Secure Boot policy/database but does not bind unlock to the selected Tilo
UKI or initramfs.

This means a stock machine that has not completed owner/OEM Secure Boot
enrollment uses the strong-passphrase path even when it has a TPM. Software
cannot truthfully manufacture firmware trust without the owner's physical
ceremony. Even an OEM-provisioned machine remains on the strong-passphrase path
unless the media also carries the signed measured-artifact policy for its exact
target UKI.

Once that ceremony is done, a machine may finish with a system volume that opens
by itself. `tilo-rollback enroll --automatic` seals a generation slot with no PIN,
and refuses unless the boot it is running is one the machine can prove it
approved: Secure Boot on, kernel lockdown active, and a signature database holding
exactly the authorities recorded when the slot was sealed
([rollback.md](rollback.md), "A keyslot with no PIN"). The same check runs at every
unlock, so a machine whose Secure Boot policy changed — a `db` that gained a
third-party CA, say — asks for the typed secret instead of opening on a boot it no
longer recognises, rather than opening anyway or leaving an unbootable disk.

The privileged installer enforces the same startup-secret policy shown by the
graphical installer. It requires at least fifteen characters (the NIST
single-factor minimum), rejects leading or trailing whitespace, common
passwords padded with digits or punctuation,
short numeric-only secrets, and repeated low-diversity patterns. A long numeric
secret remains possible, and multi-word passphrases do not have to satisfy an
arbitrary mix of upper case, punctuation, and digits. This check runs again
after privilege escalation so a modified or non-graphical client cannot bypass
the UI validation.
TPM presence with unverified early boot instead
uses a normal passphrase slot: sealing against a PCR 7 value that merely says
"Secure Boot is off" would not prevent replacement boot code from capturing the
PIN and must not be presented as hardware-backed protection.

### Fresh-machine provisioning is a trust-model decision, not an installer retry

The common factory-machine case cannot be turned into the short-PIN row by
remembering installer state across a reboot. The first Tilo executable has no
signature accepted by the factory firmware: Tilo intentionally has neither a
Microsoft/OEM-trusted bootloader nor a Tilo certificate in the factory `db`.
Consequently the live medium has to start with Secure Boot disabled (or with the
firmware already in setup mode). PCR 7 and kernel lockdown from that boot cannot
authenticate the installer, and a TPM slot sealed there would protect a PIN from
neither a replaced installer nor a replaced target UKI.

Nor can the installer portably fix that state itself. While a factory Platform
Key is active, changing `db`, `KEK`, or `PK` requires an authenticated update
authorised by a private key Tilo does not and must not possess. UEFI provides a
standard request to enter firmware setup, but no standard, unattended operation
for clearing the owner's keys, importing a new certificate, or enabling Secure
Boot. Those controls, confirmation screens, and even support for appending a
certificate from the ESP vary by firmware.

That leaves three secure product choices. The first is the supported
production path today:

1. **Enroll the Tilo release certificate.** Every published image signs its
   UKIs with Tilo's own release EFI certificate, and the owner adds that
   certificate to firmware `db` once. See
   [the release certificate](#the-user-enrolled-tilo-release-certificate).
2. **Keep owner-first trust.** Installation uses a strong LUKS passphrase. A
   later guided ceremony may generate the owner keys in the encrypted target,
   sign and verify every target UKI, ask the owner to import the public key in
   firmware, and only after a verified reboot offer to replace the passphrase
   slot with TPM2+PIN. The ceremony must persist only public progress on the ESP;
   private signing keys and a pending PIN must never be placed there. It must
   resume by re-proving the enrolled certificate, UKI signatures, Secure Boot,
   lockdown, and recovery-key access rather than trusting a progress marker.
   This is necessarily interactive and firmware-specific, so it cannot be the
   default nearly-headless path on arbitrary PCs.
3. **Add a factory-trusted Tilo release chain.** This is the prerequisite for a
   fresh PC to boot both installer and target under verified boot and use one
   TPM-throttled PIN from its first installed boot *without* an enrollment step.
   It can later replace the user-enrolled certificate without changing the PCR
   prediction authority. The release bootloader and every bootable UKI need PE
   signatures rooted in a certificate accepted by target firmware (through an
   OEM agreement or an industry Secure Boot signing programme); installation
   must stop generating target-specific unsigned UKI bytes, or obtain their
   signatures from an authenticated, availability-safe signing service. Release
   signing keys belong in an audited HSM/KMS with dual-control policy, short-lived
   CI identity, rotation/revocation and offline recovery. The resulting media,
   update, firmware-revocation, BitLocker-coexistence and TPM dictionary-attack
   paths then need a physical hardware matrix before this becomes the default.

Published images and tagged release media carry the release chain only when
their protected `TILO_PCR_PREDICTION_SIGNER` obtains both an Authenticode
signature from the pinned Tilo release EFI certificate and PCR-authority
signatures without exporting either private key. CI measures the exact resealed
target with that image's `systemd-measure`, verifies every returned signature,
and fails the release when the signer is absent. Development or custom media
without that chain keeps the secure fallback: a strong startup passphrase.
`target_verified_boot_ready` is never enabled by TPM presence, an EFI flag,
signed files alone, or a resumable-install marker; on a machine that has not
enrolled the release certificate, Secure Boot refuses the signed UKI rather than
reporting verified boot.

### The user-enrolled Tilo release certificate

`keys/efi-signing.crt` is Tilo's release EFI certificate: an RSA-2048,
code-signing X.509 certificate, self-signed through its AWS KMS key
(`alias/tilo-efi-signing`), whose private key has never existed outside KMS.
Its DER SHA-256 is pinned independently as the protected
`TILO_EFI_SIGNING_CERT_SHA256` variable, and every release UKI signature is
checked against that pin and the unchanged Authenticode payload. It is the
supported production Secure Boot trust anchor for Tilo OS:

- **The owner enrolls it.** Add the certificate to the firmware signature
  database (`db`), either through the firmware setup's key-management screen
  (most accept a DER `.cer`/`.crt` from a FAT volume; convert with
  `tilo-crypto certificate-der keys/efi-signing.crt tilo-release-efi.cer`) or,
  in Setup Mode, as part of the owner's own `db` next to any vendor
  certificates they keep. Verify the fingerprint with
  `tilo-crypto certificate-sha256` before enrolling. Tilo never writes firmware
  variables on the owner's behalf.
- **No shim, no MOK.** Tilo removed shim and GRUB; systemd-boot and the signed
  UKIs are verified by the firmware directly against `db`.
- **What it buys.** After enrollment, published images, their updates and tagged
  media boot with Secure Boot enforcing and kernel lockdown, and PCR 7 reflects
  a `db` that authenticates the Tilo release chain, which is what the
  generation-bound TPM policy binds.
- **What it does not.** A machine that has not enrolled the certificate does
  not trust Tilo's UKIs; it must either enroll it or boot with Secure Boot off
  and use the strong-passphrase path. Revoking the certificate means adding it
  to `dbx` or removing it from `db`.

A factory-trusted chain (choice 3) can replace this later. The PCR prediction
authority (`keys/tpm2-pcr-signing.public.pem`, `alias/tilo-pcr-prediction`) is
independent of the EFI certificate and does not change when the EFI trust
anchor does.

The removed TPM slot bound to **PCR 7 only**, set explicitly rather than inherited.
PCR 7 is the Secure Boot policy, so the seal would break if the boot policy changed.
Binding to a register that covers the boot artefacts themselves sounds stronger and
is unusable on its own: it changes on every OS update, so every update would send
the user to the recovery key. That is what the signed per-generation predictions in
[rollback.md](rollback.md) are for — the authority signs the values the *next*
generation will produce, so an expected change stops being a lockout. It is also
why the generation has to be on a measured command line: without it, a prediction
the authority signed for one generation would be satisfied by any other.

There is consequently no supported `--reseal` procedure in this release.
`tilo-disk-encrypt enrol --reseal` fails closed until a recovery ceremony can
re-prove the replacement TPM's PCR7 state, exact authorized generation, and a
working recovery factor before replacing the stale slot. A replaced motherboard
or cleared TPM therefore falls back to the recovery key instead of creating a
weaker slot. The recovery key remains mandatory because hardware factors can
still be lost.

### An initramfs is required

The kernel links the boot-path storage, USB, HID and network drivers statically,
which is why `/etc/kernel/install.conf` used to disable initrd generation. That
was wrong for an encrypted root: static drivers get the kernel as far as *seeing*
the block device, and opening a LUKS volume takes early userspace —
device-mapper setup, a TPM unseal or a passphrase prompt, `systemd-cryptsetup`.
`os/Containerfile` now generates the initramfs explicitly after the kernel swap,
because the base's `layout=ostree` kernel-install config deliberately leaves
initrd generation to the image build.

Tilo supports local roots only. The signed initramfs therefore explicitly omits
Dracut's network, NetworkManager, NFS, iSCSI, Clevis/NBDE, and crypt-SSH modules.
The `tilo-sibling-unlock` module (`docs/device-reach.md`) carries the image's
NetworkManager binary for its own unit instead, which only a root whose LUKS2
header holds a `tilo-sibling-unlock` token starts; it parses no network-root
command line.
Passphrase, TPM2, FIDO2, Plymouth, and rollback recovery remain mandatory. This
cuts boot-time archive I/O and removes unused pre-root network and TLS parsers.

## Testing it in a VM

`os/dev/vm.sh` always boots the 4 MB Secure-Boot OVMF build with SMM on and the
pflash marked secure, because that is the only Fedora build with TPM support: the
2 MB one reports zero active PCR banks, so nothing is measured and every PCR reads
zero. `vm.sh firmware` prints what was selected and whether it can measure; a
`--tpm` boot on a host that has only the 2 MB build is refused rather than run
against a firmware that cannot measure into it.

```bash
./os/dev/vm.sh boot --tpm --attach /tmp/tilo-vm/enc.qcow2   # TPM 2.0 via swtpm
./os/dev/vm.sh boot --secboot                              # keep the variable store
```

Enforcement is a property of the **variable store**, not of a qemu flag. By
default the store is pristine — no keys, Secure Boot in setup mode, re-seeded
every boot — which still measures into PCR 7 and is exactly the state
`tilo-secureboot enroll` expects. `--secboot` keeps that store across boots so the
enrolment done inside the guest survives a restart of qemu, making enrolment an
explicit step rather than something the firmware template arrived with.
`TILO_SECBOOT_KEYS=microsoft` boots the vendor-key store on purpose; note that it
rejects an owner-signed UKI outright, so nothing boots.

`os/tests/secureboot_vm_probe.sh` is the lower-level enforcement probe used
while developing the kernel and signing stack. It boots the compiled Tilo kernel
against a fresh Secure-Boot-capable OVMF variable store, enrolls Rust-generated
`db`, `KEK`, and self-signed `PK` updates through efivarfs, then reboots twice
with that persisted store. The negative boot must contain OVMF's explicit
`Access Denied -- rejected probably by Secure Boot`; the byte-identical EFI test
application signed with the enrolled `db` must execute and emit
`TILO_SECURE_BOOT_SIGNED_EFI_PASS` through the debug port. Absence of an unsigned
marker alone is not accepted as proof.
Kernel CI exports the exact freshly built x86 kernel, `gen_init_cpio`, and
native Rust signer through a CI-only build target and runs this probe as a
mandatory step. Kernel or signing changes therefore cannot pass on static
format tests alone.

`--tpm` attaches a software TPM, **started** (`--flags
not-need-init,startup-clear`), whose state persists in `$TILO_TPM_DIR`, so a disk
enrolled in one boot can be unlock-tested in the next. Without that flag the
emulator answers `TPM_RC_INITIALIZE` to every command and the boot is not measured
at all, which looks exactly like a UKI that failed to measure.

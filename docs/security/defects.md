# Security & identity defects — as-built audit, 2026-07-30

Findings from a four-way survey of the boot chain, identity/key management, threat-model
documentation, and the cryptographic-roles implementation. This file preserves **defects
observed at the original audit snapshot**; later status notes distinguish repaired source
from still-open validation. The forward-looking design lives in
`docs/security/architecture.md` and deliberately does not
reference any of this.

Severity: **P0** = exploitable or silently produces invalid security state. **P1** = a documented
guarantee is not delivered. **P2** = correctness/hygiene.

Findings that have since been fixed carry a **Resolved** note naming the test that holds the fix
in place. The description of the defect is left as it was written, so the record still says what
was wrong: file:line citations in a description are as found, and the current source is named in
the status note below it.

Statuses, current paths and regression-test names were last reconciled against source on
2026-10-14. That reconciliation read code and tests; it ran no installed, booted or deployed
qualification, and a status note never claims one unless it names the receipt.

---

## P0-1 — The identity ceremony persists a key that cannot sign

`crates/sdk/ui-core/src/widget/identity_flow/mod.rs:180-191`

`SerializableIdentity::from_identity` cannot reach into `frost_ed25519`'s `SigningKey`, so it
persists `SHA-512("tilo-identity-flow-v1-placeholder-key" ‖ group_public_key)` as `secret_bytes`.
The in-source comment says it outright: *"clearly NOT useful for signing."*

Any identity minted through the UI ceremony produces signatures that do not verify against its own
recorded public key.

Compounding: `crates/runtime/runtime-identity/src/lib.rs:270-297` detects the pubkey/secret
mismatch and **deliberately does not fail** — the only signal is a
`#[cfg(debug_assertions)] eprintln!`. In a release build the failure is silent.

`crates/session/shell/src/firstboot.rs:190-226` correctly bypasses this path and writes the real dalek seed via
`ThresholdIdentity::bootstrap_single_device_seed`. So firstboot-minted identities are valid and
ceremony-minted identities are not, with the same on-disk format and no way to tell them apart.

**Fix direction:** add the key-extraction helper to `threshold-identity`, delete the placeholder
branch, and make the mismatch fatal in `runtime-identity` rather than advisory.

**Resolved.** `ThresholdIdentity` holds the single-device key in dalek form and exports its seed;
the ceremony persists that seed, and `IdentityRecord::into_threshold_identity` refuses a record
whose secret does not derive its recorded public key. The mismatch is fatal in `runtime-identity`
in every build profile. Firstboot now goes through the same encoder, so there is one writer of
the format. Held by
`crates/collab/tilo-identity-ceremony/src/lib.rs::tests::identity_record_serialization_and_signing_round_trips`
(phase `ceremony_minted_record_signs_for_its_own_recorded_public_key`)
and
`crates/runtime/runtime-identity/src/unix.rs::tests::a_local_secret_must_derive_the_recorded_key_to_sign`
(phase `secret_that_does_not_derive_the_recorded_pubkey_is_refused`).
The `identity_flow` widget cited above no longer exists; the record encoder is
`crates/collab/tilo-identity-ceremony/src/lib.rs` (`IdentityRecord::into_threshold_identity`).

---

## P0-2 — The FUSE replication path performs no authentication

`crates/system/fs/src/sync.rs:159-195`

`SyncProtocol::accept` takes any connection on `TILO_FS_SYNC_ALPN` and:

- `MODE_FULL` → serialises the entire namespace document to the caller (`sync.rs:169-171`)
- `MODE_SYNC` → feeds inbound bytes directly into `sync_receive` (`sync.rs:179-183`)

There is no identity check, no ACL consultation, and no signature verification on either branch.
The same applies to `fetch_remote_doc` (`sync.rs:104-113`) and the anti-entropy full-document merge
(`sync.rs:317`).

The ACL in `crates/system/fs/src/acl.rs` is well-built — fail-closed, deny-biased, delegating to
`collab_types::decide_capability`, and it aborts the mount rather than serving unenforced. But it is
consulted only on the FUSE syscall path (`crates/system/fs/src/fs.rs:432`, `:456`, `:561`), i.e. against local
processes. **It is not a replication boundary.**

Any peer that can reach the endpoint and speak the ALPN can read and rewrite the whole namespace.

**Fix direction:** the responder must resolve the connecting node to an identity and evaluate the
ACL per-connection. Depends on a real, persistent device identity existing first.

**Resolved in source; runtime qualification separate.** At
[Tilo `260d8c7d3f`](https://code.tilo.so/tilo/tilo/commit/260d8c7d3f7133c36bf8a7d97a87fb264c286926),
[`SyncProtocol::accept`](../../crates/system/fs/src/sync.rs) calls
`authenticate_connection` before request dispatch. `serve_request` refuses a different
workspace and requires whole-workspace read authority for a document or write authority
for sync. `accept_namespace_payload` checks authority and the workspace/device signature
before applying bytes; sync rounds recheck the live write authority. The historical
unauthenticated replication description above is not a current-source vulnerability claim.

Existing regression sources are
[`peer_auth::tests`](../../crates/system/fs/src/peer_auth/tests.rs)::
`peer_auth_rejects_tampered_expired_future_or_revoked_certificates` and
[`sync::tests`](../../crates/system/fs/src/sync/tests.rs)::
`namespace_documents_reach_only_entitled_peers_and_readers_cannot_author`,
`a_host_refuses_a_namespace_request_for_another_workspace`, and
`a_sync_message_of_another_workspace_is_refused_and_nothing_merges`.
These names and guards were checked against source for the documentation correction;
no tests or installed/deployed acceptance were run by that check.

---

## P0-3 — The iroh node key is ephemeral, so FS attribution and ACL subject rotate every boot

`crates/system/fs/src/sync.rs:89-96` builds the endpoint without ever passing a `secret_key`, so the iroh NodeId
is freshly random on every process start.

Consequences:

- `crates/system/fs/src/main.rs:221,286` sets `author = endpoint.id().to_string()` — authorship attribution is a
  new anonymous string each mount.
- `crates/system/fs/src/main.rs:219,283` resolves the default ACL subject from the same key, so **any ACL grant
  written against the default subject is dead on the next boot**.
- The node-id-reuse guard at `crates/system/fs/src/namespace.rs:147-152` ("a restart with the same author can't
  reuse a node id") never engages, because the author is never the same twice.

**Resolved.** The mount binds under the machine's device key, so the `NodeId` *is* the device's
Ed25519 public key and is unchanged across a remount; a device key that will not load fails the
mount rather than falling back to a random one. `bind_endpoint` is split into
`bind_endpoint_with_key` and `bind_ephemeral_endpoint` so the random-identity path is named at
every call site. Held by `crates/system/fs/src/identity.rs::tests::bound_endpoint_is_named_by_the_device_key`
and `::the_device_identity_is_the_iroh_key_and_survives_restart` (phase
`identity_survives_a_restart`).

---

## P0-4 — The image cannot be installed, so no boot-chain claim in this file was ever testable

`os/usr/lib/bootc/install/40-tilo.toml` sets `bootloader = "systemd"`. Every `bootc install` against
a Tilo image therefore ends at the bootloader step:

```
Bootloader: systemd
error: Installing to filesystem: bootupd is required for ostree-based installs
```

bootc has two install backends and only the **composefs** one can plant systemd-boot. The
**ostree** backend — what bootc-image-builder, Anaconda's `ostreecontainer`, and `tilo-install` all
drive — matches on the resolved bootloader in `install_with_sysroot()` and, for `Systemd`, does
nothing but `anyhow::bail!("bootupd is required for ostree-based installs")`. The single caller of
bootc's `install_systemd_boot()` is `crates/lib/src/bootc_composefs/boot.rs`. This is unchanged on
bootc `main`, so it is not a version to wait out. bootc-image-builder exposes no
`--composefs-backend` flag, and that backend is otherwise only auto-selected for a unified kernel
image, which the Tilo kernel is not.

So `bootloader = "systemd"` is not a slower path or a fallback — it is an image that cannot be
installed, on a VM or on hardware.

The near-miss worth recording: the image *also* has inconsistent bootupd state, and it looks like
the cause. `rpm -e --nodeps` of `grub2-*`/`shim-x64` (P1-1) empties the payload dirs under
`/usr/lib/efi/<component>/<rpm_evr>/EFI/` while
`/usr/lib/bootupd/updates/EFI.json` still reads `grub2-1:2.12-60.fc44,shim-16.1-5`. But repairing
that metadata cannot fix the install, twice over: the bail happens before bootupd is consulted at
all, and bootupd 0.2.35's EFI component knows only grub2 + shim — `bootupctl backend
generate-update-metadata` re-derives the *same* grub2+shim names from the emptied directories and
exits 0. There is no metadata that describes systemd-boot for it to write.

Why it went undetected for a month: the container image built, linted, signed, and pushed cleanly,
and that is all CI does. Nothing in CI installs the image, so CI was green throughout. The
verification failure it caused was read as a VM problem.

This is the reason `P1-2` ends with *"Not yet confirmed by booting an encrypted install"*, and the
reason a missing initramfs survived so long unnoticed: nothing ever got as far as installing, so no
boot-chain claim in this file — Secure Boot, `lockdown=integrity`, PCR binding, LUKS unlock — had
ever been observed on a running system.

**Fix direction:** the bootloader has to be one bootc's ostree backend can plant, which today means
GRUB via bootupd, which means `grub2` + `shim-x64` stay in the image rather than being deleted after
the fact. Note that "put them back" is *not* a `dnf install`: they are inherited from the
fedora-bootc base, and once this file stopped installing them they also left the frozen RPM
snapshot, so the fix is to stop removing them. Keeping systemd-boot is possible only by moving to
bootc's composefs backend — a change to how the root filesystem is composed, not a bootloader
swap, and a decision to take deliberately.

**Confirmed against a real install.** With the GRUB stack restored and `bootloader = "grub"`,
bootc-image-builder runs to `Build complete!` and writes a qcow2: bootc reports `Bootloader: grub`,
`Installing bootloader via bootupd`, `Installation complete!`, and the ESP carries
`EFI/fedora/{shimx64,grubx64,mmx64}.efi` + `grub.cfg` + `EFI/BOOT/BOOTX64.EFI`. That disk boots
under OVMF through GRUB 2.12 into the Tilo kernel (`Linux version 7.0.14 (tilo@tilo-build)`, with the
`kargs.d` hardening set applied) and reaches `multi-user.target` and `graphical.target`, with
`greetd` and `fs.service` up. An encrypted install driven from that booted image
(`os/dev/test-encrypted-install.sh`) also completes — LUKS2 root, `rd.luks.uuid=…tpm2-device=auto`
kargs, kernel and initramfs both on `/boot` with an `initrd` line in the BLS entry — and the
resulting disk boots far enough to run early userspace, find the LUKS device, start
`systemd-cryptsetup@…`, and ask for the unlock secret. So the initramfs and the LUKS unlock path
are real, and `P1-2`'s "`--encrypt` and the custom kernel appear mutually exclusive" is now
disproved rather than unverified.

**What is still not proved: the root filesystem opening.** Both encrypted runs stop at the unlock
prompt and stay there, and the reason is enrolment, not the boot path. An offline `cryptsetup
luksDump` of the `--no-tpm --passphrase hunter2` target shows the LUKS2 header carries **exactly one
keyslot**, keyslot 1, bound to token `0: systemd-recovery`:

```
Keyslots:
  1: luks2
	PBKDF:      pbkdf2
Tokens:
  0: systemd-recovery
	Keyslot:    1
```

There is no passphrase keyslot, so typing `hunter2` at the boot prompt is rejected and the prompt
repeats — observed directly. Three separate defects sit behind that, recorded below as `P1-2a`,
`P1-2b` and `P1-2c`. All are distinct from this finding, and all three have a single common
character: **each one reported success, or reported nothing, while doing nothing.**

Also worth confirming on real hardware: the `--tpm` run reached `Please enter recovery key` rather
than unsealing, having logged `Expecting device dev-tpm0.device`. PCR 7 measures Secure Boot policy
and the volume is sealed inside the installer host then opened on a different machine boot, so a
broken seal is the expected outcome of sealing across that boundary rather than evidence about a
real machine. `P1-2a` supplies a second, simpler explanation that has to be excluded first: on that
run **no TPM key slot was ever enrolled**, because enrolment exited before reaching the TPM branch.
Falling back to the recovery key is the correct behaviour for a volume with no TPM slot.

**Superseded.** The fix direction above (keep GRUB on the ostree backend) is not what shipped.
Tilo moved to bootc's composefs backend, which is the one that can plant systemd-boot
([composefs-decision.md](composefs-decision.md)): `os/usr/lib/bootc/install/40-tilo.toml` sets
`bootloader = "systemd"` again, `os/Containerfile` still removes shim and the GRUB stack, the
image builds a UKI with `bootc container ukify`, and `os/dev/disk-image.sh` installs with
`systemd-repart` plus `bootc install to-filesystem` instead of bootc-image-builder. The
"Confirmed against a real install" evidence above was taken on the earlier GRUB/ostree image and
is not evidence about the current one; current boot evidence is in
[boot-chain.md](boot-chain.md).

---

## P1-2a — `enrol` exits after the recovery key, so every later factor is silently skipped

**FIXED.** `os/usr/libexec/tilo-disk-encrypt`, `cmd_enrol`. The recovery key was read back out of
`systemd-cryptenroll` by matching eight groups of six lowercase letters:

```bash
rec_key="$(printf '%s\n' "$rec_out" \
  | grep -oE '[a-z]{6}-[a-z]{6}-[a-z]{6}-[a-z]{6}-[a-z]{6}-[a-z]{6}-[a-z]{6}-[a-z]{6}' \
  | tail -1)"
```

systemd emits eight groups of **eight** — `ngfnerci-tejvujuv-dkhcejht-jbkvnuhu-…` — so the match
never succeeded. The script runs under `set -euo pipefail`, and a command substitution in an
assignment carries the pipeline's status, which `pipefail` sets to `grep`'s 1. **The assignment
therefore terminated `enrol`**, three lines after the recovery key was enrolled and before any
other factor. The `[ -n "$rec_key" ] || rec_key=…` fallback on the next line was unreachable, which
is why it never compensated.

Consequences, all observed:

- No passphrase key slot on the no-TPM path, so `--passphrase`/`--pin` was a silent no-op and the
  disk could not be opened by the secret its owner had just been asked to invent.
- No TPM key slot on the TPM path either — the `--tpm` run's fallback to the recovery key was not a
  broken PCR seal, it was an absent slot.
- The recovery key was never printed, so the one factor that *was* enrolled was unknown to anyone.
- `finish` then removed the throwaway install slot, as designed, leaving a single slot bound to a
  `systemd-recovery` token whose value had never left the process that generated it.

Reproduced exactly against a throwaway LUKS2 volume, using the shipped script and a container's
absent `/dev/tpm*` as the no-TPM machine: `enrol` printed `enrolling the recovery key…`, exited 1,
left two slots (install + recovery), and `cryptsetup open --test-passphrase` rejected the
passphrase that had been supplied. After the fix: exit 0, three slots, and both the passphrase and
the printed recovery key open the volume.

The fix reads the key off **stdout**, where `systemd-cryptenroll` puts the key and nothing else,
instead of pattern-matching the prose on stderr, and verifies each factor with `cryptsetup open
--test-passphrase` before reporting it enrolled. If the recovery value cannot be read back or does
not open the volume, enrollment now fails while the temporary install key still exists; finishing
without a usable off-device recovery factor is not allowed. It also normalises the pin file's bytes
— a key file is taken as the passphrase verbatim, so a trailing newline would enrol a secret that
cannot be typed at a boot prompt.

**Regression check:** `os/tests/luks_enrolment.sh`, wired into `os/Containerfile` so it runs at
image build time. It asserts the *opening*, not the exiting: the supplied passphrase opens the
volume, the printed recovery key opens the volume, and three slots exist before `finish`. A build
container has no `/dev/tpm*`, so the build exercises exactly the no-TPM path that was broken.
It fails 6 of its 8 assertions against the pre-fix script and passes all 9 against the fixed one.

---

## P1-2b — the harness discarded every step's exit status, and with it the recovery key

**FIXED.** `os/dev/test-encrypted-install.sh`. Every step ran as
`runscript … | tee "$OUT/0N-…" | tail -N`, and a pipeline's status is its last command's, so `tail`
made every step look successful. `enrol` had been exiting 1 for as long as `P1-2a` existed and the
run carried on to install, `finish`, and boot a target with no usable unlock factor.

The same step is the only one that prints the recovery key. With the step dying after its first
line and no passphrase slot enrolled, the finished target had no factor anyone could use — the run
destroyed the only evidence of the one factor it had managed to enrol.

Replaced with a `step` helper that keeps the full output in a file, shows a tail, and **returns the
step's own status**; every caller now stops on failure with a message naming the artefact. The
recovery key is extracted from the enrol output, saved to `$OUT/recovery-key.txt`, and printed in
the verdict.

**Regression check:** the enrol step is now `step … || die`, so a non-zero `enrol` fails the run
instead of being absorbed. `os/tests/luks_enrolment.sh` catches the underlying cause without a VM.

---

## P1-2c — the harness reported absence when it was watching the wrong surface

**FIXED.** `os/dev/test-encrypted-install.sh` waited for the unlock prompt by grepping the serial
log, but the installed kernel gets no `console=` karg, so the serial line carried the GRUB menu and
then nothing at all — the whole 8 KB log was firmware output. The run printed
`NO passphrase prompt within 150s` about a system that was sitting at its passphrase prompt, and
that false finding is what sent the previous investigation to the boot chain instead of to
enrolment.

This is the defect to be most careful about: a harness that cannot distinguish *did not happen*
from *I could not see it* produces false findings, and this one did.

Three changes, because instrumenting the log is not by itself enough:

1. The installed boot entry is instrumented with `console=ttyS0,115200 console=tty0`. Order is
   load-bearing — `ttyS0` first so the kernel and systemd copy their log to the serial line, `tty0`
   **last** so `/dev/console`, where the prompt is written and where a key typed through the
   monitor arrives, stays the VGA console.
2. Before drawing any conclusion the run checks whether a kernel marker reached the serial line at
   all, and reports `CANNOT OBSERVE` rather than absence when it did not.
3. The verdict no longer rests on spotting a prompt. It is taken from whether the installed system
   can be **logged into**, which is a fact about the system rather than about what the harness
   managed to see, and the proof step then reads the running system's own `cryptsetup luksDump`.

Also fixed here: `--no-initrd` expanded to a shell string the guest ran as a command name, so the
`initrd` line was never removed and the mode could not have reproduced the pre-initramfs state it
exists to reproduce. It now edits the entry guest-side and asserts the line is gone.

**Regression check:** the harness asserts its own instrumentation — it fails the layout step if
`console=ttyS0` is absent from the installed entry, and fails the run if `--no-initrd` was asked
for and the `initrd` line survived. Neither can now pass while being unobservable.

**Not yet proved end to end.** The three-mode boot proof (`no-tpm`, `noinitrd`, `tpm`) has not been
run against these fixes, because the image build on the development host currently fails before it
produces an installable image — see `P1-2d`. `P1-2a` is proved at the source against a real LUKS2
volume; `P1-2b` and `P1-2c` are proved structurally, not through a boot.

---

## P1-2d — the image build corrupts its own rpmdb, reproducibly, and blocks every boot proof

**MITIGATION IMPLEMENTED; full pinned-image proof pending.** This is unrelated to disk encryption
other than by blocking its verification. Four consecutive historical `podman build` runs of
`os/Containerfile` on the development host failed in the OS assembly stage, three of them at the
same step:

```
Transaction failed: Rpm transaction failed.
  - avahi-libs(x86-64) = 0.9~rc2-8.fc44 is needed by avahi-glib-0.9~rc2-8.fc44.x86_64
  - fuse3 is needed by xdg-desktop-portal-1.22.1-1.fc44.x86_64
Error: building at STEP "… dnf install -y … xdg-desktop-portal …"
```

dnf's resolver and librpm's transaction check disagree about what is installed. In the last good
layer, `rpm -q fuse3` reports `fuse3-3.18.2-1.fc44.x86_64` while
`rpm -q --whatprovides fuse3` reports `no package provides fuse3`, and `rpm -Va fuse3` lists
`fuse-common` and `fuse3-libs` as unsatisfied although both are installed. The package records are
present; the **Provides index rows for the packages installed a step earlier are missing**.

The layer carries a stale `rpmdb.sqlite-shm` beside a zero-length `rpmdb.sqlite-wal` — sqlite
WAL-mode sidecar files that should never be committed into an image. The signature fits a WAL that
was not checkpointed before podman committed the layer: rows already in `rpmdb.sqlite` stay
readable, rows still in the WAL are lost. Which packages lose their Provides varies between runs,
which is what an unflushed-buffer race looks like rather than a deterministic bug.

Excluded: the pinned base image is intact (`--whatprovides` resolves there);
`--source-date-epoch`/`--rewrite-timestamp` are not the cause (the failure reproduces without them,
and the `.......T.` entries in `rpm -Va` appear in the pristine base too); a single dnf step under
those flags does not corrupt anything; and it is not the disk, which had 38 GiB unallocated and
12 GiB of free memory on the final attempt. `rpm --rebuilddb` does not repair it and cannot — it
rebuilds indexes from headers that are themselves missing.

The near-miss worth recording: this is the failure mode `os/dev/build-image.sh` already documents
as *"a disk that fills mid-build can leave a cached layer holding a corrupt RPM database, which then
surfaces as dnf insisting that packages the base image plainly has are missing"*, with
`TILO_BUILD_ARGS=--no-cache` offered as the way out. `--no-cache` does **not** help: it prevents
inheriting a corrupt layer but not producing one.

**Implemented mitigation:** every rootfs RPM transaction now ends in
`os/scripts/finalize-rpmdb.sh`. It rebuilds the SQLite database onto a fresh inode, verifies it,
proves the file index still maps `/usr/bin/basename` to `coreutils`, and removes WAL/SHM only after
the last RPM reader exits. The finished image repeats the database/file-index checks and rejects
either sidecar. A clean Fedora 44 container probe on 2026-09-17 exercised the helper against the
real RPM backend and passed the rebuild, ownership lookup, sidecar-absence, and `rpm --verifydb`
checks.

What remains unproved is the original failure boundary: a complete Tilo image assembly followed by
the encrypted three-mode boot proof. Local assembly currently stops earlier because the pinned
Tilo RPM/kernel registry requires credentials unavailable to the development container; CI remains
the authoritative end-to-end exercise. Do not call this defect closed until that build crosses
multiple layer boundaries and boots the resulting image.

---

## P1-1 — Secure Boot is removed from the image, and dependent hardening is inert

`os/Containerfile:498` runs `rpm -qa | grep -E '^(shim-x64|grub2-)' | xargs -r rpm -e --nodeps`,
deleting shim and the GRUB stack, then installs `systemd-boot-unsigned` (`:497`). The kernel is
swapped for a locally built one with `--nogpgcheck` (`:640-648`). No `sbsign`/`pesign` exists
anywhere in the tree.

Knock-on effects:

- `lockdown=integrity` (`os/usr/lib/bootc/kargs.d/01-hardening.toml`) never engages — the kernel
  enters lockdown from firmware only under Secure Boot.
- TPM PCR 7 measures Secure Boot state, so binding LUKS to it carries no meaning as configured.
  No `--tpm2-pcrs` is set anywhere; the `bootc` default policy is inherited unexamined.
- `keys/tpm2-pcr-signing.public.pem` is baked to `/usr/share/tilo/` (`os/Containerfile:427`) and
  read by nothing.
- `docs/security/boot-chain.md` describes a chain — *"firmware → shim (MS-signed) → GRUB (Fedora-signed) →
  kernel (Fedora-signed)"* — that the image explicitly deletes. The document is stale and
  actively misleading.

**Superseded by owner-enrolled Secure Boot; hardware proof open.** The image still removes shim and
GRUB and installs `systemd-boot-unsigned` (`os/Containerfile`), so nothing is signed by Microsoft
or Fedora. Instead the owner's keys sign the boot chain: `os/usr/libexec/tilo-secureboot`
generates keys, signs, enrolls the keys and seals the db key, and the `tilo-secureboot-sign`
path, timer and shutdown units re-sign after an update. The UKI is built with
`bootc container ukify`, and `tpm2-pcr-signing.public.pem` is installed by the `91tilo-rollback`
dracut module. Secure Boot, and with it the firmware-driven protections above, is therefore
active only after the owner runs `tilo-secureboot enroll`; a machine that never enrolls boots
unsigned. "No `sbsign`/`pesign` anywhere in the tree" is no longer true.
[boot-chain.md](boot-chain.md) now describes this chain. Per-device firmware enrollment needs a
hardware matrix.

---

## P1-2 — Disk encryption is opt-in, TPM-mandatory, and conflicts with the initrd-less kernel

`os/usr/libexec/tilo-install:276-289` — `BLOCK_SETUP` defaults to `direct`; encryption requires
`--encrypt`. `tilo-install:176-178` aborts if neither `/dev/tpmrm0` nor `/dev/tpm0` exists, so there
is **no passphrase-only encryption path** — a machine without a TPM gets no encryption at all.

Separately, `os/etc/kernel/install.conf` sets `initrd_generator=none` and `os/Containerfile:654`
does `rm -f /boot/initramfs-*.img`. A LUKS root needs early **userspace** to unseal the TPM and open
the mapping; static drivers let the kernel mount root but cannot run `systemd-cryptsetup`. No
`crypttab`, dracut config, or conditional re-enable exists in `os/`.

`--encrypt` and the custom kernel therefore appear mutually exclusive. ~~Not yet confirmed by
booting an encrypted install~~ — **DISPROVED.** An encrypted install of the image boots through
GRUB 2.12 into kernel 7.0.14, and dracut's early userspace runs: it finds the LUKS device by
`rd.luks.uuid`, starts `systemd-cryptsetup@luks-…`, and asks for the unlock secret. A kernel that
could not run early userspace could not have reached that prompt, so the claim is false. It was
never tested together with `--encrypt`, which is the actual finding — see `P0-4` for why nothing
was ever installed, and `P1-2a`/`P1-2b`/`P1-2c` above for the enrolment and harness defects that
kept it looking true for another week after `P0-4` was fixed.

The enrolment sequence had been "verified against loopback LUKS + swtpm". That verification
asserted the commands exited, not that the resulting volume opened by the secret it had just been
given — which is exactly how `P1-2a` survived it, and why its replacement
(`os/tests/luks_enrolment.sh`) asserts the opening rather than the exiting.

**Resolved in source.** Every native install is encrypted: `os/usr/libexec/tilo-install` sets
`ENCRYPT=1` unconditionally and refuses to run without `--pin`, the startup secret. A TPM is no
longer required: the short-PIN TPM2 policy is used only when a TPM is present and
`tilo-verified-boot-install-preflight` passes, and otherwise the same input is enrolled as an
ordinary strong passphrase, checked by `tilo-startup-secret-policy`. `os/etc/kernel/install.conf`
no longer exists; the image builds its initramfs with an explicit dracut call in
`os/Containerfile`, carrying the `91tilo-rollback` and `92tilo-sibling-unlock` modules. The
enrolment itself is held by `os/tests/luks_enrolment.sh`, which runs during the image build and
asserts that each factor opens the volume. A boot of the current image to an opened root on real
hardware is not recorded here.

---

## P1-3 — MLS does not rekey on member removal

`crates/collab/channel-crypto/src/group.rs:399-404` (as found) — `remove_member` is a `todo!()`. The design
guarantee is that the group key rotates on *every* membership change; removal is the case that
matters, and it panics. The in-source workaround is "mint a fresh channel and invite only the
desired members".

Historically `channel-crypto` was a **dev-dependency** of `shell`, so MLS was
compiled only for tests and absent from application endpoints.

**Library defect resolved; production integration remains open.**
`ChannelGroup::remove_member` now creates and merges an OpenMLS removal commit
and returns its serialized bytes for fan-out. Existing members process the same
commit, and the removed member becomes inactive after processing it. The
two-member convenience admission now refuses once an existing peer would need
a commit, so multi-party admission cannot silently discard epoch state. Held by
`third_member_joins::members_who_miss_or_are_removed_by_a_commit_cannot_decrypt_after_it`
(phase `removal_commit_advances_survivors_and_excludes_removed_member`) and
`third_member_joins::a_third_member_joins_and_everyone_can_talk` (phase
`multi_party_add_cannot_drop_the_commit`). Production brokers now validate
route-bound signed metadata and relay opaque MLS payloads byte-for-byte, while
`ui::collaboration` can encrypt/decrypt after a trusted caller installs the
exact route's `ChannelGroup`. The crypto layer now supplies vault-key-sealed
atomic OpenMLS state, signed revision-bound governance frames, a durable
targeted Welcome/commit/removal journal, partial-failure rollback, and a
snapshot-acknowledged plaintext gate. The shell now has a broker-internal,
handle-only workspace authority transaction with idempotency and crash recovery
tying those primitives to role admission/revocation. Production broker startup
installs the Secret Service provider with no automatic file-key fallback; a sealed owner-only
file store is used only when `TILO_SECRETS_DIR` is set explicitly (headless machines;
`crates/platform/tilo-secrets/src/lib.rs`, acknowledged in
`crates/session/shell/src/shell/mls_vault.rs`). Its document
store fsyncs encrypted compaction snapshots before minting route-bound receipts;
restart, wrong-route and tamper behavior are covered by hermetic provider tests.
The remaining work at the time was user-facing device/member provisioning and
recovery UX, plus an authenticated peer-control adapter that drains and
acknowledges the already durable targeted governance outbox. The ciphertext
document relay is intentionally not that adapter; it must never broadcast
Welcome bytes.

**Production adapter now present.** `crates/collab/tilo-action-broker/src/channel_encryption.rs`
runs the steward for every channel a device governs: it reconciles each MLS group with the room's
access policy, so an invite, a removal, a role change that ends access or a revoked device becomes
a commit (`Steward::reconcile`, which stages removals through `ChannelGroup::stage_remove`), and it
redelivers the steward's outbox over the room transport (`NeighbourMsg::MlsGovernance`) with
backoff until each device acknowledges. Held by that module's
`an_announced_revocation_is_enforced_at_once_and_idle_stewards_park`,
`a_new_channel_is_encrypted_from_creation_and_follows_the_policy`,
`delivery_survives_a_network_outage` and
`observing_live_keys_never_opens_a_channel_or_rejoins_after_removal`, plus
`channel-crypto/tests/content_history.rs::removed_member_cannot_read_content_sealed_after_removal`.
The steward admits per record, a Deny over an app or a pattern of records included; see `P1-11`.
Provisioning and recovery UX were not assessed by this reconciliation.

---

## P1-4 — The signed-op validator is never fed in production

The 8-step (extended to 9) validation pipeline in
`crates/collab/collab-types/src/validator.rs:589` is implemented and thoroughly tested, and the
broker's default state is correctly fail-closed (`crates/session/shell/src/shell/collab_broker.rs:797-800`: an
empty `authorised_signers` rejects every signed op).

But `register_channel_state` has no non-test caller, and no production path mints a
`RoleAttestation`. `CollaborationManager::set_identity` / `with_identity` likewise have no callers
in `crates/session/shell/`, `crates/sdk/ui-core/`, or `apps/`. Roles are therefore advisory in practice.

`Space` (`crates/collab/collab-types/src/space.rs`) has zero consumers outside its own crate and
tests. A channel-browser widget with no shell call site has since been removed.

**Resolved.** Both production ingress paths now call
`validate_admitted_inbound_op` before recording or fanning out an operation.
The network broker validates the certified route and current workspace role;
the Cap'n Proto broker uses the independently admitted join identity and the
same artifact state. Invalid signatures, live operations dated outside the
five-minute clock window, per-author sequence replays, route mismatches, role
violations and already-redacted targets are rejected before publication.
History a workspace host replays from its retained, host-ordered log is
admitted at any age (`validate_replayed_inbound_op`; only a future date is
refused), because history is old by nature; the per-author sequence still
refuses a replayed duplicate. The old `register_channel_state` observation described a previous
bootstrap shape; current authority is populated from admitted workspace state
at the ingress boundary instead of trusting an app-supplied role attestation.
Held by the broker ingress tests in
`crates/collab/tilo-action-broker/src/{collab_broker.rs,collab_broker_capnp.rs,collab_inbound.rs}`
and `crates/session/shell/src/shell/collab_broker.rs`, plus
`crates/collab/collab-types/tests/validator.rs`.

---

## P1-5 — No lock on suspend; idle auto-lock off by default

`crates/system/power/src/service.rs:255-265` handles `PrepareForSleep`: the `true` branch only logs.
Nothing locks the session before sleep or on resume, so suspend-then-open-lid lands in an unlocked
session. `crates/system/power/src/logind.rs:19-20` documents this as working — *"so the compositor
can lock before the machine sleeps"* — which is not implemented.

`crates/session/shell/src/compositor/idle.rs:107-113` defaults `lock_timeout` to `None`; idle auto-lock is
reachable only via `TILO_IDLE_LOCK_SECS`.

**Resolved.** The power service maps `PrepareForSleep(true)` to
`PowerSignal::PreparingForSleep` (`crates/system/power/src/service.rs`), and the compositor
answers it with a mandatory lock before sleep (`IdleState::prepare_for_sleep` in
`crates/session/shell/src/compositor/idle.rs`); the resume edge resets idle state. The compositor
defaults both blanking and locking to five minutes; an explicit `TILO_IDLE_LOCK_SECS=0` remains the
documented opt-out for idle locking only. Unit coverage pins the sleep-edge mapping and secure
default. `crates/session/shell/tests/session_lock_e2e.rs`, a module of the shell's `it` test
target, drives idle lock plus failed and successful unlock through the live loop, but it is
`#[ignore]`d because it needs a display, so default CI does not run it
(`cargo test -p shell --test it session_lock_e2e:: -- --ignored`).

---

## P1-6 — TOTP vault key still sits beside its ciphertext

The accounts vault half is closed: production account-vault keys now prefer
the freedesktop Secret Service, migrate a verified local key into it, and
remove the adjacent copy only after a read-back match. A headless environment
without Secret Service retains a visibly reported owner-only fallback.

The authenticator vault is closed too: normal desktop sessions keep its key in
Secret Service, verify a migrated key by reading it back, and only then remove
the adjacent owner-only fallback. Headless/compatibility sessions retain the
same explicitly documented fallback rather than silently losing the vault.

The identity defect is closed: schema-1 plaintext records migrate to public
metadata plus Secret Service storage (or a truthful host-only 0600 fallback),
and apps receive an app-scoped signing socket rather than a mounted key file.

**Resolved by redesign.** The authenticator no longer has a vault file. Each account's seed is
its own item in the app's secret store (`apps/totp/src/secrets.rs`, through
`crates/platform/tilo-secrets`), and the collaborative document names accounts without holding a
seed. The only file-backed store is the explicit `TILO_SECRETS_DIR` opt-in for headless machines.
Held by `apps/totp/src/secrets.rs::tests` (`seeds_live_under_the_account_id_and_leave_with_the_account`,
`a_refused_write_is_kept_for_the_session_until_a_retry_lands`).

Identity storage uses the standard libsecret `secret-tool` client when an
unlocked freedesktop Secret Service is available.

---

## P1-7 — The base-image digest pin expires in about a day, so cross-time reproducibility is half true

As found: `os/Containerfile:67,88,208`, `os/Containerfile.host:25,61`, `os/kernel/Containerfile:41`
— all pinned `FROM quay.io/fedora/fedora-bootc@sha256:…`. Digest pinning is the right instinct, but
Fedora rebuilds the `fedora-bootc:44` tag daily and garbage-collects the manifest it replaces
almost immediately, so the pin resolves for roughly one day.

Observed twice:

- A digest pinned on 2026-06-30 was gone by 2026-07-30 (`manifest unknown`), breaking every image
  build at its first `FROM` — including `snapshot-rpms`, which resolves the frozen package set
  against that same base. The failure is self-blocking: the package set could not be refreshed
  either.
- Repinned 2026-07-30 to `sha256:c466de53…`; `snapshot-rpms` succeeded with it (run 662). By
  2026-08-01 that one-day-old digest was also gone (run 726), confirmed independently with
  `skopeo inspect`.
- Meanwhile `main` carried a third digest, `sha256:a0fc5a3b…`, and every `build-image` run against
  it on 2026-08-01 failed the same way (runs 722, 724, 727, 730, 733, 736, 738). The OS image had
  not been buildable from `main` all morning, and nothing about it was specific to any of those
  commits.

`docs/supply-chain.md` claims builds are reproducible across time. `set-<fingerprint>` resolution +
`os/Containerfile.rpms` genuinely deliver that for the package set, but the base underneath
evaporates, so a rebuild weeks later does not merely differ — it does not run. Supply-chain
integrity rests on the pin being real rather than aspirational: a pin that reliably fails is one
"just point it at whatever resolves today" away from becoming an unpinned build under deadline
pressure, and the frozen RPM set would then be resolved against an unrecorded base.

**Fix direction:** mirror the base into `registry.tilo.so/tilo/fedora-bootc` and pin against the
mirror — the same shape `os/Containerfile.rpms` + `snapshot-rpms` already use for packages, since
our registry collects nothing. Copy with the multi-arch index intact. Fixed on this branch: the
`mirror-base` workflow plus `os/base-snapshot.env` (`BASE_REF`), consumed by every Containerfile
as a `--build-arg`. The workflow has since been replaced by `os/dev/ensure-base-mirror.sh`;
`BASE_REF` in `os/base-snapshot.env` still pins `registry.tilo.so/tilo/fedora-bootc` by digest.

Two adjacent holes in the same guarantee were found alongside it:

- `os/containers/tilo-relay/Containerfile:74` (now around `:149`; **still floating** on current
  source) builds its runtime stage `FROM quay.io/fedora/fedora-minimal:44`, a floating tag — it never breaks, it just silently changes,
  so the relay image is not reproducible across time. **Still open**, and not a one-line pin:
  `BASE_REF` is the bootc base, ~1 GB with a kernel in it, so reusing it for a runtime stage whose
  whole point is to be small is not the fix. A digest against quay.io expires the same way the
  bootc pin did, so this needs its own mirrored image — and `mirror-base` cannot mirror it as
  written (it hardcodes the `tilo/fedora-bootc` destination and asserts an `ostree.linux` label
  fedora-minimal does not carry). Parameterise that workflow over source/destination, mirror
  fedora-minimal, and add a second ref to `os/base-snapshot.env` that
  `.forgejo/workflows/release-relay.yml` passes alongside `BASE_REF`.
- `os/Containerfile`'s `wpe-builder` stage installs its build deps with `dnf builddep
  webkitgtk6.0` against live Fedora repos, a deliberate exception noted in the file. That much is
  still open. (**Closed by removal:** the browser moved from WPE to a Chromium engine and the
  `wpe-builder` stage no longer exists; the history below is kept as written.) The stage was also **entirely broken**, and unpinned turned out to mean unwatched in
  the strongest sense: nobody noticed, because the base `FROM` above failed first and the layer
  cache served the stage before that. Once the base pin was fixed and the stage actually ran, it
  had five independent defects stacked behind one another, each only visible after the previous
  was fixed. The image now builds end to end — run 811, signed and pushed.

  1. `build-wpe.sh` exported `PKG_CONFIG_SYSROOT_DIR=/wpe` to bridge the staged installs' unstaged
     `${prefix}`. A sysroot is a blanket rewrite — pkgconf prepends it to every `-I`/`-L` it
     emits — so system libraries in the real `/usr` were relocated too and libwpe's CMake configure
     died on `Imported target "XkbCommon::libxkbcommon" includes non-existent path
     "/wpe/usr/include"` (run 742). Now `PKG_CONFIG_RELOCATE_PATHS`, which derives each `${prefix}`
     from its own `.pc` file's location and so relocates only what was actually staged.
  2. libwpe's `wpe.pc.in` hardcodes `libdir=${exec_prefix}/lib` while the library installs into
     `CMAKE_INSTALL_LIBDIR` (`lib64`). An unstaged install survives that because the linker
     searches `/usr/lib64` by default; a staging root does not, and wpebackend-fdo failed to link
     with `cannot find -lwpe-1.0` (run 754). The installed `.pc` is now corrected.
  3. The WPE port's `ENABLE_WPE_PLATFORM_DRM` needs libinput and libudev, which the *GTK* port's
     builddep never pulls, so `find_package(LibInput)` failed (run 755). Added to the explicit
     package list the stage already keeps for this purpose.
  4. With the engine finally built, the Rust builder could not find `wpe-webkit-2.0` (run 758).
     The `.pc` was present; it declares `Requires: glib-2.0 libsoup-3.0 wpe-1.0 wpe-platform-2.0`,
     and pkg-config fails the whole lookup when one link in that chain has no `.pc` — reported by
     the pkg-config crate as the requested module being missing. The image ships `libsoup3` for
     the browser at runtime but the builder never installed `libsoup3-devel`.
  5. Linking `browser` then failed on hundreds of undefined `gst_*`/`epoxy_*`/`manette_*` symbols
     (run 773). The engine is *not* under-linked — ld says so first: `libgstreamer-1.0.so.0,
     needed by /usr/lib64/libWPEWebKit-2.0.so, not found`. 17 sonames, nine packages, absent from
     the builder.

  Each was found only by running the build; none is visible by reading it. Defect 5 also exposed a
  latent runtime bug that no build would ever have caught: four of those libraries — `atk`,
  `at-spi2-atk`, `libmanette`, `libxslt` — were missing from the *final image* too, so the browser
  would have failed to exec on a booted system.

  Guards now fail at the point of the mistake rather than somewhere downstream: every staged
  package must resolve to an include directory that exists *and* link a trivial program; each
  build step dumps its real output on failure instead of the `tail -1` that had been hiding the
  compiler error behind ninja's "build stopped"; and the hour-long engine compile streams progress
  rather than going silent, which had made three runner-level deaths (a Docker daemon that went
  away mid-build) indistinguishable from a hang.

## P1-8 — `catch_unwind` soft-fails are inert in release, so the compositor aborts at startup with no GPU adapter

`crates/sdk/ui-core/src/warmup.rs:191`, `crates/session/shell/src/compositor/native.rs:707`,
`crates/session/shell/src/compositor/backend/headless.rs:77` (line numbers as found; now fixed)

Three call sites wrapped `ui::offscreen::OffscreenRenderer::new` in `std::panic::catch_unwind` to
degrade gracefully when wgpu finds no adapter. `[profile.release]` sets `panic = "abort"`
(`Cargo.toml:447`) — deliberately, and correctly, since a compositor cannot meaningfully recover
from a panic — under which the panic runtime aborts at the panic site and no unwinding, hence no
catching, ever begins. All three guards were inert in exactly the configuration that ships, and
`OffscreenRenderer::new`'s own doc comment told callers to use the mechanism that does not work.

The load-bearing one is `ui::warmup::spawn_warmup`, called on the compositor's real startup path
(`crates/session/shell/src/main.rs:129`) from a `tilo-startup-warmup` thread. Its comment claimed *"A missing
adapter isn't fatal to the shell — the compositor's real backend surfaces its own adapter error
later — so we swallow it here."* In a release build nothing is swallowed: on a machine where
adapter enumeration fails the shell aborts before it ever reaches the backend that was supposed to
report the problem. The dev build degrades; the shipped build does not. The other two are the
test-control snapshot writer and the headless backend's renderer init, both documented as
never-panicking and neither of which was.

Found by verifying a claim, not by reading one: the resilience comment was checked against the
panic strategy the release profile actually sets.

**Fix direction (applied):** `OffscreenRenderer::try_new` returns `OffscreenError` instead of
panicking; `new` is retained for its many test/example callers and panics through it. All three
sites take the `Result`. `panic = "abort"` is unchanged. Pinned by
`warmup_degrades_silently_when_no_wgpu_adapter_exists` (`crates/sdk/ui-core/tests/warmup_no_adapter.rs`), which
re-executes itself as a child with an empty wgpu backend set and an aborting panic hook — cargo
ignores `panic` on the test profile, so the strategy has to be emulated — and fails if the child
dies by signal rather than exiting. It first asserts no adapter is reachable, so it cannot pass
vacuously on a GPU host. Against the previous code the child dies with SIGABRT.

**Current source.** The fallible constructor is taken in
`crates/sdk/ui-core/src/native/warmup.rs`, `crates/session/shell/src/compositor/native.rs` and
`crates/session/shell/src/compositor/backend/headless.rs`, and the compositor reaches
`ui::warmup::spawn_warmup` from `crates/session/shell/src/linux_main.rs`. `[profile.release]` has
since changed to `panic = "unwind"` so the compositor can restart its collaboration broker, but
`tilo-rt` installs a hook that aborts every panic outside
`tilo_rt::panic_boundary::isolate` (`crates/platform/tilo-rt/src/panic_boundary.rs`), so a bare
`catch_unwind` is still not a recovery path and `try_new` remains the fix.

**Residual:** `OffscreenRenderer::render` still `expect`s internally (`crates/sdk/ui-core/src/offscreen.rs`, the
`render_to_texture` and buffer-map paths). Those were nominally inside the same dead
`catch_unwind`s, so nothing regressed, but a mid-render driver failure still aborts a release
build. The `catch_unwind` pair in `crates/collab/channel-crypto/src/group.rs:237,379` (now `:523`,
`:710`) was checked
and is *not* the same defect: it absorbs an openmls `debug_assert!(false, …)` that only fires in
debug builds, which is where `catch_unwind` works.

---

## P1-9 — A fetched namespace tree is not bound to the requesting workspace

`crates/system/fs/src/sync.rs`, `sync/session.rs`, `main.rs`

The namespace is one Automerge document per workspace, and nothing inside its bytes says which
workspace it is. Every path that accepts one from a peer checked the peer's certificate, but none
checked the tree against the workspace the ticket names:

- The sync ALPN (`tilo-fs-sync/0`) carried no workspace at all. `fetch_remote_doc` merged whatever
  document the peer sent, and both sides of `MODE_SYNC` fed any sync message into `sync_receive`.
  A home peer serves several workspaces under one device key, so a request could reach the wrong
  one, and a lying peer could serve any tree it held.
- A read-only device's per-request pull fetched the full document from any peer that had ever
  authenticated to it and merged it, with no check of the local policy at all.
- Bootstrap merged documents from every persisted content peer, including peers registered for
  another workspace and ticketless legacy entries.
- On the session plane only the server compared the request's `workspace_id`. The client applied
  whatever `Sync` and `NamespaceChunk` frames came back.
- The exact-record share client ignored the response's session, so a snapshot from a session on
  another workspace or record was taken as the invite's.

Signed changesets (`PullChangesets`) were already bound: the signed header carries the workspace
and `apply_changeset` refuses a mismatch.

**Resolved.** Every namespace request names its workspace (`tilo-fs-sync/1`,
`tilo-fs-session/2`). Every namespace payload, whether a document, the changes beyond a set of
heads or a sync message, travels with a `NamespaceBindingV1`: the sending device's signature over
the workspace, the payload kind and the digest of the exact bytes. Receivers on both sides apply a
payload only after `sync::accept_namespace_payload` has accepted it. That check requires all of:

- the binding names the requested workspace;
- it was signed by the authenticated peer's own device;
- the bytes match the signed digest;
- the local policy lets that peer read the whole workspace (write it, for sync).

A host refuses a request for another workspace explicitly. Bootstrap takes the tree only from the
ticket's node and from persisted peers registered for the same workspace. The share client refuses
a snapshot whose session is not the invite's workspace and record. Held by
`crates/system/fs/src/namespace_binding.rs::tests`, the
`crates/system/fs/src/sync/tests.rs` tests from
`a_document_of_another_workspace_is_refused_although_validly_signed` to
`read_only_member_never_downloads_full_snapshot` (which replaced
`a_read_only_pull_merges_only_a_members_document_of_this_workspace`),
`crates/system/fs/src/sync/session/tests.rs` and
`crates/system/fs/src/record_repository.rs::tests::a_validly_signed_changeset_of_another_workspace_is_refused`.

---

## P2-1 — `PeerId` is a 64-bit fold of the public key

`crates/session/shell/src/network/behaviour.rs:98-107` derives the wire `PeerId` as a 64-bit FNV-1a fold of the
public key, and it is used as the `PeerIdentityStore` key. 64 bits is collision-findable. Note the
newer `PeerOrigin` (`crates/session/shell/src/shell/state.rs:109-127`) correctly carries the full base58 peer id —
the fold is the older path.

**Resolved.** `PeerIdentityStore` keys on `PeerOrigin`. The `u64` that remains on the wire is named
`peer_liveness_handle`, never reaches an authority decision, and has one derivation shared by the
actor and the shell instead of two copies of `DefaultHasher` — whose output std does not promise
across Rust releases, so two peers on different toolchains would have stopped recognising each
other's publishes. Held by
`crates/session/shell/src/network/identity.rs::tests::identity_store_keys_entries_per_peer_and_rejects_invalid_ones`
(phase `distinct_peers_never_share_an_entry`),
`crates/session/shell/src/network/behaviour.rs::tests::device_key_persists_and_is_never_replaced`
(phase `liveness_and_salt_are_stable`) and
`crates/collab/neighbour-wire/src/lib.rs::tests::peer_handle_is_fnv1a_of_the_device_key`.

## P2-2 — Corrupt peer-key file silently rotates identity

`crates/session/shell/src/network/behaviour.rs:122-127` — if `/var/lib/tilo-fs/peer-key` exists but does not parse,
it is overwritten with a fresh keypair rather than raising. Identity loss becomes indistinguishable
from first run. The same function documents a known TOCTOU window between write and `chmod 0600`
(`:142-146`).

**Resolved.** An unreadable key file propagates as an error with the bytes left on disk, and the
compositor leaves peer sync off rather than syncing an unsalted id space. The key itself is now the
machine's one device key, so the compositor no longer keeps a network identity of its own.
The write goes through an atomic write-with-mode, closing the TOCTOU window. The key now lives at
`/var/lib/tilo/device-private/keypair.bin` (`DEVICE_KEY_PATH`). Held by
`crates/session/shell/src/network/behaviour.rs::tests::device_key_persists_and_is_never_replaced`
(phase `malformed_key_is_never_replaced`), `::created_device_key_is_not_world_accessible`, and
`crates/collab/tilo-user-identity/src/persist.rs::tests::device_keys_are_group_private_created_once_and_never_rotated_silently`.

## P2-3 — Revocation is modelled but undeliverable

`crates/collab/tilo-user-identity/src/revocation.rs` implements signed revocation entries with
correct before/after-cutoff semantics (tested at `crates/session/shell/src/network/identity.rs:364-406`), but
`PeerIdentityStore::set_revocations` has no non-test caller and there is no distribution mechanism.

**Resolved.** Each workspace heartbeat now starts with an `IdentityHello` carrying the sender's
root-signed device certificate and additive revocation set. The receiver verifies the endpoint
key, certificate, every revocation signature, expiry, and revocation status before admitting any
production collaboration message; a later revocation removes the peer immediately. Revocation
also withdraws this machine's exported filesystem authority and stops the app identity broker
from signing locally. Held by
`shell::network::tests::redeemed_invites_grant_exactly_their_signed_role_and_name`
(phase `exact_invite_redeems_as_editor_and_revocation_removes_trust`),
`shell::state::identity_fs_tests::filesystem_export_maps_verified_people_and_per_workspace_membership`,
and `crates/runtime/tilo-identity-host/src/broker.rs::tests::identity_service_wire_is_pinned_and_signing_requires_the_exact_capability`.

## P2-4 — Egress documentation describes a weaker mechanism than the code implements

`docs/app-model/sandbox.md:162-172` describes DNS-layer filtering that direct-IP connections bypass.
The code has no DNS proxy — `dns-only` resolves to `EgressDecision::Blocked`
(`crates/packaging/launcher/src/netgate.rs:241`) and address literals arrive as
`Destination::Addr` and are matched as literals, pinned by the test
`an_address_literal_never_matches_a_hostname_grant` (`netgate.rs:1112`).

The code is correct; the document is stale. Recorded because a stale doc describing a weaker
mechanism is how a strong one gets "simplified" later.

**Resolved.** `docs/app-model/sandbox.md` now says an address literal never satisfies a hostname
grant and describes no DNS filtering. The test is now the phase
`an_address_literal_never_matches_a_hostname_grant` of
`grants_narrow_but_never_widen_the_manifest_allowlist` in `netgate.rs`.

## P2-5 — `tilo-security-audit` cannot fail

`os/usr/libexec/tilo-security-audit` exits 0 unconditionally (`:5`, `:86`) and the unit sets
`SuccessExitStatus=0 1`. It reports; it never gates a boot. Its image-signing check greps
`policy.json` for the string `sigstoreSigned` (`:63-67`) rather than verifying anything.

**Resolved.** Required-invariant failures now accumulate and return a non-zero
health result. The unit is wanted rather than required, so this remains
diagnostic and cannot strand the user during boot, but system health, logs and
agents can no longer mistake a failed audit for success. The image-policy check
requires the Tilo registry scope, a `sigstoreSigned` rule, the pinned key path
and the installed non-empty verification key instead of accepting one unrelated
string. Held by `os/tests/kernel_hardening_invariants.sh`.

**Residual, resolved.** The image-policy check was three independent `grep -F` matches over
`policy.json`, not a parse, so a signed `code.tilo.so/tilo/tilo` or
`registry.tilo.so/tilo/legacy-guest` entry would have satisfied it while the
`registry.tilo.so/tilo/tilo` scope itself accepted anything. The audit now runs
`os/usr/libexec/tilo-image-policy-check`, which parses the policy and passes only when
`registry.tilo.so/tilo/tilo` and `code.tilo.so/tilo/tilo` each hold nothing but `sigstoreSigned`
requirements on `/usr/share/tilo/cosign.pub` with a repository-bound `signedIdentity`, and every
more specific scope under either Tilo namespace is that or `reject`. An unreadable, malformed or
duplicate-keyed policy fails. `os/tests/kernel_hardening_invariants.sh` runs it against the
shipped policy and eighteen weakened copies (each Tilo scope accepting anything, another key,
`signedBy`, a remapped identity, extra key material, a tag override, a removed scope, a removed
legacy-guest or development scope that leaves it to the permissive default, a permissive
namespace scope, malformed JSON, a duplicate scope, a missing file), every one of which must
fail. Separately, the audit's mem-init check required the kernel to report
`heap free:on` and so warned on every correct boot once `init_on_free` left the kernel
arguments; it now requires `alloc:on`, and `os/tests/kernel_hardening_invariants.sh` refuses the
old pattern.

## P2-6 — `insecureAcceptAnything` is the container policy default

`os/etc/containers/policy.json:2-4`. Signature enforcement covers the official Tilo
repositories only; every other image reference is unverified by the policy.

**Still open: awaiting the owner's decision to accept or tighten.** The default is still
`insecureAcceptAnything`. Current source requires a `sigstoreSigned` signature for
`code.tilo.so/tilo/tilo`, `registry.tilo.so/tilo/tilo` and `registry.tilo.so/tilo/legacy-guest`,
and rejects the `tilo-vm`, `tilo-ci` and `tilo-dev` repositories on both registries.
`tilo-image-policy-check` now also judges the legacy guest and those development repositories by
their *effective* rule, so removing one's scope (handing it to the permissive default) or adding
a permissive `registry.tilo.so/tilo` namespace scope fails the boot audit.

What the permissive default reaches, as of this entry:

- **No Tilo OS image path depends on it.** The installer installs the booted medium's image from
  local container storage, and the offline target verifies the cosign signature, docker reference
  and provenance itself before importing with `skopeo --insecure-policy`. The recorded update
  origin is restricted to `registry.tilo.so/tilo/tilo` or `code.tilo.so/tilo/tilo`
  (`os/dev/hardware_profile.py`), production `tilo os upgrade` refuses `--to`, and the legacy guest
  is fetched only by a compiled-in digest of `registry.tilo.so/tilo/legacy-guest`
  (`crates/runtime/legacy-vm/src/image.rs`). `tilo-install` does not pass bootc's
  `--enforce-container-sigpolicy`.
- **Only root can point bootc at another reference**; there is no polkit rule for bootc or
  rpm-ostree, and the wheel update unit takes fixed arguments.
- **Rootless podman, skopeo and distrobox** read `~/.config/containers/policy.json` first, which the
  user can write anyway, so the system default grants a session nothing it lacks.
- **Features that would break under a default `reject`:** the Containers app's `podman pull`,
  distrobox (the runtime portal's container shell), `tinker base add`, the local-model
  `podman load`, dev images' `tilo os upgrade --to`, and possibly the installer's
  `containers-storage` handoff. A tightened default therefore needs a root/user split (a
  `reject` system default plus a per-user permissive policy) and an install scenario run.
- **One uncovered Tilo origin:** the operator-built host image (`os/build-host.sh`) records
  `localhost/tilo-host:latest`, which no scope covers.

## P1-10 — The filesystem daemon's SELinux domain is permissive

`os/usr/share/tilo/selinux/tilo-base.cil` declares `tilo_fs_t` and then
`(typepermissive tilo_fs_t)`: "log-but-allow everything tilo_fs_t does while the enforcing
allow-set is being established from real AVCs". The design puts the filesystem daemon in trust
tier T0, whose boundary includes mandatory access control ([architecture.md](architecture.md)
§2.3), but today the root FUSE daemon's domain only logs denials. Its systemd sandboxing (checked
by `os/tests/kernel_hardening_invariants.sh`) still applies; the MAC layer does not.

**Fix direction:** collect the AVCs from a booted image under real workloads, write the
enforcing allow-set, drop `typepermissive`, and add an invariant that fails while the domain is
permissive.

**Resolved.** `tilo-base.cil` no longer makes `tilo_fs_t` permissive; its allow-set names each
type fs uses, grouped by subsystem with the reason for each rule.
`os/tests/kernel_hardening_invariants.sh` ("root FUSE daemon SELinux domain enforces with named
types") fails if a `typepermissive` returns, if the FUSE device grant disappears, or if a rule
grants `tilo_fs_t` a whole attribute, an unconfined domain or every permission of a class.
Receipt: on a dev VM (image `tilo-vm:main-f4c9380-ci`, aarch64) with fs.service running under a
certified device identity, the AVCs were collected with the domain permissive and dontaudit rules
disabled, then this module was loaded and the same workload re-run enforcing: mounting
`/var/workspace`, file create/write/append/rename/copy/truncate/chmod/xattr/delete as two local
accounts, a 5 MB content round trip, the storage socket's snapshot and eviction requests, Iroh
waking (relays, net report, DNS), a stop and start, and the native data plane's probes on a Btrfs
store volume. No `tilo_fs_t` denial was logged and every operation matched the permissive run.
Peer sync with a second device was not part of that run. The shipped kernel has no fanotify, so
the native data plane itself cannot start there (fs serves FUSE); its own rules will come from
AVCs on a kernel that has it.

## P1-11 — A Read Deny below the room does not withhold that record's channel key

`docs/architecture/permissions.md` §5.3. Room grants can deny Read on one app or record, and every
per-record read path honours it (`StatePeer::check_document_read`). Channel stewards used to admit
devices per room: `allowed_devices` in `crates/session/shell/src/shell/channel_policy.rs` asked Read
once, on the room node, and the steward in `crates/collab/tilo-action-broker/src/channel_encryption.rs`
admitted every allowed device to every record channel of that room. A person denied one record
therefore still received that record's MLS keys, and the room was the confidentiality boundary
against the member's own modified client.

**Resolved for records, apps and patterns of records.** Stewards admit per channel
(`ChannelDirectory::allowed_devices(room, channel, route, now)`; the relay's
`PeerGovernance::withheld_record_readers`), deciding Read on the channel's record with the same decision
every door applies. A channel id is a hash of its record's route, so the steward names the record
from a grant naming it or from the route a device sends beside its access request
(`NeighbourMsg::MlsRoutedAccessRequest`), which it keeps only if the route hashes to the channel's
id and keeps beside the channel's group across restarts. A channel whose record it cannot name is
withheld from every device a Deny over a pattern could keep out
(`collab_types::pattern_read_witnesses`), so omitting the route gains a denied device nothing. A
denied device that held a key is removed in a new epoch of that channel alone. Held by
`channel_encryption`'s `a_read_deny_on_an_app_withholds_every_routed_channel_of_it` and
`a_device_withheld_from_one_channel_loses_only_that_channels_key`, `channel_policy`'s
`a_read_deny_over_an_app_withholds_every_routed_channel_of_it`, the relay stewardship's
`a_read_deny_over_an_app_withholds_its_routed_and_its_unknown_channels` and
`learned_routes_survive_a_restart_and_only_true_ones_are_kept`, and peer-state's
`a_read_deny_on_a_record_withholds_only_that_records_channel`.

**Remaining:** a Deny below a record (an item, a field) binds honest clients and doors only. One
key opens the whole record, so against a modified client the record is the boundary; content that
must stay hidden from a member belongs in a record of its own, denied by name or pattern.

## P2-7 — `sudo` and polkit ship in the image although the design brokers every privileged operation

[architecture.md](architecture.md) §4.6 says "No `sudo`, no `pkexec`, no general-purpose
escalation". The image's package allowlist keeps `sudo` (`os/scripts/verify-runtime-crypto-packages.sh`)
and installs polkit (`os/Containerfile`). Recorded as the gap between the design and this build,
not as an exploitable path: who may use either is decided by the image's account and polkit
configuration, which this entry does not assess.

## P2-8 — Hardware shadow stacks are an x86-64 property only

[architecture.md](architecture.md) lists hardware shadow stacks in the image-wide Tier 0.
`os/kernel/config-fragments/x86_64.config` enables `X86_USER_SHADOW_STACK` and `X86_KERNEL_IBT`;
`aarch64.config` says ARM pointer authentication and BTI are not equivalents and makes no such
guarantee. ARM64 builds therefore lack this layer.

## P1-12 — A filesystem grant covering the home directory reaches Tilo's private stores

`crates/packaging/launcher/src/capabilities.rs`, `is_bindable_path`. A granted path is bound and
allowed by Landlock with no filter beyond absolute-and-not-`/`, and Landlock cannot carve a hole
beneath an allowed directory. An app granted `$HOME` or `~/.config` therefore reads, as the same
uid, the identity store's fallback secrets (`~/.config/tilo/identity/secrets/*.key`, the person's
root secret when no keyring answered at provisioning), any secret-vault `vault.key`, and with a
writable grant can rewrite `trusted-attestors.toml` and `revoked-keys.toml`. Nothing is exposed
without such a grant ([identity-security.md](../identity-security.md)).

**Fix direction:** mask these paths in the mount namespace after the grant's bind (a tmpfs over
the directories, `/dev/null` over the files), on both launcher pipelines.

**Resolved in source.** `CapabilityPlan::for_app` confines every plan against
`private_store_roots()` (`$XDG_CONFIG_HOME/tilo`, `$HOME/.config/tilo`, `$TILO_SECRETS_DIR`): a
grant at or inside one is refused; a grant containing one keeps its bind and the whole directory is
covered by an empty read-only tmpfs after every grant (`sandbox::push_capability_binds`, shared by
both pipelines, and the Windows capsule planner). Masking alone would leave a writable grant able
to rename `~/.config` aside and plant new trust files at the old path, so each directory between a
writable grant and the store is bound onto itself (a mount point cannot be renamed or removed), and
a writable grant holding a symlink on the store's route is bound read-only. Held by
`room_confinement::a_home_grant_never_reaches_tilos_private_configuration` (live bwrap and
Landlock: unreadable secret and vault key, unwritable trust files, unmovable `~/.config`, Documents
grant unchanged) and the plan/argv tests in `capabilities.rs` and `sandbox.rs`. The macOS Seatbelt
profile confines the same plan (`launcher_macos::policy::SeatbeltPolicy::resolve`) and, after every
grant and the chooser's picked-path extension, denies each store outright and every write to each
directory and symlink its path resolves through, spelled as the disk spells it, so nothing can
rename an ancestor aside (`SeatbeltPolicy::hidden`/`pinned`). Held by the live `sandbox-exec`
launches in `crates/packaging/launcher-macos/tests/seatbelt_launch.rs`
(`a_home_grant_never_reaches_tilos_private_stores` and its symlink and sibling-grant cases).

## P2-9 — The per-launch identity broker loads the root secret and never uses it

`crates/packaging/launcher/src/run.rs`, `start_identity_broker`. Every authored launch loads the
selected record *with* its secret (`runtime_identity::load_for_launch`, a Secret Service round
trip or a read of the fallback file) into a broker thread that refuses `sign` for every record
it can be given (`SigningIdentity::signs_with_root_key` is true whenever `device_certificate` is
`None`, which every launcher-built identity is). The secret sits in launcher memory for the app's
lifetime for nothing; app signatures come from the identity service.

**Fix direction:** serve fd 15 from `runtime_identity::load_metadata_from_path`, which validates
the record without contacting the secret provider.

**Resolved in source.** `start_identity_broker` reads a store record through
`runtime_identity::load_metadata_for_launch` (an explicit debug record through
`load_metadata_from_path`) and serves it with `runtime_identity::broker::serve_metadata`, which
answers metadata and refuses every `sign` with the same bytes. Neither contacts the secret
provider; an unsecured store record still fails the launch, as does one naming a provider this
session cannot resolve. The secret-loading `load_for_launch`, the identity-backed `broker::serve`
and its dead local-signing arm are gone. Held by
`crates/packaging/launcher/src/run.rs::identity_resolution_tests::a_launch_never_reads_the_identity_secret`
(the secret is deleted from its provider before the launch),
`crates/runtime/runtime-identity/src/unix.rs::tests::a_launch_reads_public_fields_without_the_secret_provider`
and `crates/runtime/runtime-identity/src/broker.rs::tests::broker_answers_metadata_and_refuses_every_sign_over_pinned_bounded_frames`.

---

## Not defects, recorded to prevent regression

These are load-bearing and correct. Changing them requires understanding why they are as they are.

- `crates/session/shell/src/shell/state.rs` (`apply_room_changes`) — merges into a **copy** first, because
  *"a check that runs after the merge has already happened is not a check."* Whole batches are
  refused rather than partially applied.
- `crates/session/shell/src/shell/state.rs` (`PeerOrigin`) — `PeerOrigin` is *"named as the transport authenticated it —
  not as the message claims."*
- `crates/session/shell/src/shell/state.rs:1750-1753` — re-pinning room authority is refused, because otherwise
  *"send the guest another invite"* becomes a way to re-point them at an attacker.
- `crates/session/shell/src/shell/state.rs:1078-1084` — a role **is** the verb on the membership capability, not a
  separately stored field, so the two cannot drift.
- `crates/packaging/launcher/src/netgate.rs` — egress is fail-closed: if the gate cannot start the
  launch is refused, not downgraded.
- `crates/collab/collab-types/src/caps/net.rs` (`host_covers`) — `host_covers` is shared by the grant model and the
  egress gate specifically so the two interpretations cannot diverge.
- `docs/app-model/trust.md:218` — package revocation is re-read at every launch; there is no
  "trust the install-time verification forever" cache.

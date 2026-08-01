# Security & identity defects — as-built audit, 2026-07-30

Findings from a four-way survey of the boot chain, identity/key management, threat-model
documentation, and the cryptographic-roles implementation. This file records **defects in the
code as it stands today**. The forward-looking design lives in
`docs/superpowers/specs/2026-07-30-security-identity-architecture.md` and deliberately does not
reference any of this.

Severity: **P0** = exploitable or silently produces invalid security state. **P1** = a documented
guarantee is not delivered. **P2** = correctness/hygiene.

---

## P0-1 — The identity ceremony persists a key that cannot sign

`ui/src/widget/identity_flow/mod.rs:180-191`

`SerializableIdentity::from_identity` cannot reach into `frost_ed25519`'s `SigningKey`, so it
persists `SHA-512("qos-identity-flow-v1-placeholder-key" ‖ group_public_key)` as `secret_bytes`.
The in-source comment says it outright: *"clearly NOT useful for signing."*

Any identity minted through the UI ceremony produces signatures that do not verify against its own
recorded public key.

Compounding: `crates/runtime/runtime-identity/src/lib.rs:270-297` detects the pubkey/secret
mismatch and **deliberately does not fail** — the only signal is a
`#[cfg(debug_assertions)] eprintln!`. In a release build the failure is silent.

`shell/src/firstboot.rs:190-226` correctly bypasses this path and writes the real dalek seed via
`ThresholdIdentity::bootstrap_single_device_seed`. So firstboot-minted identities are valid and
ceremony-minted identities are not, with the same on-disk format and no way to tell them apart.

**Fix direction:** add the key-extraction helper to `threshold-identity`, delete the placeholder
branch, and make the mismatch fatal in `runtime-identity` rather than advisory.

---

## P0-2 — The FUSE replication path performs no authentication

`fs/src/sync.rs:159-195`

`SyncProtocol::accept` takes any connection on `QOS_FS_SYNC_ALPN` and:

- `MODE_FULL` → serialises the entire namespace document to the caller (`sync.rs:169-171`)
- `MODE_SYNC` → feeds inbound bytes directly into `sync_receive` (`sync.rs:179-183`)

There is no identity check, no ACL consultation, and no signature verification on either branch.
The same applies to `fetch_remote_doc` (`sync.rs:104-113`) and the anti-entropy full-document merge
(`sync.rs:317`).

The ACL in `fs/src/acl.rs` is well-built — fail-closed, deny-biased, delegating to
`collab_types::decide_capability`, and it aborts the mount rather than serving unenforced. But it is
consulted only on the FUSE syscall path (`fs/src/fs.rs:432`, `:456`, `:561`), i.e. against local
processes. **It is not a replication boundary.**

Any peer that can reach the endpoint and speak the ALPN can read and rewrite the whole namespace.

**Fix direction:** the responder must resolve the connecting node to an identity and evaluate the
ACL per-connection. Depends on a real, persistent device identity existing first.

---

## P0-3 — The iroh node key is ephemeral, so FS attribution and ACL subject rotate every boot

`fs/src/sync.rs:89-96` builds the endpoint without ever passing a `secret_key`, so the iroh NodeId
is freshly random on every process start.

Consequences:

- `fs/src/main.rs:221,286` sets `author = endpoint.id().to_string()` — authorship attribution is a
  new anonymous string each mount.
- `fs/src/main.rs:219,283` resolves the default ACL subject from the same key, so **any ACL grant
  written against the default subject is dead on the next boot**.
- The node-id-reuse guard at `fs/src/namespace.rs:147-152` ("a restart with the same author can't
  reuse a node id") never engages, because the author is never the same twice.

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
- `keys/tpm2-pcr-signing.public.pem` is baked to `/usr/share/qos/` (`os/Containerfile:427`) and
  read by nothing.
- `docs/secure-boot.md` describes a chain — *"firmware → shim (MS-signed) → GRUB (Fedora-signed) →
  kernel (Fedora-signed)"* — that the image explicitly deletes. The document is stale and
  actively misleading.

---

## P1-2 — Disk encryption is opt-in, TPM-mandatory, and conflicts with the initrd-less kernel

`os/usr/libexec/qos-install:276-289` — `BLOCK_SETUP` defaults to `direct`; encryption requires
`--encrypt`. `qos-install:176-178` aborts if neither `/dev/tpmrm0` nor `/dev/tpm0` exists, so there
is **no passphrase-only encryption path** — a machine without a TPM gets no encryption at all.

Separately, `os/etc/kernel/install.conf` sets `initrd_generator=none` and `os/Containerfile:654`
does `rm -f /boot/initramfs-*.img`. A LUKS root needs early **userspace** to unseal the TPM and open
the mapping; static drivers let the kernel mount root but cannot run `systemd-cryptsetup`. No
`crypttab`, dracut config, or conditional re-enable exists in `os/`.

`--encrypt` and the custom kernel therefore appear mutually exclusive. **Not yet confirmed by
booting an encrypted install** — verify with `os/dev/vm.sh boot --tpm` before treating as fact. The
enrollment sequence itself was verified against loopback LUKS + swtpm, not against a real boot.

---

## P1-3 — MLS does not rekey on member removal

`crates/collab/channel-crypto/src/group.rs:399-404` — `remove_member` is a `todo!()`. The design
guarantee is that the group key rotates on *every* membership change; removal is the case that
matters, and it panics. The in-source workaround is "mint a fresh channel and invite only the
desired members".

Also: `channel-crypto` is a **dev-dependency** of `shell` (`shell/Cargo.toml:251`), so MLS is
compiled only for tests and is absent from production builds.

---

## P1-4 — The signed-op validator is never fed in production

The 8-step (extended to 9) validation pipeline in
`crates/collab/collab-types/src/validator.rs:589` is implemented and thoroughly tested, and the
broker's default state is correctly fail-closed (`shell/src/shell/collab_broker.rs:797-800`: an
empty `authorised_signers` rejects every signed op).

But `register_channel_state` has no non-test caller, and no production path mints a
`RoleAttestation`. `CollaborationManager::set_identity` / `with_identity` likewise have no callers
in `shell/`, `ui/`, or `apps/`. Roles are therefore advisory in practice.

`Space` (`crates/collab/collab-types/src/space.rs`) has zero consumers outside its own crate and
tests. `ui/src/widget/channel_browser.rs` is a complete widget with no shell call site.

---

## P1-5 — No lock on suspend; idle auto-lock off by default

`crates/system/power/src/service.rs:255-265` handles `PrepareForSleep`: the `true` branch only logs.
Nothing locks the session before sleep or on resume, so suspend-then-open-lid lands in an unlocked
session. `crates/system/power/src/logind.rs:19-20` documents this as working — *"so the compositor
can lock before the machine sleeps"* — which is not implemented.

`shell/src/compositor/idle.rs:107-113` defaults `lock_timeout` to `None`; idle auto-lock is
reachable only via `QOS_IDLE_LOCK_SECS`.

---

## P1-6 — Vault keys sit beside the ciphertext they protect

`crates/system/accounts/src/vault.rs:175-215` — XChaCha20-Poly1305 with a fresh nonce per write, but
the 256-bit key is stored as `vault.key` in the same directory as `vault.enc`, both 0600. The module
doc is honest about it: *"a local reader with access to the config directory sees both the key and
the ciphertext."* `apps/totp/src/storage.rs` repeats the pattern.

The threshold identity itself is plaintext JSON at `~/.config/qos/identity/primary.json`
(`shell/src/shell/identity_provisioning.rs:39-47`), 0600 in a 0700 dir.

A Secret Service wrapper exists (`crates/runtime/runtime-portal-xdg/src/lib.rs:354-370`) with no
caller in any app.

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

`docs/supply-chain.md` claims builds are reproducible across time. `os/rpms-snapshot.env` +
`os/Containerfile.rpms` genuinely deliver that for the package set, but the base underneath
evaporates, so a rebuild weeks later does not merely differ — it does not run. Supply-chain
integrity rests on the pin being real rather than aspirational: a pin that reliably fails is one
"just point it at whatever resolves today" away from becoming an unpinned build under deadline
pressure, and the frozen RPM set would then be resolved against an unrecorded base.

**Fix direction:** mirror the base into `code.podesta.ai/qos/fedora-bootc` and pin against the
mirror — the same shape `os/Containerfile.rpms` + `snapshot-rpms` already use for packages, since
our registry collects nothing. Copy with the multi-arch index intact. Fixed on this branch: the
`mirror-base` workflow plus `os/base-snapshot.env` (`BASE_REF`), consumed by every Containerfile
as a `--build-arg`.

Two adjacent holes in the same guarantee are still open:

- `os/containers/qos-relay/Containerfile:74` builds its runtime stage `FROM
  quay.io/fedora/fedora-minimal:44`, a floating tag — it never breaks, it just silently changes,
  so the relay image is not reproducible across time.
- `os/Containerfile`'s `wpe-builder` stage installs its build deps with `dnf builddep
  webkitgtk6.0` against live Fedora repos, a deliberate exception noted in the file. It is
  currently broken — `build-wpe.sh:67` exports `PKG_CONFIG_SYSROOT_DIR=/wpe`, so every system
  `-I/usr/include` becomes `-I/wpe/usr/include` and libwpe's CMake configure fails on
  `Imported target "XkbCommon::libxkbcommon" includes non-existent path` — and nobody noticed,
  because the base `FROM` above failed first and the layer cache served the stage before that.
  An unpinned stage is also an unwatched one.

---

## P2-1 — `PeerId` is a 64-bit fold of the public key

`shell/src/network/behaviour.rs:98-107` derives the wire `PeerId` as a 64-bit FNV-1a fold of the
public key, and it is used as the `PeerIdentityStore` key. 64 bits is collision-findable. Note the
newer `PeerOrigin` (`shell/src/shell/state.rs:109-127`) correctly carries the full base58 peer id —
the fold is the older path.

## P2-2 — Corrupt peer-key file silently rotates identity

`shell/src/network/behaviour.rs:122-127` — if `/var/lib/qos-fs/peer-key` exists but does not parse,
it is overwritten with a fresh keypair rather than raising. Identity loss becomes indistinguishable
from first run. The same function documents a known TOCTOU window between write and `chmod 0600`
(`:142-146`).

## P2-3 — Revocation is modelled but undeliverable

`crates/identity/qos-user-identity/src/revocation.rs` implements signed revocation entries with
correct before/after-cutoff semantics (tested at `shell/src/network/identity.rs:364-406`), but
`PeerIdentityStore::set_revocations` has no non-test caller and there is no distribution mechanism.

## P2-4 — Egress documentation describes a weaker mechanism than the code implements

`docs/app-model/sandbox.md:162-172` describes DNS-layer filtering that direct-IP connections bypass.
The code has no DNS proxy — `dns-only` resolves to `EgressDecision::Blocked`
(`crates/packaging/launcher/src/netgate.rs:241`) and address literals arrive as
`Destination::Addr` and are matched as literals, pinned by the test
`an_address_literal_never_matches_a_hostname_grant` (`netgate.rs:1112`).

The code is correct; the document is stale. Recorded because a stale doc describing a weaker
mechanism is how a strong one gets "simplified" later.

## P2-5 — `qos-security-audit` cannot fail

`os/usr/libexec/qos-security-audit` exits 0 unconditionally (`:5`, `:86`) and the unit sets
`SuccessExitStatus=0 1`. It reports; it never gates a boot. Its image-signing check greps
`policy.json` for the string `sigstoreSigned` (`:63-67`) rather than verifying anything.

## P2-6 — `insecureAcceptAnything` is the container policy default

`os/etc/containers/policy.json:2-4`. Signature enforcement is scoped to `code.podesta.ai/qos/qos`
only; every other image reference is unverified.

---

## Not defects, recorded to prevent regression

These are load-bearing and correct. Changing them requires understanding why they are as they are.

- `shell/src/shell/state.rs:2085-2093` — `apply_room_changes` merges into a **copy** first, because
  *"a check that runs after the merge has already happened is not a check."* Whole batches are
  refused rather than partially applied.
- `shell/src/shell/state.rs:109-127` — `PeerOrigin` is *"named as the transport authenticated it —
  not as the message claims."*
- `shell/src/shell/state.rs:1750-1753` — re-pinning room authority is refused, because otherwise
  *"send the guest another invite"* becomes a way to re-point them at an attacker.
- `shell/src/shell/state.rs:1078-1084` — a role **is** the verb on the membership capability, not a
  separately stored field, so the two cannot drift.
- `crates/packaging/launcher/src/netgate.rs` — egress is fail-closed: if the gate cannot start the
  launch is refused, not downgraded.
- `crates/collab/collab-types/src/caps.rs:447` — `host_covers` is shared by the grant model and the
  egress gate specifically so the two interpretations cannot diverge.
- `docs/app-model/trust.md:218` — package revocation is re-read at every launch; there is no
  "trust the install-time verification forever" cache.

# qOS security architecture — full defense-in-depth plan (UX-first)

Status: **plan / design.** Supersedes nothing; **extends** `plan/security.md`
(sandbox, seccomp, portals, capability grants, input firewall, P2P identity —
all built) and `docs/secure-boot.md` (Secure Boot assessment), and upgrades the
naive `tpm2-luks` shipped in E2 into a real, recoverable, measured boot+unlock
story.

It maps the requested stack onto qOS's actual architecture, and — per the
directives — leads with **UX** and with one cross-cutting rule:

> **No single root of trust. Never *just* trust the TPM chip.**

Every mechanism here is one layer; none is load-bearing alone. The TPM in
particular is treated as *a* factor and *a* measurement device — never as the
sole gate to data (it can be sniffed on a discrete bus, cleared by firmware, or
fail with the motherboard) and never as the sole proof of integrity (pair it
with signed predictions, an independent recovery path, and a public transparency
log so "trust the vendor" is *verifiable*, not blind).

---

## 1. Trust tiers (the spine)

Everything below is organized by *who you're defending against*, because the
right mechanism depends on the trust tier:

| Tier | Principal | Trust | Primary boundary |
|---|---|---|---|
| **T0 Trusted base** | kernel, compositor (`shell`), FUSE driver (`fs`), sync daemon, portal backend | trusted, must be *verifiable* | Secure Boot + measured boot + SELinux MAC + signed/reproducible image + transparency log |
| **T1 First-party apps** | terminal, file-explorer, editor, settings | semi-trusted (our code, may have bugs) | bubblewrap + seccomp + Landlock + per-workspace bridge + capability grants |
| **T2 Third-party local apps** | user-installed apps | untrusted code, trusted-ish *source* | same as T1, default-deny, network off by default |
| **T3 Peer rendering code** | code/content that arrives *from a peer* to render locally | **hostile** — kernel sandbox is insufficient | microVM (Firecracker/gVisor-class) or WASM-no-host-syscalls |
| **T4 Hosted host operator** | the cloud operator running a hosted workspace | "honest but curious" / potentially hostile | confidential computing (SEV-SNP / TDX) + remote attestation before mount |

The existing `plan/security.md` covers T1/T2 well. This plan fills T0 (boot
integrity + supply chain), T3 (peer code isolation), and T4 (hosted), and adds
the per-capability layers (Landlock, typed egress) that harden T1/T2.

**Identity assumption:** qOS v1 is **single-identity** (one human owns the
device). This plan targets that model but is deliberately structured so
**multi-user is a later extension, not a rewrite** — see **§6.1**, which carries
the single→multi-user seam for every layer.

---

## 2. UX principles (these gate every mechanism)

Security the user fights gets turned off. The whole plan is constrained by:

1. **Invisible when safe.** A healthy boot on your own machine asks for
   *nothing* — TPM+measured-boot releases the disk key silently. You see a
   prompt only when state is *unexpected* (then: PIN, then: recovery key). The
   common path has zero friction; friction appears exactly proportional to
   anomaly.
2. **One enrollment, many uses.** Enroll a security key / set a PIN **once**;
   it serves disk unlock, login (homed), workspace-operation signing, and P2P
   device identity (§6). No per-feature re-enrollment.
3. **Recovery is a designed flow, not an afterthought.** Every credential that
   can be lost has a humane backstop *created at the same moment* it's set
   (recovery key shown as QR + "print / save to phone" at install; a second
   FIDO2 key encouraged). **No dead-ends** — losing a TPM/laptop must never mean
   losing data.
4. **Graceful degradation, never lockout.** No TPM → passphrase/FIDO2, clearly
   explained. Post-update PCR change → the updater *re-seals automatically*
   (§4); the user never types a passphrase because we shipped an update. A
   failed measurement *warns and offers recovery*, it doesn't brick.
5. **Secure by default, legible when advanced.** SB on, hardening on, sandbox
   default-deny, network-off-for-peers — all shipped defaults. PIN policy, PCR
   selection, confidential-computing, per-app egress allowlists live behind one
   **"Advanced security"** pane with plain-language explanations and a
   risk indicator, not scattered toggles.
6. **Trust you can read.** Attestation/transparency surface as a human badge —
   *"This workspace runs verified qOS `af3fd27`, logged publicly"* with a "what
   does this mean?" affordance — not a hash dump and not blind "trust us."
7. **Permission prompts stay inline + contextual** (the existing
   `PermissionPrompt` pattern): asked at the moment of use, scoped
   (Once/Session/Always/Workspace), attributing the *app and the reason*.

---

## 3. Cross-cutting: no single root of trust

Concrete rules applied throughout:

- **TPM is paired, never solo.** Disk unlock = TPM2 **+ PIN** (`--tpm2-with-pin`)
  so a stolen powered-off machine isn't game-over from the chip alone. Prefer
  **firmware/integrated TPM (fTPM)** over a discrete TPM (no sniffable LPC/SPI
  bus). And every TPM binding has an **independent** unlock path (recovery key
  and/or FIDO2) that does **not** depend on the TPM or on boot state.
- **Integrity is measured *and* signed *and* logged.** Secure Boot (signatures)
  + measured boot (PCRs) + a **signed PCR prediction** (so updates don't break
  unlock) + a **public transparency log** of image hashes (so the prediction
  itself is auditable). Any one of these can be subverted; together they aren't.
- **Identity is two-factor by construction.** HW-backed (TPM/FIDO2) *and* a
  recoverable software path; the P2P/device key is anchored in hardware but the
  *account* survives device loss (§6).
- **Availability is a security property.** A mechanism that can permanently
  destroy access (TPM clear, lost key, motherboard swap) without a backstop is a
  *denial-of-service against the owner*. Every "lock" in this plan has a
  designed "unlock-if-you're-really-you" that doesn't trust the same root.

---

## 4. Boot integrity (T0) — **load-bearing, design into the updater now**

### What exists
`docs/secure-boot.md`: the `fedora-bootc:44` base ships the signed chain
(MS-signed shim → Fedora-signed GRUB → Fedora-signed kernel). qOS needs no MOK
today (no out-of-tree modules). SB is HW-gated for verification (dev OVMF is
non-SB).

### The gap & design
1. **Secure Boot** — keep the signed chain; add `lockdown=integrity` LSM
   (kernel cmdline) so even root can't patch the running kernel under SB; ensure
   **module signing** enforcement stays on (it's on in the Fedora kernel). Ship
   SB-on as the documented install default; the installer detects SB-state and
   warns if off.
2. **Measured boot with a *signed prediction* (the load-bearing piece).**
   - Move to a **UKI** (Unified Kernel Image: kernel+initrd+cmdline, one signed
     PE) so the measured surface is one signed artifact, not a GRUB+initrd combo
     that's hard to predict.
   - Use **`systemd-pcrlock`** (or `systemd-measure` PCR *signing*) to bind LUKS
     unlock to a **signed prediction** of PCRs 7 (SB state) + 11 (UKI) rather
     than raw current PCR values. The unlock policy trusts *a public key that
     signs expected PCR sets*, not a frozen snapshot.
   - **The update pipeline re-seals.** Because qOS auto-updates (A3:
     `bootc-fetch-apply-updates.timer` → `code.podesta.ai/qos/qos:latest`), a
     new image changes PCR 11. The build/update flow must **publish the signed
     PCR prediction for the new UKI alongside the image**, and the client
     applies it on update **before reboot**, so the TPM still auto-releases the
     key on the next boot. This is why measured boot must be designed **into the
     updater**, not bolted on: otherwise every update prompts for a recovery
     key. *(This is the single most important architectural item in this doc.)*
3. **UX:** healthy boot = silent. After an update = still silent (re-sealed).
   Tamper (SB off, swapped bootloader, unexpected UKI) = the *prediction
   signature won't match* → fall to **PIN**, then **recovery key**, with a clear
   "your system's boot state changed unexpectedly — if you didn't change
   anything, use your recovery key and contact support" screen.

### Why this beats "TPM seals to current PCRs"
Raw-PCR sealing (what naive `tpm2-luks` does) breaks on **every** kernel/boot
update → trains users to type the recovery key constantly → they paste it into
anything → the recovery key is worthless. Signed predictions keep the common
path frictionless, which is what *keeps* the recovery key rare and therefore
meaningful.

---

## 5. Disk / data encryption (T0/identity) — upgrade E2 beyond "trust the TPM"

### What exists
E2: `qos-install --encrypt` → `bootc install to-disk --block-setup tpm2-luks`.
This is **TPM-only, no PIN, no recovery key, no FIDO2, default PCR policy** — the
exact anti-pattern. It must become the multi-factor design below.

### Design — LUKS2 with multiple independent keyslots
Enroll, at install, **several** keyslots on the LUKS2 volume via
`systemd-cryptenroll` so no single factor is load-bearing:

| Keyslot | Factor | Defends | UX |
|---|---|---|---|
| TPM2 + PIN | `--tpm2-device=auto --tpm2-with-pin=yes` sealed to **signed** PCR 7+11 | powered-off theft; tamper | silent on healthy boot; short PIN if PCRs unexpected |
| Recovery key | `--recovery-key` (high-entropy) | lost TPM/PIN, motherboard swap, TPM clear | shown **once** as QR at install: "print this / save to your phone"; never stored on the device |
| FIDO2 (optional, encouraged) | `--fido2-device=auto` | survives updates + reinstalls; portable; not boot-state-bound | "tap your security key" |
| Passphrase (fallback if no TPM) | `--password` | machines without a usable/firmware TPM | normal passphrase prompt |

Rules: **prefer fTPM** (firmware TPM — no sniffable bus; the installer detects
and prefers it, warns on discrete TPM). **Always** create the recovery key when
encryption is enabled — it's not optional, it's the backstop that makes trusting
the TPM acceptable. The TPM keyslot is a *convenience*; the recovery key is the
*guarantee*.

### `systemd-homed` — per-user encrypted home (alongside FDE)
- Each user's `/home/<user>` is its own LUKS-backed home, unlocked by the user's
  auth at login and **locked on logout/suspend**. FDE protects the device at
  rest; homed protects *one user's data from another* and gives "locked when I
  walk away" semantics that map to the identity model (§6).
- Bind homed unlock to the **same** factors (FIDO2 / TPM+PIN / recovery), so
  §2.2 holds: one enrollment, used for login too.
- **UX:** login *is* the decryption; logout *is* the lock. Suspend re-locks
  homed so a stolen sleeping laptop doesn't leak the home. (Run alongside FDE,
  not instead — defense in depth: FDE for the device, homed for the user.)

### UX flows (the part that makes this usable)
- **Install:** one screen — "Encrypt this device? (recommended)". On yes: detect
  TPM (prefer fTPM) → set a PIN → **show the recovery key as a QR + text, force
  "I've saved it"** → optionally "add a security key now (tap it)". Done.
- **Daily:** power on → silent unlock → homed unlock at login. Zero passphrases.
- **Recovery:** lost PIN/TPM → recovery-key screen (paste or scan QR). Lost
  recovery key but have FIDO2 → tap. Re-enroll the missing factor afterward from
  Settings → Security with a single guided step.
- **Motherboard/TPM swap:** recovery key unlocks, then "re-bind to this device's
  TPM?" one-tap re-enroll. Data never trapped behind dead silicon.

---

## 6. Identity (T0–T3) — one hardware-anchored identity, recoverable

### What exists
Per-host ed25519 key at `/var/lib/qos-fs/peer-key` (libp2p `PeerId`); signed
invite tokens; per-user identity self-asserted.

### Design — hardware-backed, **but the account outlives the hardware**
- **One enrollment → four uses:** a TPM/FIDO2-anchored credential drives (a)
  disk unlock, (b) homed login, (c) **workspace-operation signing** (grant
  changes, invites), and (d) **P2P device identity**. The device key moves from
  a plaintext file into a **TPM-resident / FIDO2-resident** key (non-exportable;
  signing happens in hardware).
- **Critical anti-"trust the chip" rule:** the *device* key is hardware-bound,
  but the *user identity* is **a separate, recoverable account key**. A device is
  *authorized by* the account (cross-signed), so losing/replacing a device =
  revoke the device key + authorize a new one **from the account**, not "your
  identity is gone with the laptop." This is the difference between
  hardware-*backed* (good) and hardware-*trapped* (a self-DoS).
- Device authorization list is itself a CRDT object in the user's identity doc,
  replicated P2P; revocation propagates like any grant.

### UX
- First run: "This device is now part of your qOS identity" — invisibly creates
  the HW device key, cross-signs from the account (or *creates* the account with
  a recovery phrase on the very first device).
- New device: scan a QR from an existing device (or enter the recovery phrase) →
  authorized. Lost device: "forget this device" from any other → its key is
  revoked everywhere.

---

## 6.1 Identity model: single-identity today, multi-user-ready

qOS v1 is **single-identity**: one human owns the device; that owner's identity
is the device's primary identity; collaboration is **between owners across
devices** (P2P), not between users on one device. This plan is built to that
model — but every layer is chosen so that **becoming multi-user later is an
extension, not a re-encrypt / re-architecture**. The rule: *design the seams
now, don't build the machinery yet.*

### The one invariant that makes multi-user possible: split device ↔ user
The single most important decision (already in §6) is keeping the **device key**
(hardware-bound, machine-scoped) **separate from the user/account identity**
(recoverable, person-scoped). In v1 they look 1:1 (one owner per device), but
because they're *distinct objects cross-signed together*, multi-user is later
just "this device hosts **N** authorized user identities" instead of one. If we
instead collapsed them into a single "owner = device" principal (the tempting
v1 shortcut), multi-user would require a rewrite. **We do not take that
shortcut.** Concretely, even in v1: no code assumes a global singleton "the
owner" — ownership is always *an* identity that happens to be the only one.

### Per layer: single-user reality → multi-user extension (and what to design now)

| Layer | v1 (single-identity) | Multi-user later | Design now so it's not a rewrite |
|---|---|---|---|
| **FDE (§5)** | device key unlocked by the owner's factors | device-wide key; **any** enrolled user's factor (or their homed credential) can bring the device up | keep FDE a **device** capability distinct from user login — never seal the *device* key to one *user's* identity alone. Adding a user later = enroll another LUKS keyslot, **no re-encrypt.** |
| **homed (§5)** | one encrypted home | **N** homed homes, each unlocked by its own user, locked on that user's logout | **adopt homed from the start** (even for the single user) so per-user crypto is the boundary on day one; a second user is then just `homectl create`. Retrofitting homed onto a plain `/home/quickuser` later is the painful path we avoid. |
| **Identity / device key (§6)** | device key ↔ owner account (1:1, cross-signed) | device hosts a **set** of authorized user identities; device key cross-signed by each | the device-authorization list is **already a CRDT set**, not a scalar — it holds one entry in v1, many later. **Don't special-case "the" owner.** |
| **Login / session-lock (D2)** | single session; lock = the owner's lock | greeter picks a user; per-user session + per-user lock/idle; fast user-switch | route lock/unlock through the **homed/identity credential**, not a device-global secret, so "who is unlocking" is a real question with a real answer even when the answer is always the same person today. |
| **Workspaces & capability grants (§7, plan/security.md)** | "only the owner can grant" | grants owned **by a user**; a workspace has an owning user; co-resident users can't grant in each other's workspaces | model grant authority as **"the workspace's owning identity"**, not "the device owner." In v1 they coincide; the field exists either way. (This is the one place current code says "owner" as a singleton — flag it for a `WorkspaceOwner = UserId` refactor before multi-user.) |
| **P2P / collaboration** | peers = remote owners; my device speaks as me | peers can be **other users on my own device** too; identity is per-user not per-device | already true — P2P identity is the *account* key (§6), which is per-user by construction; nothing to change. |
| **Disk-at-rest, boot, supply chain, hardening, egress, attestation (§4, §8–§11)** | device/base level | **unchanged** — these are machine/image properties, not user properties | nothing; they're already user-count-agnostic. Multi-user adds *no* boot/supply-chain work. |

### UX implications
- **v1 stays dead-simple:** install creates one user, one home, one identity; no
  user-picker, no "switch user" — that surface is simply not shown when there's
  one user. Single-identity users never pay a multi-user tax.
- **The growth path is additive:** "Add a user" in Settings → Users later =
  `homectl create` (new encrypted home + enroll their FDE keyslot + their
  identity credential). Existing data is untouched (no re-encrypt, because FDE
  was never sealed to one *user*). The greeter gains a user list; everything
  else the user already knows stays put.
- **Recovery scales per-user:** each user gets their **own** homed recovery
  credential (§5); the device recovery key (FDE) stays the device-owner's. Two
  independent recovery scopes = no user can lock another out, and no single
  recovery secret rules everyone (consistent with §3).

### Anti-patterns to avoid in v1 (the seams)
1. Don't seal the **device** LUKS key to a **user's** TPM/identity such that the
   key dies or must be re-wrapped when users change → keep device-key and
   user-key independent keyslots.
2. Don't bake a global `OWNER` singleton into grant/sandbox logic → use
   `UserId` even when there's exactly one.
3. Don't put `/home` on plain FDE-only storage if homed is feasible → adopt
   homed now so per-user encryption isn't a later migration.
4. Don't make the P2P/device key *be* the identity → it authorizes *an*
   identity (§6); keep the indirection.

---

## 7. Sandboxing / confinement (T1–T3) — finish the layers

### Have (per `plan/security.md`)
bubblewrap per-workspace box, seccomp-bpf blocklist, per-workspace Wayland
bridge, capability grants (CRDT), portals (FileChooser/ScreenCast/Camera),
per-workspace netns + nftables, input firewall, SELinux base (E1).

### Add
1. **Landlock (per-capability, in-kernel self-restriction).** After bwrap builds
   the box, the app (or our launcher stub) calls Landlock to drop FS/net rights
   to *exactly* the granted set, enforced by the kernel on the process **and all
   descendants** — so a sandbox escape to a child still can't widen FS/net.
   Landlock is the *direct* enforcement of the capability model (bwrap binds are
   coarse; Landlock is rule-precise and dynamic). Layer **with** seccomp, don't
   replace it. *(Landlock ABI ≥3 net scoping on the Fedora kernel; degrade
   gracefully on older ABI — log "Landlock net unavailable, relying on netns".)*
2. **SELinux = MAC backstop for T0 only.** Confine the trusted base
   (sync daemon, FUSE driver `fs`, compositor `shell`, portal backend) with
   targeted policy so a compromised base component is still boxed by the kernel
   MAC. **Not** per-app policy (that's bwrap+Landlock+seccomp's job — SELinux
   per-app doesn't scale and fights the dynamic capability model). E1 already
   established the policy-module pattern (`.cil` + `semodule`); extend it from
   the installer domain to these daemons.
3. **T3: untrusted peer rendering code → stronger-than-kernel isolation.**
   Kernel namespaces share one kernel with hostile code; for *code/content that
   arrives from a peer to render locally*, that's the wrong trust tier. Two
   options, pick per workload:
   - **microVM** (Firecracker / cloud-hypervisor, or gVisor's userspace kernel)
     — separate kernel, tiny attack surface, for rendering arbitrary peer code.
   - **WASM with no host syscalls** — a capability-clean sandbox for
     peer-supplied *widgets/logic* (the CRDT-native UI is well-suited: render to
     a scene buffer, no ambient authority). Preferred where the peer code is
     "UI logic" rather than "a whole app."
   - **UX:** invisible — the user sees a normal tab; the boundary is internal.
     A subtle "running isolated (peer code)" chip in the tab chrome makes the
     stronger boundary *legible* without nagging.
4. **portals stay the brokered path** for any host-resource access from any tier
   (already built) — extend coverage as new resource types appear.

---

## 8. Network (T1–T3) — typed, attenuable egress

### Have
Per-workspace netns + `nftables` host-allowlist for `Network::Always{hosts}`.

### Design — a real capability, not just a firewall rule
- Model egress as a **typed, attenuable capability**: *this app → this
  peer/relay/host:port only*, and a holder can **attenuate** (hand a child a
  strictly-narrower cap, never wider). Enforce with **per-app netns + an
  allow-listed proxy + Landlock net scoping** (three layers; §3).
- Default: **peers' apps get no egress**; first-party apps get egress only to
  declared endpoints. P2P collaboration traffic goes through the compositor's
  brokered libp2p path, not raw app sockets.
- **UX:** egress requests surface like any capability ("*Editor wants to reach
  api.github.com*") with the same inline Once/Session/Always prompt; the
  Advanced pane shows a per-app egress allowlist users can read and prune.

---

## 9. Supply chain (T0) — make "trust the vendor" verifiable

### Have
Reproducible-ish multi-stage image built in CI and pushed to
`code.podesta.ai/qos/qos` (A2, verified green — image digest published per
build).

### Add
1. **Sign images** (cosign / `bootc`'s signature support) and have the client
   **verify the signature before applying** an update (wire into the A3 update
   path — refuse unsigned/untrusted images).
2. **Reproducible builds** — pin toolchain + deps (the `os/Cargo.lock.pinned`
   from A2 is a start); goal is bit-reproducible so third parties can
   independently rebuild and confirm the published digest.
3. **Transparency log** — publish every released image digest to an append-only,
   publicly-auditable log (Rekor-style). Then the **measured-boot PCR policy can
   require that the running image's digest appears in the log** — so a targeted
   "just for you" backdoored image fails the policy because it was never logged.
   This is what converts "trust Anthropic/qOS" into "verify qOS, publicly."
4. **UX:** the trust badge (§2.6) reads the signature + log inclusion and shows
   *"verified & publicly logged"* vs *"⚠ unverified image"* in Settings →
   About, and gates auto-update on it.

---

## 10. Hosted workspaces (T4, commercial) — operator can't read tenant data

- **Confidential computing:** run hosted workspace backends inside **AMD SEV-SNP
  / Intel TDX** confidential VMs; memory is encrypted from the host operator.
- **Remote attestation before mount:** the client **attests** the enclave
  (verifies the CVM measurement + that it's running the expected, logged qOS
  image) **before** it mounts the workspace or sends CRDT data. Ties directly to
  §9's transparency log (attestation checks the image is the logged one) and §6
  (the client's HW identity authenticates the channel).
- **UX:** a hosted workspace shows *"host-blind: the operator can't read this
  workspace"* once attestation passes, or refuses to mount with a clear reason
  if it fails. The user gets a one-line guarantee, not a crypto lecture.

---

## 11. Kernel / userspace hardening (T0, table stakes — verify, mostly free)

Audit + assert-on in the image build (cheap, high-value):
- **Hardened allocator** (`hardened_malloc` or scudo) for the trusted base
  daemons at minimum; `init_on_alloc=1 init_on_free=1` kernel cmdline.
- **Restricted `/proc`** (`hidepid`), **constrained unprivileged userns**
  (allowed — bwrap needs it — but bounded), **`lockdown=integrity`** under SB.
- **Wayland-only** display server (already true — no X11 keylogging surface).
- Module signing enforced (Fedora default; assert).
- Add a boot-time/CI **self-check** that logs which of these are actually active
  (so "we think it's on" becomes "verified on").

---

## 12. The TPM threat model, explicitly (why §3 exists)

Stating the failure modes the plan defends against — the reasons *not* to trust
the chip alone:

- **Bus sniffing** (discrete TPM): the key can be captured on the LPC/SPI bus in
  transit → **mitigation:** prefer fTPM; pair with PIN so the sniffed seal is
  useless without the PIN.
- **TPM clear / firmware reset / motherboard swap:** the sealed keyslot is gone
  → **mitigation:** independent recovery-key + FIDO2 keyslots (§5).
- **PCR-policy brittleness:** raw-PCR sealing breaks on updates, training users
  to overuse recovery → **mitigation:** signed predictions re-sealed by the
  updater (§4).
- **Firmware/TPM CVEs:** a TPM is software too → **mitigation:** it's never the
  *only* factor; compromising it alone doesn't yield data (PIN) or integrity
  (signature + transparency log).
- **Availability-as-DoS:** trusting only the TPM means a dead TPM = dead data →
  **mitigation:** §3's "every lock has a non-TPM unlock."

---

## 13. Phasing & near-term actions

**Now / table-stakes (cheap, mostly Fedora-default — just verify + assert):**
- §11 hardening audit + boot self-check.
- `lockdown=integrity`, confirm module signing, Wayland-only (done).
- Extend E1 SELinux from installer → trusted-base daemons (T0 MAC).

**Load-bearing — design before it's painful to retrofit:**
- §4 measured boot with **signed PCR predictions re-sealed by the updater**
  (UKI + pcrlock). *Highest priority — design into A3's update pipeline now.*
- §5 upgrade E2: TPM2**+PIN** + **mandatory recovery key** + optional FIDO2 +
  fTPM preference. (Direct, near-term code change to `qos-install` + a Settings →
  Security pane.)
- §5 **adopt `systemd-homed` for the single user now** — per-user encryption is
  the multi-user boundary; retrofitting it later is the painful path (§6.1).
- §9 image **signing + verify-before-update**; stand up the transparency log.

**Multi-user seams — cheap to design now, expensive to retrofit (§6.1):**
- Keep **device key ≠ user identity** (§6) — device-authorization is a CRDT
  *set*, not a scalar; FDE device-key independent of any user keyslot.
- Replace the `OWNER` singleton in grant/sandbox logic with `WorkspaceOwner =
  UserId` (coincides with the sole user in v1; no behavior change, just the
  seam).
- Route D2 lock/login through the per-user homed/identity credential, not a
  device-global secret.

**Next:**
- §7.1 Landlock per-capability enforcement; §8 typed egress cap.
- §6 move device key into TPM/FIDO2 + recoverable account model.

**Later / specialized:**
- §7.3 microVM/WASM for T3 peer code.
- §10 confidential computing + attestation (commercial hosted tier).

---

## 14. Verification (how we prove each layer is real, not aspirational)

- **Boot/disk:** the QEMU **SB harness** from `docs/secure-boot.md` (`*.secboot.fd`
  + enrolled vars) + a **swTPM** (`-tpmdev emulator`) in the dev VM → test:
  healthy silent unlock, post-update re-seal (no recovery prompt), tamper →
  recovery path, recovery-key + FIDO2 unlock. None of this needs real hardware
  except the final fTPM-bus claims.
- **Sandbox:** extend `tests/portal_test.rs` + sandbox tests with Landlock
  allow/deny cases and a T3 "peer code cannot reach a host syscall" test.
- **Supply chain:** CI verifies the image signature + that its digest is in the
  transparency log before tagging `:latest`.
- **The covenant** (`tests/concept_parity_spec.rs`): assert on the real path —
  e.g. that `qos-install --encrypt` actually creates the recovery + TPM-with-PIN
  keyslots (`cryptsetup luksDump`), not that a flag was passed.

---

## 15. Relationship to existing docs

- **Extends** `plan/security.md` (T1/T2 sandbox + identity — unchanged, still
  current).
- **Extends** `docs/secure-boot.md` (§4 adds measured boot + signed predictions
  on top of its SB assessment).
- **Upgrades** E2 (`os/usr/libexec/qos-install` `--encrypt`) from §5's naive
  TPM-only to multi-factor + recovery.
- **Builds on** A2 (CI image pipeline) for §9 signing/transparency and A3
  (update timer) for §4's re-seal-on-update.

# Security and identity architecture

Status: design. This describes the system we are building toward, not the one that runs today.

---

## 1. The thesis

Two sentences carry the whole design.

**The TPM is never the root of trust. It is a convenience for unattended unlock.**

The root is a person-scoped account key that is recoverable by construction. Devices are
authorised *by* the account, not the other way round. Everything else — disk unlock, login, peer
identity, document authorship, filesystem access — derives from that one chain.

This single decision produces the degradation story for free. On a machine without a TPM you
authenticate more often; you do not hold weaker guarantees. Same hierarchy, same revocation, same
peer identity, same access control. The hardware makes it *quiet*, not *safe*. A design where the
hardware makes it safe is a design that bricks when the motherboard dies, and a mechanism that can
permanently destroy access without a backstop is a denial-of-service against the owner.

---

## 2. Threat model

### 2.1 Adversaries

**A1 — Thief with the device.** Has physical possession, unlimited time, can remove the disk, sniff
the LPC/SPI bus, replace firmware, and attempt evil-maid persistence. Cannot break Ed25519 or
AES-256.

**A2 — Malicious or compromised application.** Runs as an app the user tried once. Wants files it
was not granted, other workspaces' data, key material, network egress, or persistence. Assume
memory-safety bugs in every C/C++ dependency we ship.

**A3 — Hostile collaborator.** A peer the user *did* invite. Wants to read beyond their grant,
write where they may only read, inject input into windows that aren't shared, keep access after
removal, or forge authority over a shared space.

**A4 — Compromised supply chain.** Wants to ship a malicious image or package, downgrade the system
to a known-vulnerable release, or sign as us.

**A5 — Honest-but-curious host operator.** Runs a relay or hosted workspace. Sees ciphertext and
routing metadata and wants content.

### 2.2 Out of scope, stated plainly

- **Rendering-code isolation from a hostile peer.** Kernel namespaces share a kernel with the code
  they confine. Doing this properly needs a microVM or a WASM runtime with no host syscalls. Until
  that exists, peer-supplied rendering code is *not* isolated to the standard we claim elsewhere.
- **Undoing disclosure.** Redaction is a cooperative compliance protocol. Cryptographic protocols
  cannot un-deliver bytes. A peer who has read something has read it.
- **Off-band capture.** Screenshots, photographs of the screen, OCR, accessibility-tree scraping.
- **Firmware-level attackers on machines with firmware TPMs.** fTPM inherits firmware attack
  surface (the faulTPM voltage-fault class). A discrete TPM is better; we do not require one.
- **Boot-time refusal of downgraded images.** See §4.4 — we can make a downgrade inert, not
  unbootable. This is a real gap versus Android Verified Boot and we state it rather than paper
  over it.

### 2.3 Trust tiers

| Tier | Principal | Posture | Boundary |
|---|---|---|---|
| T0 | kernel, compositor, filesystem daemon, portal broker | trusted, must be **verifiable** | signed UKI, sealed root, MAC, measured boot |
| T1 | first-party apps | our code, assume bugs | sandbox + capability grants |
| T2 | third-party apps | untrusted code | same, default-deny, no network |
| T3 | peer-supplied rendering code | **hostile** | out of scope today (§2.2) |
| T4 | hosted relay operator | honest-but-curious | end-to-end encryption; relay sees ciphertext + routing metadata |

---

## 3. The identity spine

One chain, five uses.

```
account key            person-scoped, recoverable, never on a single device alone
  └── device cert      per machine, hardware-bound where hardware allows, individually revocable
        ├── disk unlock       LUKS keyslot
        ├── login             per-user home credential
        ├── peer identity     the node identity on the network
        ├── document actor    CRDT authorship
        └── access subject    filesystem and capability decisions
```

### 3.1 Account key

Created once, with the person, not with the machine. Held as a threshold key across the user's
devices so that no single device holds it whole and losing one device does not lose the account.
An offline recovery share is minted **at creation time**, never later — a backstop created after
the fact is a backstop that does not exist for the people who need it most.

The account key's only jobs: sign device certificates, and sign revocations.

### 3.2 Device certificate

Binds a device key to the account. Where a TPM or FIDO2 token is present the device key is
generated in and never leaves that hardware. Where neither is present it is a software key wrapped
by the login credential. **The certificate is the same shape either way**, so every consumer of
identity is unaware of what kind of hardware, if any, sits underneath.

The set of authorised devices is a replicated CRDT set from day one, never a scalar and never a
distinguished "owner" field. A system that special-cases the first device needs a rewrite to
support the second.

### 3.3 What this replaces

Every identity in the system derives from the device certificate. There is exactly one
cryptographic identity per device, and its public half is the name other machines know it by. No
subsystem mints its own.

### 3.4 Revocation

"Forget this device" from any other device signs a revocation into the account's identity
document, which replicates like any other data. A revoked device's operations are rejected from
the revocation's timestamp forward; operations it signed before that point remain valid, because
retroactively invalidating history would destroy the audit log the system depends on.

### 3.5 Recovery

Three independent scopes, none of which can be lost by the same event:

| Loss | Recovery |
|---|---|
| Forgot PIN | Recovery key unlocks the disk |
| Dead TPM / new motherboard | Recovery key unlocks, one-tap re-seal to the new chip |
| Lost one device | Any other device revokes it; account is untouched |
| Lost every device | Offline recovery share reconstitutes the account |

The recovery key is created and displayed **at the moment encryption is enabled**, as a QR code
that can be printed or photographed, and enrollment does not complete until the user confirms they
have it. The TPM keyslot is a convenience; the recovery key is the guarantee.

### 3.6 The credential ladder

**One secret in day-to-day use. Not two, and not one-plus-occasional-surprises.**

This is a hard requirement, not an aspiration, because a system that asks for a second credential
"only sometimes" trains people to expect prompts — and a user who expects prompts is a user who
types their passphrase into whatever asks.

| Credential | When | Why it can be short |
|---|---|---|
| **PIN** | Every boot, every unlock | The TPM throttles guesses in hardware |
| **Passphrase** | Only on machines with no TPM at all | Nothing throttles guesses, so it must carry the entropy itself |
| **Recovery key** | Account recovery, dead TPM, forgotten PIN | Never typed in normal operation |

The PIN cascades rather than repeating. Entering it unseals the device key; the device key unlocks
the disk, authorises the session, and unwraps the credential vault. One entry, one cascade — not
the same secret demanded three times by three subsystems that don't know about each other.

**Machines without a TPM type exactly one secret too.** It is a passphrase rather than a PIN,
because without hardware throttling the secret itself must be strong enough to resist offline
guessing. Same number of prompts, longer string. That is the honest shape of the degradation: it
costs typing, not safety, and it does not cost a *second* prompt.

**A short PIN is only safe if guess-throttling actually works.** The TPM's dictionary-attack
lockout is what makes four digits defensible, and its reset authorisation must be set to a real
value — left at the empty default, an attacker simply resets the counter and the PIN becomes
four digits of nothing. Lockout parameters are vendor-set and often generous, so they must be
read and, where possible, tightened at enrollment rather than assumed.

### 3.7 Why the passphrase should never appear

The usual reason a Linux machine surprises you with a passphrase prompt is that a firmware update
changed the boot measurements and invalidated the seal. Sealing to *signed policy* rather than to
literal measurement values fixes this: the updater ships a signature for the new expected state, so
a legitimate firmware or kernel update re-satisfies the policy instead of breaking it.

This is usually filed as a security mechanism. It is equally a usability one — it is the difference
between a recovery credential that stays in a drawer and one the user keeps somewhere convenient
because they need it every few weeks.

### 3.8 Where the single PIN stops

Disk unlock is a **device** capability; a home directory is a **user** capability. On a
single-user machine these collapse into one PIN and one cascade, which is the case to optimise
for. On a multi-user machine the device unlocks once and each user still authenticates to their own
home — one PIN *per person*, which is correct rather than a regression.

The cascade must therefore be built as "device credential unlocks device-scoped things, user
credential unlocks user-scoped things," which happens to be one prompt when there is one user. A
cascade built as "the PIN unlocks everything" would have to be dismantled to add a second person.

---

## 4. Boot chain

Goal: a boot chain the owner can build and sign themselves, that remains attestable to third
parties afterwards. Owner-buildable and attestable are usually traded against each other; the
whole design here is arranged to avoid that trade.

### 4.1 Owner-enrolled Secure Boot

The system ships with tooling to generate the machine's own Platform Key and signing keys, enroll
them alongside the retained Microsoft KEK (so firmware updates and signed option ROMs keep
verifying), and sign the boot artifacts with the owner's key.

This mirrors the one decision worth copying wholesale from GrapheneOS: attestation is over
*whatever key the owner enrolled*, not over a vendor key. A self-built image stays fully
attestable to collaborators who choose to trust its key.

### 4.2 Unified Kernel Image

Kernel, initramfs, command line, and os-release are sealed into one signed binary. Nothing in the
boot path is an independently editable file — the command line cannot be edited to disable a
protection, because the command line is inside the signature.

The initramfs is required, not optional: unlocking an encrypted root needs early userspace.

### 4.3 Sealed root filesystem

The root filesystem is content-addressed with a single digest authenticating file contents *and*
metadata — modes, owners, xattrs, symlinks. The digest is baked into the UKI's command line, and
therefore into the signature, and therefore into the boot measurements. Integrity is enforced
continuously on the read path rather than checked once at boot.

The consequence worth noting: because the root digest is on the sealed command line, a single
measurement transitively attests the exact root filesystem.

### 4.4 Rollback protection, and its honest limit

A TPM NV monotonic counter is incremented on each successful update. Unlock secrets are sealed to
a policy requiring the counter to be at or above the current generation.

**An old image can still execute. It cannot unseal.** A downgrade produces an inert machine that
demands the recovery key, not a running vulnerable system.

This is weaker than Android Verified Boot, where the rollback index lives in tamper-evident storage
consulted by the bootloader *before* execution. Our check happens after the bootloader has already
run. We accept this and state it; we do not claim parity.

### 4.5 Security policy as a signed object

Every switch that weakens the system — developer mode, unsigned-module loading, sandbox
exceptions, root shell — lives in a single signed policy object, not in root-writable text files.
The object is protected by a nonce held in the TPM, and **changing any policy regenerates the nonce
and re-signs the whole object**, so an older, weaker policy cannot be replayed.

Weakening the system requires physical presence and a credential. The running system cannot
silently weaken itself, and neither can anything that compromises it.

### 4.6 Privileged operations are brokered

No `sudo`, no `pkexec`, no general-purpose escalation. Privileged operations are individual,
individually-authorised requests to a broker. The failure mode we are eliminating is the one where
a single authorisation prompt converts into unrestricted, indefinite control of the machine.

The thing to get right is that a locked-down system must not feel like it is fighting its owner.
Android-style lockdown reads as *gimmicky* to anyone who wants to look inside their own computer,
and an OS that makes tinkering feel adversarial has failed at something more important than the
attack it prevented.

Two commitments follow.

**Unlocking does not destroy anything.** Reaching full control is a supported operation, not a
punishment. Wiping user data to permit it — the Android bootloader model — protects the vendor's
threat model at the owner's expense, and it makes the safe path the one where you never learn how
your system works.

What unlocking costs instead is **attestability**. An unlocked machine says so: the state is
recorded where the running system cannot forge it, and it appears in what the machine attests to its
peers. You cannot unlock quietly, and you cannot unlock something you do not own. That preserves
the property the wipe was protecting — an attacker with brief physical access gains nothing durable
and invisible — while leaving the owner's data alone.

Relocking is likewise supported, and returns the machine to a fully attestable state.

**Most tinkering must not require unlocking at all.** If full control is the only way to install a
package or read a log, then the granular capability system has failed and everyone will just unlock.
The measure of success is that the unlock exists and is rarely reached: elevated operations are
requested at the granularity actually needed, and a first-class environment exists for building,
installing and experimenting that does not need host privilege at all.

### 4.7 Without a TPM

Signed UKI and sealed root still give full offline integrity. What is lost is measured boot,
rollback binding, and automatic unlock; the disk falls back to a passphrase. The machine reports
itself as unattestable and **continues to work**. Hardware absence must never become a hardware
requirement by accident.

### 4.8 Separately verifiable components, for reasons that are not security

The root filesystem is sealed under one digest, which cannot express "this kernel is suspect but
that firmware is fine". Splitting the image into independently signed components — kernel, firmware
bundles, out-of-tree modules — is worth doing, but **not** for the reason it is usually claimed.

On a monolithic kernel it buys almost no isolation. A compromised graphics module and a compromised
kernel are the same compromise: both run in ring 0, and an attacker who has either has the machine.
Verifying them separately tells you *which* component was subverted, not that the subversion was
contained. The argument for component isolation as a security boundary belongs to a system with an
actual boundary there — a microkernel — and Linux is not that system.

The real reasons, stated honestly so nobody later mistakes this for defence in depth:

- **Attack surface by omission.** A machine with no Nvidia hardware should not carry Nvidia drivers.
  Code that was never installed cannot be exploited, and that is a genuine reduction — just a
  different mechanism from isolation.
- **Install size**, for the same reason.
- **Low-level components need a distribution channel anyway.** Firmware, out-of-tree modules and
  kernel variants have to reach machines somehow, signed and verifiable. A component format is that
  channel, and inventing a second mechanism later would be worse.
- **It leaves room for a boundary that does not exist yet.** If a future kernel offers real
  intra-kernel confinement, a system that already ships components separately can use it. One that
  ships a single blob cannot without re-architecting.

This is close to conventional package management with signing and verification done properly, and
that resemblance is fine — but it should be recognised, so the design does not drift into
re-solving problems package managers already solved badly.

---

## 5. Runtime hardening

Principle: **full-strength protection where the bugs live, zero cost where they don't.** Most of
the system is Rust, which does not produce the bug classes that heavyweight allocator hardening
catches. Spending a uniform performance tax across the whole image to protect code that is already
memory-safe is the wrong trade.

There is no memory-tagging option on x86_64 and there will not be one for years. The design does
not assume one.

### Tier 0 — image-wide, effectively free

Hardware-enforced shadow stacks; full RELRO with immediate binding; position-independent
executables; fortified source and stack-clash protection for **every vendored C dependency**
(these do not inherit distribution build flags and must be set explicitly); zero-on-allocate and
zero-on-free; slab hardening; kernel stack offset randomisation; kernel lockdown; enforced module
signatures with a per-build key discarded after signing; cross-process debugging disabled.

### Tier 1 — high-risk processes only

Everything that parses untrusted bytes: the browser's content processes, media decoders, font
shaping, image codecs, the GPU stack.

Hardened allocator at full strength — out-of-line metadata, guard slabs, canaries, quarantines,
write-after-free detection. Write-xor-execute enforced by seccomp. **JIT disabled by default with
a per-site opt-in**, and dynamic code execution blocked outright for processes that never need it.
Protection keys to seal key material away from the rest of the address space. Kernel-native guard
regions rather than mapping-per-guard, so guard-heavy allocation stays cheap.

### Tier 2 — CI only

Address, memory, and thread sanitizers; allocator hardening at maximum with every check enabled.

### The constraint that must not be "hardened" later

The sandbox depends on unprivileged user namespaces. Disabling them is a standard hardening
recommendation and would break the entire application model. The correct response is to constrain
what a namespace can do from the inside, never to remove the ability to create one. This is
written down so that nobody later improves the system into a broken one.

---

## 6. Permissions

### 6.1 Two layers

**The manifest is the ceiling.** Signed with the package, immutable after signing, declaring the
maximum capability envelope. Neither the user nor the app can exceed it.

**The prompt is the grant.** Dynamic, revocable, scoped to the workspace, keyed to the package's
signing identity.

An app can never exceed its manifest even with enthusiastic user consent, and a prompt is never
the only thing standing between an app and a capability it never declared.

### 6.2 Denial is absence, not refusal

**A denial must be indistinguishable from a state the application already knows how to handle.**

`EACCES` is a state most software handles by crashing or nagging. "The folder is empty", "there is
no network", "no such file" are states every application handles correctly, because they occur
naturally.

We own the filesystem, so this is nearly free: a sandbox sees a *filtered view* in which granted
subtrees exist and everything else does not appear in a directory listing at all. An app that
enumerates the workspace sees what it was granted. Not a permission error — just less.

Network denial is an empty network namespace, so connections fail as genuinely unreachable rather
than hanging until timeout. **This must cover device-local addresses too**: an app denied the
network must not reach local services as a bypass.

### 6.3 File access is a handle, not a path

The user picks a file in a chooser the application does not draw and cannot influence. The broker
opens it and passes back an open file descriptor. The application's capability is *permission to
ask*, never permission to read.

There is no path to race, no ambient authority to re-derive, and no permission database to tamper
with. The sandbox holds no rule granting the workspace tree — the descriptor is the only way in.

### 6.4 Read-only participation is a first-class shape

An app may be granted read participation in a shared document while being structurally unable to
author changes. Per-object write consent is unusable in practice; a clean read-only capability is
both simpler and more useful — for viewers, previews, and untrusted embeds.

### 6.5 Two capabilities with no prior art

**Peer capability.** "May this app publish and subscribe on the collaboration mesh, and to which
peer identities?" A peer identity is a first-class capability object, granted per workspace like a
file.

**Remote input injection.** A remote peer injecting input is functionally a second local user with
no local presence. It is a per-session capability, individually revocable, continuously and
visibly indicated, and never a workspace-level grant that persists silently. This is the one place
in the design with nothing to copy, and it deserves the most care.

---

## 7. Collaboration authority

Two checks with distinct jobs. Collapsing them loses something either way.

### 7.1 Admission — guards the door

Decides whether a peer may synchronise a space at all, and whether a connection's bytes may touch
a document. Per-connection, cheap, fail-fast.

Authority rests on **who the transport authenticated, never on what the message claims** — that is
the only thing a receiver knows about a payload that its author did not write. Authority over a
space is pinned on first contact and cannot be silently re-pinned, so sending someone a second
invitation is never a way to re-point them at an attacker.

Inbound changes are validated **before** they are merged, against a copy, and a batch that fails is
refused whole. A check that runs after the merge has already happened is not a check. Partial
application is worse than refusal, because it advances state past changes that were never accepted.

### 7.2 Authorship — guards the content

Decides whether an operation is something its claimed author was permitted to perform. Per
operation, verified against the author's key, **independent of who delivered it**.

This is what admission cannot do. A relay, by definition, delivers operations it did not author.
Under admission alone a receiver can only ever conclude "the relay sent this" — authority does not
survive the hop. Since always-on replicas and store-and-forward are core to the product, operations
must carry their own proof.

Roles are attestations issued by a space's owner, revocable, and themselves replicated as signed
data. Each operation carries a sequence number so a replayed operation is rejected.

### 7.3 Why both

The hop check gates the *connection*. The operation check gates the *change*. A relay passes the
hop check by being an admitted peer; the operations it carries still verify independently against
their authors. Neither is redundant.

The risk is drift — someone later making one authoritative and hollowing out the other. The
mitigation is one shared decision point and a test that pins the boundary, so the two cannot
silently converge.

### 7.4 Confidentiality from the relay

Content is encrypted to the group; routing metadata stays clear so a relay can do its job without
reading anything. The group key rotates on **every** membership change — most importantly on
removal, which is the case that matters and the one that is easy to leave for later.

---

## 8. Attestation between people

Everyone builds attestation so a device can prove itself to a vendor. The interesting version here
is the opposite: **prove yourself to your collaborators, with no central service.**

Before accepting remote input or buffer streaming, a peer may demand proof of what the other
machine is running. The attestation key is bound to the peer's network identity, so the machine
that attests and the machine that connects are provably the same machine.

Trust is pinned on first contact, per peer — the same shape as pairing a device. Afterwards, a
changed boot key, a changed image, or a rolled-back version are all detectable.

Policy is tiered by what a peer is willing to prove:

| Peer state | May do |
|---|---|
| Unattested | View shared content |
| Attested, pinned image | Edit |
| Attested, pinned image, explicit grant | Inject input |

**Verifiers pin what the owner enrolled, not what we sign.** Self-built images stay fully
attestable to collaborators who chose to trust them. This is the property nearly every verified-boot
system throws away, and keeping it is the point.

**"Unattested" is a first-class state, not a failure.** A machine with no TPM participates, with
fewer privileges. The moment unattested peers cannot federate, the TPM has silently become
mandatory.

### The structural shortfall

A discrete TPM has no per-application isolation — anything that can reach it speaks for the whole
machine. Per-app attestation is therefore brokered *in software* by a privileged service, and the
guarantee degrades from "the hardware attests which app asked" to "the image was measured, and that
measured image's broker asserts which app asked." Strictly weaker, and it matters the moment the
broker is the target.

Boot measurements are also boot-time. Nothing re-measures userspace afterwards. What is attested is
that *the configuration which enforces integrity was present at boot*, not that integrity holds
right now. Continuous enforcement comes from the sealed root, not from attestation.

---

## 9. What we do not claim

- That a downgraded image will refuse to boot. It will fail to unseal (§4.4).
- That peer-supplied rendering code is isolated to the standard claimed elsewhere (§2.2).
- That redaction can un-share data (§2.2).
- That attestation proves anything about the running state after boot (§8).
- That a machine without a TPM is equally protected against a thief. It is equally protected
  against every *other* adversary, and that distinction is the design (§4.7).
- That separately signed components isolate a compromise. They identify it. On a monolithic kernel
  those are different things (§4.8).
- That an unlocked machine is as trustworthy as a locked one. It is as *usable*, and it says which
  it is (§4.6).

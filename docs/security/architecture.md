# Security and identity architecture

Status: design. This describes the system we are building toward, not the one that
runs today. What runs today, and where it falls short, is [defects.md](defects.md)
— keep the two separate rather than annotating this document with current-code
caveats.

Deliberately not repeated here: the boot chain's mechanics
([boot-chain.md](boot-chain.md)), the rollback contract ([rollback.md](rollback.md)),
why the deployment backend is composefs ([composefs-decision.md](composefs-decision.md)),
and how tinkering works without ambient root ([tinkering.md](tinkering.md)).
[README.md](README.md) is the map.

---

## 1. The thesis

The design has two governing principles.

**The TPM is never the root of trust. It is a convenience for unattended unlock.**

The root is a person-scoped account key that is recoverable by construction. Devices are
authorised *by* the account, not the other way round. Everything else — disk unlock, login, peer
identity, document authorship, filesystem access — derives from that one chain.

On a machine without a TPM, authentication occurs more often, but the guarantees remain the same:
the same hierarchy, revocation, peer identity and access control apply. The hardware simplifies
unlock; it is not the security root. A design that depends on the hardware for safety can lose
access when the motherboard fails, so recovery must remain independent of the TPM.

---

## 2. Threat model

### 2.1 Adversaries

**A1 — Thief with the device.** Has physical possession, unlimited time, can remove the disk, sniff
the LPC/SPI bus, replace firmware, and attempt evil-maid persistence. Cannot break Ed25519 or
AES-256.

**A2 — Malicious or compromised application.** Runs as an app the user tried once. Wants files it
was not granted, other workspaces' data, key material, network egress, or persistence. Assume
memory-safety bugs in every C/C++ dependency we ship.

**A3 — Hostile member.** A peer the user *did* invite. Wants to read beyond their grant,
write where they may only read, inject input into windows that aren't shared, keep access after
removal, or forge authority over a shared space.

**A4 — Compromised supply chain.** Wants to ship a malicious image or package, downgrade the system
to a known-vulnerable release, or sign as us.

**A5 — Honest-but-curious host operator.** Runs a server or hosted workspace. Sees ciphertext and
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
- **Boot-time refusal of downgraded images.** See §4.4 — a downgrade can be made inert, not
  unbootable. This remains a gap versus Android Verified Boot.

### 2.3 Trust tiers

| Tier | Actor | Posture | Boundary |
|---|---|---|---|
| T0 | kernel, compositor, filesystem daemon, portal broker | trusted, must be **verifiable** | signed UKI, sealed root, MAC, measured boot |
| T1 | first-party apps | our code, assume bugs | sandbox + capability grants |
| T2 | third-party apps | untrusted code | same, default-deny, no network |
| T3 | peer-supplied rendering code | **hostile** | out of scope today (§2.2) |
| T4 | hosted server operator | honest-but-curious | end-to-end encryption; server sees ciphertext + routing metadata |

---

## 3. The identity spine

One chain, five uses.

```
account key            person-scoped, restorable from its 24 recovery words
  └── device cert      one per computer, over that computer's one device key, individually revocable
        ├── disk unlock       LUKS keyslot
        ├── login             per-user home credential
        ├── peer identity     the node identity on the network
        ├── document actor    CRDT authorship
        └── access subject    filesystem and capability decisions
```

### 3.1 Account key

Created once, with the person, not with the machine, and kept by `tilo_identity::store`: one
Ed25519 key in the person's identity directory (`$XDG_CONFIG_HOME/tilo/identity`), its secret in
the Secret Service or a 0600 file ([identity-security.md](../identity-security.md)). It is one
key, held whole on each computer the person keeps it on. Losing one such computer loses nothing, and losing all of them is
what the recovery words are for (§3.5).

The account key's only jobs: sign device certificates, and sign revocations.

### 3.2 Device certificate

Binds a device key to the account (`tilo_user_identity::DeviceCert`). Each computer has exactly
one device key, kept by `tilo_device_identity::IdentityPaths`: a file only the device daemon's own
account can read (`device-private/keypair.bin`, owner-only in a directory of that account), or on
macOS a login Keychain item. Only the device's daemon reads
it: the other programs of the computer ask the daemon to seal, prove and sign with it. It is a software key, created
before anyone signs in because the machine needs its identity first ([sign-in.md](sign-in.md) §2).
No device key is generated in or sealed to a TPM or FIDO2 token today. If one is, the certificate
keeps the same shape, so no consumer of identity has to know what holds the key.

The set of authorised devices is one signed list per person, never a scalar and never a
distinguished "owner" field. It is kept in the person's identity directory and checked against
their root on every read. A device is added one way, `join_with_token`: a code one of the
person's devices approves, and a login code, a QR code and a boot-key request only supply it.

A certificate that carries an expiry is renewed before it runs out — by a device holding the
account key, or, for a device that holds none, through the person's other devices that do. How
that works, and what it is allowed to grant, is in
[device-certificate-renewal.md](device-certificate-renewal.md).

**Two clocks never agree, and one rule says how far they may differ: 60 seconds**
(`CERTIFICATION_CLOCK_ALLOWANCE_NS`). Whoever dates a thing applies it; no check of a certificate
is loosened for it, and every verifier keeps a certificate's exact start and exact expiry. A new
device's pairing request may be dated up to that far ahead of the clock that checks it. A
certificate the person's root issues for another machine to read the moment it arrives is valid
from that long before the issuer's clock: the certificate of a paired device, and, since
17 November 2026, the two a person's Tilo on the web answers a service's sign-in with (the
browser's own, which the proof carries, and the grant's for an agent or an API key).

Accepted since then that was refused before: an approval on the sign-in page from a computer whose
clock is up to 60 seconds ahead of the API server's. Until then it was refused as not yet valid at
any difference at all, 6.5 ms on a real site. It gives nothing to anyone who does not hold the
person's root key and the device's key. Only the root signs the two certificates, and the device's
key still signs the server's own challenge. The server's checks are unchanged: a computer more
than 60 seconds ahead, a certificate past its expiry, a revoked device and another server's
challenge are refused exactly as before. The certificates name the same keys and grant the same.
The grant ends when the server asked, and the most it may last counts from its earlier start; the
browser's 30 days count from the earlier start too, so nothing lasts longer than it did.

### 3.3 What this replaces

Every identity in the system derives from the device certificate. There is exactly one
cryptographic identity per device, and its public half is the name other machines know it by. No
subsystem mints its own: `tilo login` and the agents it serves sign with the `IdentityPaths` key.
The certificate the person's root signed for the key is kept beside it, in
`device/certs/<user>.bin`, and its label names the agent that runs there. Nothing else of a login
is written down: the daemon keeps the server the ticket named in memory, and after a restart reads
the person's server from its copy of their own workspace. Other programs ask the daemon
(`account#who`).

#### Adding a computer with a ticket

1. On a device of the person's, `tilo device add` prints a ticket: that device's iroh endpoint id,
   where it is reached (its relay and direct addresses) and a one-time secret, as one string. It
   lasts ten minutes and is spent by its first good use; ten wrong secrets spend it too.
2. On the new computer, `tilo login <ticket>`. The computer loads its device key, or creates it
   if it has none yet, dials the issuing device directly and calls its `tilo.device`
   `devices#certify` action (`tilo_device_identity::login::CertifyInput`: the secret, the
   computer's public key and the agent that runs on it). No site, DNS name or server is asked.
3. The issuing device admits a connection with no certificate for that one action alone, in its
   own scope (`tilo.new-device`); every other call on it is refused. It checks that the key asked
   for is the key the connection comes from and spends the secret.
4. An approver answers. It may be another device than the one serving the ticket: it gets the
   computer's key, label and agent and the hash of the ticket's secret, signs the
   `DeviceCert` with the person's root, and signs a digest of those fields
   (`tilo-device-approval-v1`) with its own device key. The serving device checks that signature
   against the approver's own root-signed, unrevoked certificate, and that the certificate it
   was given is the one it asked for, before it answers. On a computer that holds the root,
   `tilo device add` is that approver itself.
5. The computer gets the certificate and where to enter (the workspace and its server's
   certificate and address, `CertifyAnswer`). Its daemon, which `tilo login` asks
   (`devices#logIn`), files the certificate beside its device key, starts its person's part again
   as that device, and enters through that server over its one session.

A computer keeps one device key, and a login is never kept with a key of its own. On a computer whose Tilo desktop is running, the desktop
is the device already, and `tilo login` refuses rather than open a second session under the same
key.

### 3.4 Revocation

"Forget this device" from any other device signs a revocation into the account's identity
document, which replicates like any other data. Removing a device takes its future, not its
person's past: the operations a workspace had taken from it when the removal was written down remain
valid for everyone, because retroactively invalidating history would destroy the audit log the
system depends on, and would leave a workspace unreadable for every device added later.
Everything else the device signs is rejected.

Where the past ends is a place in each document, never a time. A revocation's timestamp is its
signer's and an operation's timestamp is its author's, so no clock can say what a removed device
wrote before: it would only have to date its operations earlier. Every operation carries its
author's own running number per document, so the device that writes a removal into a workspace's
governance writes its **cut** beside it: for each document, the number of the removed device's last
operation the workspace had taken. An operation at or below its document's cut is history, whatever
the reason for the removal. A compaction restates history under a number its author used before, so
the cut also names each compaction of the removed device that the workspace had taken, and no other
compaction of its is history. One above the cut, or in a document the cut does not name, is refused by
every device and holder, whoever hands it over and whatever time it claims. A replica that knows of
a removal and not yet of its cut holds that device's operations undecided, and a cut is never
lowered or erased. The device itself is refused everywhere from the instant its removal is known:
at the handshake, for everything it asks after it, and for keys.

At the handshake the device is also told so, and told nothing else. Where the removal alone refuses
it, its certificate being valid and its person admitted there, the server answers its hello with
that one reason, signed for that hello as an acknowledgement is. The device believes it only from
the server it dialled, by the check it makes of that server's acknowledgement, stops asking, and
tells its person that it was removed. Who removed it, when, for what reason and which other
devices the person has are never said. The reason gives away no more than the refusal does:
nothing else refuses that certificate there, and a device that enters through a workspace's entry
is told only once its proof of its key verified.

For a device that was stolen or compromised the person may also name the moment from which it is
not to be trusted. What it wrote after that moment and before its removal was written down cannot
be untaken, since devices merged it before anyone knew. It stays in its document marked as written
while the device was not trusted, is handed to apps with that mark and not as its person's own act,
and the document's history offers to take it back. The moment becomes a second place per document,
fixed by the governing device from when each operation reached it, which it notes itself: the
removed device's own dates are never asked.

### 3.5 Recovery

Three independent scopes, none of which can be lost by the same event:

| Loss | Recovery |
|---|---|
| Forgot PIN | The disk's recovery key unlocks it ([sign-in.md](sign-in.md) §4.5) |
| Lost one device | Any other device revokes it; account is untouched |
| Lost every device | The account's 24 recovery words restore its key on a new computer |

The account's recovery words are its 32-byte seed written as 24 BIP39 words
(`tilo_identity::store::recovery_words`). Onboarding shows them when it creates the person, and
they restore the identity in onboarding or with `tilo login --recovery-key`. They are the only
recovery format: there is no recovery share, and nothing reconstitutes an account from parts.

### 3.6 One secret

**Day-to-day use must require one secret.**

This is a hard requirement, not an aspiration, because a system that asks for a second credential
"only sometimes" trains people to expect prompts — and a user who expects prompts is a user who
types their passphrase into whatever asks.

The secret is the person's login, typed once per boot at a screen that is the login screen. On a
verified boot the TPM unlocks the system by itself and the person signs in; when the boot can't
be verified, or there is no TPM, the login screen comes first and the same secret opens the system
too. Everything personal is encrypted under a per-person key that only that person's factors
release, so device-scoped things unlock with the device and user-scoped things with the user —
one prompt when there is one person, one per person when there are several. A fingerprint, a
security key or an approval from another of the person's devices stands in for the secret where
that is safe; the password always works.

| Credential | When | Why it can be short |
|---|---|---|
| **PIN or password** | Every sign-in, unlock and privileged prompt | On TPM machines the TPM throttles guesses in hardware |
| **Passphrase** | The same, on machines with no TPM at all | Nothing throttles guesses, so it must carry the entropy itself |
| **Recovery key** | Forgotten secret, cleared TPM, lost factors | Never typed in normal operation |

**A short PIN is only safe if guess-throttling actually works.** The TPM's dictionary-attack
lockout is what makes six digits defensible, and its reset authorisation must be set to a real
value — left at the empty default, an attacker simply resets the counter and the PIN becomes
six digits of nothing. Lockout parameters are vendor-set and often generous, so they must be
read and, where possible, tightened rather than assumed.

The design — storage scopes, the per-person key and its wrappers, what each screen offers,
development machines that sign what they boot — is in [sign-in.md](sign-in.md).

### 3.7 Why the passphrase should never appear

The usual reason a Linux machine surprises you with a passphrase prompt is that a firmware update
changed the boot measurements and invalidated the seal. Sealing to *signed policy* rather than to
literal measurement values fixes this: the updater ships a signature for the new expected state, so
a legitimate firmware or kernel update re-satisfies the policy instead of breaking it.

This is usually filed as a security mechanism. It is equally a usability one — it is the difference
between a recovery credential that stays in a drawer and one the user keeps somewhere convenient
because they need it every few weeks.

---

## 4. Boot chain

Goal: a boot chain the owner can build and sign themselves, that remains attestable to third
parties afterwards. The design supports both owner-built images and attestation.

### 4.1 Owner-enrolled Secure Boot

The system ships with tooling to generate the machine's own Platform Key and signing keys, enroll
them alongside the retained Microsoft KEK (so firmware updates and signed option ROMs keep
verifying), and sign the boot artifacts with the owner's key.

This follows GrapheneOS's model in which attestation covers *whatever key the owner enrolled*,
not a vendor key. A self-built image remains attestable to members who choose to trust its
key.

### 4.2 Unified Kernel Image

Kernel, initramfs, command line, and os-release are sealed into one signed binary. Nothing in the
boot path is an independently editable file — the command line cannot be edited to disable a
protection, because the command line is inside the signature.

The initramfs is required, not optional: unlocking an encrypted root needs early userspace.

A static unified image embeds a command line that must name the deployment it boots, and on the
ostree backend that name changes on every update; only the content-addressed deployment backend
described in §4.3 can carry one. The image is on that backend and builds the unified image
([boot-chain.md](boot-chain.md), [composefs-decision.md](composefs-decision.md)), which upstream
still marks experimental.

On the ostree backend Tilo left, signature verification covered the kernel and the boot loader but
not the initramfs or the command line, so secure boot provided a **verified kernel, not a verified
boot**. [boot-chain.md](boot-chain.md) keeps that account as history.

### 4.3 Sealed root filesystem

The root filesystem is content-addressed with a single digest authenticating file contents *and*
metadata — modes, owners, xattrs, symlinks. The digest is baked into the UKI's command line, and
therefore into the signature, and therefore into the boot measurements. Integrity is enforced
continuously on the read path rather than checked once at boot.

Because the root digest is on the sealed command line, a single measurement transitively attests
the exact root filesystem.

### 4.4 Rollback protection and its limitations

A TPM NV monotonic counter is incremented on each successful update. Unlock secrets are sealed to
a policy requiring the counter to be at or above the current generation.

**An old image can still execute. It cannot unseal.** A downgrade produces an inert machine that
demands the recovery key, not a running vulnerable system.

This is weaker than Android Verified Boot, where the rollback index lives in tamper-evident storage
consulted by the bootloader *before* execution. This check happens after the bootloader has
already run, so it does not provide bootloader-level rollback protection.

**Generation binding rides on the measured command line.** The unified image's command line
carries the generation, and the register that measures the image therefore differs per generation.
The objection to using such a register — that it changes on every update — applies only to a
*fixed* policy. A signed per-generation prediction is the mechanism for a register that moves; the
register choice and policy order are in [boot-chain.md](boot-chain.md).

**No register provides anti-rollback by itself.** An old signed prediction stays valid
forever, so an attacker who restores an older signed kernel produces a measurement the TPM will
honour. Refusing that needs monotonic state the attacker cannot wind back — the NV counter in this
section — and standard enrollment tooling does not expose it. Measured boot proves *what* ran,
not that what ran is *current*; refusing an old but validly signed image requires monotonic state.

The mechanism is built and tested against an emulated TPM, and the installer enrolls it on
verified TPM2 installs ([rollback.md](rollback.md)). Physical TPM validation is outstanding.

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

The locked-down system must retain an owner-controlled path for inspection and modification
without granting ambient root.

Two commitments follow.

**Unlocking does not destroy anything.** Reaching full control is a supported operation. Wiping
user data to permit it would protect the vendor's threat model at the owner's expense.

Unlocking changes **attestability**. The state is recorded where the running system cannot forge it
and appears in what the machine attests to its peers. Unlocking requires ownership and cannot be
performed quietly, while the owner's data remains intact.
Brief physical access must not let an attacker gain durable or invisible access
through unlocking.

Relocking is likewise supported, and returns the machine to a fully attestable state.

**Routine development must not require unlocking.** Elevated operations are requested at the
granularity needed, and a first-class environment exists for building, installing and experimenting
without host privilege.

Two mechanisms serve those two needs and remain separate:

- **Work layers.** A namespace where you are uid 0, a package manager works, and every change
  lands in an overlay that can be inspected, kept, or discarded. No privilege is granted, the host
  filesystem is never mounted, and a wrecked layer cannot survive a reboot. This is where nearly all
  tinkering should happen.
- **Capability leases.** Individual verbs, granted per session with an argument constraint and a
  duration. The invariant that makes them safe is that **no capability grants code execution as
  root**: a verb takes a unit name or a signed module name, never a command. This is exactly where
  a conventional escalation helper that runs an arbitrary program removes this constraint.

**A writable system directory is not a capability.** It is the one operation that unavoidably grants
arbitrary root code execution, because a session that can write the system tree can leave a binary
that the *next* boot executes outside any namespace. A temporary write does not preserve integrity,
so this operation is not leasable at any granularity. It is the unlock described above, with
everything that entails: an
explicit act, recorded where the running system cannot forge it, visible in what the machine attests,
and reversible by relocking. It must also clone a pristine rollback target first, so the way back is
guaranteed rather than hoped for.

Wanting a package manager must not put someone on the path that weakens the system. Unlocking the
whole system directory for the rest of the boot does so and is therefore reserved for explicit
owner-authorized unlocking.

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

The reasons for separate components are:

- **Attack surface by omission.** A machine with no Nvidia hardware should not carry Nvidia drivers.
  Code that was never installed cannot be exploited, which reduces attack surface without
  providing isolation.
- **Install size**, for the same reason.
- **Low-level components need a distribution channel anyway.** Firmware, out-of-tree modules and
  kernel variants have to reach machines somehow, signed and verifiable. A component format is that
  channel, and inventing a second mechanism later would be worse.
- **It leaves room for a boundary that does not exist yet.** If a future kernel offers
  intra-kernel confinement, a system that already ships components separately can use it. One that
  ships a single blob cannot without re-architecting.

This resembles signed package management; the design should retain those established functions
instead of duplicating them.

---

## 5. Runtime hardening

Principle: use stronger protection in processes that parse untrusted bytes and avoid unnecessary
overhead elsewhere. Most of the system is Rust, which does not produce the bug classes that
heavyweight allocator hardening catches. Applying that overhead across the whole image would be
an unnecessary tradeoff.

There is no memory-tagging option on x86_64 and there will not be one for years. The design does
not assume one.

### Tier 0 — image-wide, effectively free

Hardware-enforced shadow stacks; full RELRO with immediate binding; position-independent
executables; fortified source and stack-clash protection for **every vendored C dependency**
(these do not inherit distribution build flags and must be set explicitly); zero-on-allocate
(zero-on-free is deliberately left off: it zeroes all RAM on one CPU at every boot and switches
alloc-time zeroing off, for a shorter lifetime of freed data that matters only to an attacker who
can already read kernel memory; `os/usr/lib/bootc/kargs.d/01-hardening.toml` records the
decision); slab hardening; kernel stack offset randomisation; kernel lockdown; enforced module
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
recommendation and would break the application model. Hardening must therefore constrain namespace
operations rather than disable namespace creation.

---

## 6. Permissions

### 6.1 Two layers

**The manifest is the ceiling.** Signed with the package, immutable after signing, declaring the
maximum capability envelope. Neither the user nor the app can exceed it.

**The prompt is the grant.** Dynamic, revocable, scoped to the workspace, keyed to the package's
signing identity.

An app can never exceed its manifest even with enthusiastic user consent, and a prompt is never
the only thing access between an app and a capability it never declared.

### 6.2 Denial is absence, not refusal

**A denial must be indistinguishable from a state the application already knows how to handle.**

`EACCES` is a state most software handles by crashing or nagging. "The folder is empty", "there is
no network", "no such file" are states every application handles correctly, because they occur
naturally.

We own the filesystem, so a sandbox sees a *filtered view* in which granted
subtrees exist and everything else does not appear in a directory listing at all. An app that
enumerates the workspace sees only what it was granted.

Network denial is an empty network namespace, so connections fail as unreachable rather
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

### 6.5 Two additional capabilities

**Peer capability.** "May this app publish and subscribe on the collaboration mesh, and to which
peer identities?" A peer identity is a first-class capability object, granted per workspace like a
file.

**Remote input injection.** A remote peer injecting input is functionally a second local user with
no local presence. It is a per-session capability, individually revocable, continuously and
visibly indicated, and never a workspace-level grant that persists silently. This capability
requires particular care because it has no local-user equivalent.

---

## 7. Collaboration authority

The two checks protect distinct boundaries.

### 7.1 Admission — guards the entry

Decides whether a peer may synchronise a space at all, and whether a connection's bytes may touch
a document. Per-connection, cheap, fail-fast.

Authority rests on **who the transport authenticated, never on what the message claims** — that is
the only thing a receiver knows about a payload that its author did not write. Authority over a
space is pinned on first contact and cannot be silently re-pinned, so sending someone a second
invitation is never a way to re-point them at an attacker.

Inbound changes are validated **before** they are merged, against a copy, and a batch that fails is
refused whole. Validation after merging cannot prevent an invalid change from advancing state.
Partial application is worse than refusal because it advances state past changes that were never
accepted.

### 7.2 Authorship — guards the content

Decides whether an operation is something its claimed author was permitted to perform. Per
operation, verified against the author's key, **independent of who delivered it**.

This is what admission cannot do. A server, by definition, delivers operations it did not author.
Under admission alone a receiver can only ever conclude "the server sent this" — authority does not
survive the hop. Since always-on replicas and store-and-forward are core to the product, operations
must carry their own proof.

A role comes from the workspace's member list, which the host keeps and each member's admission
follows. Each operation carries a sequence number so a replayed operation is rejected.

### 7.3 Why both

The hop check checks the *connection*. The operation check checks the *change*. A server passes the
hop check by being an admitted peer; the operations it carries still verify independently against
their authors. Neither is redundant.

The two checks must remain separate. A shared decision point and a boundary test enforce that
separation.

### 7.4 Confidentiality from the server

Content is encrypted to the group; routing metadata stays clear so a server can do its job without
reading anything. The group key rotates on **every** membership change, including removal.

### 7.5 Signed invocations

An action a host runs for someone is asked by them, under their own key. The caller's device (a
person's computer, a browser tab's key, an agent's grant, a key another key minted) signs each
invocation it sends a host: the workspace, the invocation id, the target (provider, resource,
action), a BLAKE3 hash of the canonical arguments, the invocation it continues, the issue time and
the signing key. The envelope carries the signer's certificate, with the certificate that issued
it when another key minted it, so it checks against the person's root with nothing else at hand.
The host never signs an invocation in a caller's place.

The host verifies the signature, that it covers the request it came with, the certificate chain,
its revocation, a grant's workspace scope and the issue time's freshness, then admits and decides
the action **as the verified signer**: the workspace's member list and the signer's grants, never the
connection's say. A denial stays a denial. One invocation id names one request: the same id signed
again for the same request is a retry, answered with what the first did; for anything else it is
refused.

A change the host makes for an invocation is signed by the host, as the one who ran it, and keeps
the caller's signed invocation in its history entry, with the argument it signed (left out when it
is larger than what the entry documents of an action whole). One invocation id signed again for
another request is answered as a conflict.

What a peer can check from a signed invocation:

- **Who asked for which action on which resource.** The signer's key and certificate chain, the
  workspace, the target and the invocation id are all under the signature.
- **That the change is the one asked for.** The signed invocation id is the change's own, the
  workspace is this one, and the person who signed is the invoker the change names.
- **The arguments, if it can read them.** A peer that can read a sealed resource's content can hash
  the arguments the change keeps and compare them against the signed hash. A peer that cannot read
  the resource has nothing to compare: the hash tells it nothing about the arguments.
- **Not that the host ran it faithfully.** The envelope proves who asked, not what the executor did
  with the request. A change the executor writes is still checked as its own operation (§7.2).

---

## 8. Attestation between people

Attestation can prove a device to a vendor or to members. Tilo uses the latter without a
central service: **prove yourself to your members.**

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

**Verifiers pin what the owner enrolled, not what the distributor signs.** Self-built images remain
attestable to members who choose to trust their key.

**"Unattested" is a first-class state, not a failure.** A machine with no TPM participates with
fewer privileges. Unattested peers remain able to federate, so the TPM is optional.

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
- That the boot chain in §4.2–4.3 is in force on a machine whose owner has not enrolled Secure
  Boot keys. The image ships no vendor-signed loader; until the owner enrolls, firmware boots an
  altered UKI. Its measurement differs, so a TPM-bound unlock does not open for it, but a typed
  passphrase or recovery key still does ([boot-chain.md](boot-chain.md)).
- That measured boot prevents downgrade. It proves what ran, not that what ran is current; refusing
  an old-but-validly-signed image needs monotonic state, which is a separate mechanism (§4.4).

### What is verified, and what is only built

The split is recorded explicitly:

**Verified by execution:** the identity spine, with a ceremony-minted key proven to sign for its own
recorded public key; the runtime hardening set, each mechanism checked in the binary or the running
kernel rather than assumed from a build flag; permission denial being indistinguishable from
absence, including an adversarial errno-comparison test that caught a leak; the rollback
mechanism against an emulated TPM, including a header-restore attack performed rather than argued;
and startup resilience, proven by reproducing the abort against the previous code.

**Booted in virtual machines, not on a hardware matrix:** the boot chain. The composefs UKI
boots, and `os/tests/rollback-boot-check.sh` passes, on a VM booted from an image
`os/dev/build-image.sh` built; LUKS enrolment is asserted to open the volume during the image
build (`os/tests/luks_enrolment.sh`); Secure Boot enforcement of owner-enrolled keys is probed
against an OVMF variable store (`os/tests/secureboot_vm_probe.sh`). Firmware enrollment and TPM
behaviour vary by device, and none of this has run across real hardware. The tooling refuses to
act when its preconditions are absent, which is the right failure mode, but a refusal that has
never been exercised end to end is a claim like any other.

The current evidence covers the components furthest from the hardware best; the boot chain has
virtual-machine evidence only.

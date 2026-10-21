# Tilo threat model and assurance boundary

Status: **as built on this branch**, 2026-09-01. This document is the security
review entry point. It separates controls we can test in software from claims
that require release infrastructure, firmware, or physical hardware evidence.
It does not claim GrapheneOS equivalence.

## Security objective

Tilo aims to keep a person's data and identity confidential, authentic and
recoverable when an app, collaborator, network, registry, or stolen powered-off
device is hostile. Ordinary use has one boot credential: a TPM-throttled PIN on
a verified Tilo installation, or one high-entropy passphrase where TPM2 is
unavailable. Convenience must not silently select a weaker security mode.

The `Tilo Desktop installed on an existing Linux distribution` path is a clearly
labelled compatibility tier. It may associate identity with the existing Unix
account and must not create another password, but it cannot inherit Tilo's
verified boot, encrypted-home, measured-unlock, MAC, or immutable-release
guarantees. Existing identity state is never silently migrated between tiers.

## Assets and invariants

1. Person root keys and recovery material are never treated as device identity.
2. Device keys are individually certified and revocable.
3. User data at rest is unavailable without a TPM-gated PIN or strong recovery
   credential; the recovery route remains independent of the TPM.
4. Boot-critical code, initramfs, kernel arguments and root digest are bound to
   an owner-enrolled Secure Boot key through a UKI.
5. A confirmed rollback floor cannot move backwards by restoring disk state.
6. Apps start with no authority beyond an explicit package/runtime policy.
7. Collaboration authority is scoped to a room, subject and action; a bearer
   invite can never grant ownership.
8. A release channel never exposes an image before signature, SBOM, provenance,
   reproducibility and boot gates complete.

Availability, metadata privacy, and recovery are explicit assets too. A design
that preserves secrecy by irrecoverably losing a user's data has failed.

## Adversaries

- A thief has a powered-off device, can remove or snapshot storage, modify the
  ESP, replace firmware, and make unlimited offline guesses.
- A malicious tapp app has attacker-chosen native code and inputs. It may exploit
  its own dependencies and probe every syscall, path, socket and inherited fd.
- A compromised legacy desktop app runs under the compatibility Linux account.
- A hostile collaborator has a valid identity and whatever old material they
  legitimately received. They may replay, reorder, fork and forge network data.
- A network/relay operator observes endpoints, timing and sizes, drops or replays
  traffic, and tries machine-in-the-middle substitution.
- A registry attacker can rewrite, delete and split-view tags and blobs but lacks
  the release signing key.
- A CI dependency or runner is malicious. A KMS/HSM operator and the protected
  release branch are separate high-value trust domains.
- A local kernel exploit has kernel authority. Sandboxing limits reachability and
  persistence opportunities; it does not survive a fully compromised kernel.
- A malicious firmware or hardware implant is in scope for detection only where
  measured/verified boot and independent attestation can observe it. Defeating a
  physical implant is not claimed.

Cryptographic assumptions are Ed25519, BLAKE3, SHA-256 and the configured disk
cipher remaining secure; random generation and implementations must also remain
sound. Side-channel resistance beyond the platform/toolchain is not claimed.

## Trust boundaries

```text
firmware + owner db key
  -> signed UKI (stub, kernel, initramfs, cmdline, root digest, generation)
    -> kernel / LSM / composefs / system services
      -> Tilo brokers and compositor
        -> Landlock + namespaces + seccomp tapp process

person root -> device certificate -> transport key -> signed collaboration op

source + pinned inputs -> two builds -> boot gate -> candidate digest
  -> KMS signature + SBOM + SLSA + transparency log -> release channel
```

Crossing inward requires evidence; a name, UID, registry tag, relay connection,
or locally cached role is not evidence by itself.

## Abuse-case analysis and current controls

| Attack | Required control and current evidence | Residual risk / status |
|---|---|---|
| Offline disk theft and guessing | LUKS with an independent recovery key. Verified TPM2 installs bind a short PIN to release-authorized PCR11 measurements and the device's direct PCR7 state; every missing or changed prerequisite falls back to requiring a strong ordinary passphrase, never PCR7-only enrollment. | DMA/cold-boot attacks against a running device remain. |
| Evil-maid ESP/initramfs/cmdline change | Owner-enrolled Secure Boot; signed UKI embeds initramfs, command line, os-release, root digest and generation. Preflight verifies every section and signature. | Firmware UI and key-enrollment correctness vary by device and need a hardware matrix. |
| Root filesystem replacement | Composefs/fs-verity root digest is sealed into the UKI command line. Release VM checks the installed image contract. | Kernel/composefs vulnerabilities remain trusted-computing-base defects. |
| OS downgrade by disk snapshot | Per-generation LUKS secrets require a TPM NV monotonic floor; confirmation advances only after health, retains a bounded recovery window, and proves another unlock route before deleting a slot. Emulator tests restore old LUKS headers adversarially. | An old image may execute before unlock refusal; this is weaker than bootloader-enforced Android rollback indexes. Physical TPM validation is outstanding. |
| Registry adds a fake `genN` | Generation resolver requires a release signature and SLSA generation match; history must be dense and signed `latest` must equal the highest generation. Promotion additionally requires a digest/generation/source-bound freshness attestation lasting at most seven days. | The freshness statement is initially signed by the release trust domain. A registry can freeze a *new* machine on an older complete view until clients require independent witness signatures; installed devices retain their TPM floor. |
| Release key theft | CI accepts only a non-exportable KMS/HSM URI; no file or environment private key. Candidate is digest addressed and signed after boot verification. | KMS policy, workload identity, quorum, audit alerts and disaster recovery are deployment obligations, not proven by this repository. |
| Compromised action/dependency | OS-release actions and OS/base/RPM/kernel inputs are immutable digest/commit pinned; SLSA records inputs. | A malicious pinned input is still malicious. Dependency review and builder diversity remain necessary. |
| Non-reproducible/backdoored release | A second no-cache OS build must have the same OCI manifest before publication. If installation adds the deterministic UKI reseal layer, the gate independently applies it and compares the final publishable manifests too. SBOM and SLSA attestations bind that digest. | Both invocations currently share one runner/operator/toolchain. This detects nondeterminism, not a trusting-trust compiler or runner compromise. Independent builders are required for a stronger claim. |
| Malicious tapp reads host files | Every tapp launch path requires fully enforced Landlock V5, including device-ioctl control. The shim closes every ambient descriptor except explicitly connected brokers. Seccomp denies namespace creation plus kernel administration, fault injection and process-introspection surfaces; core dumps are disabled and descriptor counts bounded. Two image-granted exceptions exist. Packages the image lists in `/usr/lib/tilo/web-engine-entitled.conf` (the browser) get the `tilo-web-engine` profile, which permits user, PID and network namespaces and `chroot` for Chromium's renderer sandbox; mount, `pivot_root`, `setns` and every other namespace kind stay refused. On an image that declares itself provider-sandboxed (`/etc/tilo/provider-sandbox.toml`, root-owned), first-party apps from the root-owned system store run without Landlock, because the hosting VM is the boundary; bubblewrap, seccomp and the network namespace still apply, and user-installed apps still require full Landlock (`crates/packaging/launcher/src/provider_sandbox.rs`). | The web-engine entitlement adds the kernel surface reachable through unprivileged user namespaces to the entitled packages. ABI 6 socket/signal scoping awaits a Tilo kernel that reports the applicable Landlock errata without making enforcement status ambiguous. Landlock and seccomp are not a complete sandbox; kernel bugs, permitted-path parser exploits, permitted device ioctls and covert channels need continuing review. |
| Malicious tapp gets network access | Network is absent unless explicitly granted by package/runtime policy and enforced by the namespace/broker boundary. | Broker bugs and confused-deputy paths are in scope for fuzzing and integration tests. |
| Package/manifest parser exploit | Signed root and semantic manifest validators have coverage-guided fuzz targets with bounded hostile input; daily CI preserves regressions. | Fuzzing proves neither completeness nor memory safety in native dependencies. Corpus quality and sanitizer coverage must be monitored. |
| Stolen invite link | Ordinary Tilo invite UI requires a pasted person identity key and signs that key, role, name, room and expiry. Redemption requires a certified device for that person. Pending state stores only a BLAKE3 digest at mode 0600. | Compatibility APIs can still mint bearer invites for the Tilo Desktop/legacy tier. Treat those links as credentials and never present that path as hardened Tilo. |
| Invite replay or restart race | Admission is single-use and atomically consumed from a durable ledger; invalid/duplicate ledger data fails closed. Administrators can revoke by digest. | Authority-offline admission is not complete: a host cannot safely invent durable governance without a verifiable admission proof. |
| Collaborator self-promotes or writes outside role | Room governance, live certified identity, role/action checks and authority-pinned document merge reject local or remote capability widening. Encrypted envelopes are bound to the exact route-derived MLS channel. Signed MLS governance frames bind channel, live membership revision, governor, epoch and hash-chain head; the broker-internal handle-only authority transaction journals exact Welcome/commit recipients and applies workspace admission/revocation with idempotent crash recovery. For MLS endpoint state, production startup installs the Secret Service provider and fails closed if it is unavailable (MLS stays off); a sealed owner-only file store replaces it only when `TILO_SECRETS_DIR` is set explicitly, for headless machines. Identity signing secrets differ: they fall back automatically to an owner-only private file when the Secret Service does not answer ([identity-security.md](../identity-security.md)). | Recovery and member/device provisioning UX still require product-level ceremony and testing. |
| Accidental/hostile ownership transfer | Invite and generic role controls exclude owner. The dedicated, typed transfer protocol binds an offer to the exact workspace, current root and recipient, limits it to seven days, and makes clear that the owner's signature authorizes immediate commit when that exact recipient explicitly signs acceptance while still an Admin. The completed audit retains both party signatures. Pending proposals, signed cancellations and bounded completed/cancellation histories persist atomically with shell workspace state, so restart, replay, retargeting, role-change and accept/cancel races fail closed. Workspace Settings links to the same confirmed, resource-addressed propose/accept/cancel actions that search and headless automation invoke. | Cross-device delivery still depends on the collaboration transport's availability; an offline recipient cannot accept until the signed records reach the governing host. |
| Revoked device reconnects | Root-signed device revocation is checked when projecting peer identity and permissions. | Revocation freshness while all honest authorities are offline, and already-disclosed content, cannot be recovered cryptographically. |
| Relay reads workspace content | For a route with an installed MLS endpoint, mutation payloads are encrypted before outer signing; brokers/relays validate clear signed metadata and fan out ciphertext without keys. Complete OpenMLS endpoint state has a Secret Service-key-sealed atomic restart format. The document store fsyncs encrypted compaction snapshots before minting a route-bound receipt; the authority transaction registers the journal whose migration gate rejects plaintext after acknowledgement. | Endpoint/timing/volume metadata and traffic analysis remain visible. A route is explicitly plaintext until its acknowledged snapshot transaction flips the durable gate, and a host reads every unsealed channel in full. A node certified to own workspaces stewards the channels of the rooms it governs and holds their keys, so "without keys" holds only for a plain host certificate ([security.md](../architecture/security.md), "What a workspace host reads and originates"). iroh's default discovery and relays are number 0's public infrastructure, which sees endpoint ids and IP addresses. |
| Parser/resource exhaustion over network | Protocol, invite chain and identity wire formats have bounded inputs and fuzz targets; queues and roles are checked before application. | Sustained bandwidth/CPU/storage denial of service is only partially bounded. |
| Compromised Tilo Desktop-on-Linux host | Compatibility identity associates with the existing Unix account and creates no second credential. | The host distro, account, boot, disk, session and ambient apps are trusted. This tier is explicitly not a hardened Tilo installation. |
| Recovery abuse or loss | Recovery key is independent of TPM state; destructive rollback-floor/keyslot operations require proof another unlock path works. | Lost recovery material plus lost credential can mean permanent data loss; social/recovery UX needs dedicated tests. |
| Sleeping or unattended device is reopened | The compositor locks after five idle minutes by default, and the power service requests a lock on every pre-sleep notification. Explicit owner policy may disable idle locking but never the sleep lock. | A software test proves the decision and compositor path; real machines still need timing tests showing the lock is painted before firmware sleep and before post-resume content can scan out. |
| Authenticator seeds copied or read by another app | Each account's seed is its own item in the app's secret store (Secret Service in a desktop session); the collaborative document names accounts and never holds a seed, and there is no vault file (`apps/totp/src/secrets.rs`). | A sealed owner-only file store replaces Secret Service only where `TILO_SECRETS_DIR` is set explicitly (headless machines). Seeds stay on the devices that were given them; moving them needs an explicit export ceremony. |

## Release decision and evidence levels

Security claims use four labels:

- **Implemented**: code and negative tests exist.
- **CI-evidenced**: an isolated CI run exercised the real artifact.
- **Hardware-evidenced**: the published image passed the documented physical
  device matrix, including adverse cases.
- **Externally reviewed**: an independent report covers the exact commit and
  configuration.

A release may not describe an implemented control as hardware-evidenced. The
release record should retain image digest, source commit, both build results,
SBOM, SLSA statement, transparency inclusion, VM boot result, fuzz/security
workflow results, and (when required) signed hardware-matrix results.

## Required negative tests

Every security gate needs an adversarial case: modified UKI section, Secure Boot
off, unsigned/higher/gapped generation tag, mismatched provenance, split latest,
restored LUKS header, wrong-generation TPM prediction, partial Landlock support,
path traversal package, stolen recipient-bound invite, replayed invite,
cross-room revocation, unauthorized role grant and ownership transfer by a
delegated admin. “Could not observe” is failure, not skip.

## Known priority gaps

1. Add an independent signed freshness witness/checkpoint so a new install can
   detect a registry freeze or complete signed rollback view.
2. Run reproducibility on independently administered builders and publish both
   attestations, not just two invocations on one runner.
3. Complete MLS removal/rekey and authority-offline admission proofs.
4. Surface the implemented pending-invite list/revoke/expiry backend in settings,
   add first-class identity-card/QR exchange, and keep bearers out of storage.
5. Build a multi-machine adversarial campaign: three peers, host-only routing,
   concurrent edits/grants, restart, partition, revoke, reconnect and stale op.
6. Execute the TPM2/Secure Boot/PIN/firmware-update/rollback matrix on supported
   hardware; no software-only result closes this item.
7. Reduce the trusted computing base with broker isolation, syscall/ioctl review,
   memory-safe media/rendering paths and exploit mitigations; commission external
   review and recurring red-team work.

These gaps are release-engineering inputs.

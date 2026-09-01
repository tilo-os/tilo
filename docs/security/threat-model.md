# qOS threat model and assurance boundary

Status: **as built on this branch**, 2026-09-01. This document is the security
review entry point. It separates controls we can test in software from claims
that require release infrastructure, firmware, or physical hardware evidence.
It does not claim GrapheneOS equivalence.

## Security objective

qOS aims to keep a person's data and identity confidential, authentic and
recoverable when an app, collaborator, network, registry, or stolen powered-off
device is hostile. Ordinary use has one boot credential: a TPM-throttled PIN on
a verified qOS installation, or one high-entropy passphrase where TPM2 is
unavailable. Convenience must not silently select a weaker security mode.

The `qDesktop installed on an existing Linux distribution` path is a clearly
labelled compatibility tier. It may associate identity with the existing Unix
account and must not create another password, but it cannot inherit qOS's
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
- A malicious qpkg app has attacker-chosen native code and inputs. It may exploit
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
      -> qOS brokers and compositor
        -> Landlock + namespaces + seccomp qpkg process

person root -> device certificate -> transport key -> signed collaboration op

source + pinned inputs -> two builds -> boot gate -> candidate digest
  -> KMS signature + SBOM + SLSA + transparency log -> release channel
```

Crossing inward requires evidence; a name, UID, registry tag, relay connection,
or locally cached role is not evidence by itself.

## Abuse-case analysis and current controls

| Attack | Required control and current evidence | Residual risk / status |
|---|---|---|
| Offline disk theft and guessing | LUKS plus TPM dictionary-attack throttling for a short PIN; strong passphrase/recovery otherwise. Installer only offers the PIN after verified-boot preflight proves the current chain. | Hardware behavior and vendor lockout parameters need physical validation. DMA/cold-boot attacks against a running device remain. |
| Evil-maid ESP/initramfs/cmdline change | Owner-enrolled Secure Boot; signed UKI embeds initramfs, command line, os-release, root digest and generation. Preflight verifies every section and signature. | Firmware UI and key-enrollment correctness vary by device and need a hardware matrix. |
| Root filesystem replacement | Composefs/fs-verity root digest is sealed into the UKI command line. Release VM checks the installed image contract. | Kernel/composefs vulnerabilities remain trusted-computing-base defects. |
| OS downgrade by disk snapshot | Per-generation LUKS secrets require a TPM NV monotonic floor; confirmation advances only after health, retains a bounded recovery window, and proves another unlock route before deleting a slot. Emulator tests restore old LUKS headers adversarially. | An old image may execute before unlock refusal; this is weaker than bootloader-enforced Android rollback indexes. Physical TPM validation is outstanding. |
| Registry adds a fake `genN` | Generation resolver requires a release signature and SLSA generation match; history must be dense and signed `latest` must equal the highest generation. | Registry can freeze a *new* machine on an older complete signed view. Installed devices retain their TPM floor. An independent freshness witness is still required. |
| Release key theft | CI accepts only a non-exportable KMS/HSM URI; no file or environment private key. Candidate is digest addressed and signed after boot verification. | KMS policy, workload identity, quorum, audit alerts and disaster recovery are deployment obligations, not proven by this repository. |
| Compromised action/dependency | OS-release actions and OS/base/RPM/kernel inputs are immutable digest/commit pinned; SLSA records inputs. | A malicious pinned input is still malicious. Dependency review and builder diversity remain necessary. |
| Non-reproducible/backdoored release | A second no-cache OS build must have the same OCI manifest before publication. SBOM and SLSA attestations bind the digest. | Both invocations currently share one runner/operator/toolchain. This detects nondeterminism, not a trusting-trust compiler or runner compromise. Independent builders are required for a stronger claim. |
| Malicious qpkg reads host files | qpkg launch defaults to mandatory Landlock V4 and refuses execution on partial/unsupported enforcement; namespace/mount/seccomp policy adds defence in depth. | Landlock is path access control, not a complete sandbox. Kernel bugs, permitted-path parser exploits, inherited descriptors, ioctls and covert channels need continuing review. |
| Malicious qpkg gets network access | Network is absent unless explicitly granted by package/runtime policy and enforced by the namespace/broker boundary. | Broker bugs and confused-deputy paths are in scope for fuzzing and integration tests. |
| Package/manifest parser exploit | Signed root and semantic manifest validators have coverage-guided fuzz targets with bounded hostile input; daily CI preserves regressions. | Fuzzing proves neither completeness nor memory safety in native dependencies. Corpus quality and sanitizer coverage must be monitored. |
| Stolen invite link | Preferred invites sign the recipient person key, role, name, room and expiry. Redemption requires a certified device for that person. Pending state stores only a BLAKE3 digest at mode 0600. | The current ordinary name-only UI can still mint compatibility bearer invites until it collects/selects a recipient identity key. Treat those links as credentials. |
| Invite replay or restart race | Admission is single-use and atomically consumed from a durable ledger; invalid/duplicate ledger data fails closed. Administrators can revoke by digest. | Authority-offline admission is not complete: a host cannot safely invent durable governance without a verifiable admission proof. |
| Collaborator self-promotes or writes outside role | Room governance, live certified identity, role/action checks and authority-pinned document merge reject local or remote capability widening. | Full multi-machine reconnect/concurrency and revocation campaigns remain incomplete. MLS member removal is not end-to-end. |
| Accidental/hostile ownership transfer | Invite roles exclude owner. Dedicated transfer requires the exact structural owner and a recipient already promoted to Admin; replacement removes the old root capability. | User-visible confirmation and dual acceptance by the incoming owner are not implemented. Root-key-signed governance log and recovery require integration with shell state. |
| Revoked device reconnects | Root-signed device revocation is checked when projecting peer identity and permissions. | Revocation freshness while all honest authorities are offline, and already-disclosed content, cannot be recovered cryptographically. |
| Relay reads workspace content | End-to-end identity/channel encryption is the design boundary; relay is not an authority. | Endpoint/timing/volume metadata and traffic analysis remain visible. Verify MLS coverage per payload before claiming all collaboration content is E2E encrypted. |
| Parser/resource exhaustion over network | Protocol, invite chain and identity wire formats have bounded inputs and fuzz targets; queues and roles are checked before application. | Sustained bandwidth/CPU/storage denial of service is only partially bounded. |
| Compromised qDesktop-on-Linux host | Compatibility identity associates with the existing Unix account and creates no second credential. | The host distro, account, boot, disk, session and ambient apps are trusted. This tier is explicitly not a hardened qOS installation. |
| Recovery abuse or loss | Recovery key is independent of TPM state; destructive rollback-floor/keyslot operations require proof another unlock path works. | Lost recovery material plus lost credential can mean permanent data loss; social/recovery UX needs dedicated tests. |

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
3. Complete root-key-signed collaboration governance, incoming-owner acceptance,
   MLS removal/rekey, and authority-offline admission proofs.
4. Make recipient-bound invites the ordinary UI path and surface list/revoke/
   expiry management without ever storing the bearer.
5. Build a multi-machine adversarial campaign: three peers, host-only routing,
   concurrent edits/grants, restart, partition, revoke, reconnect and stale op.
6. Execute the TPM2/Secure Boot/PIN/firmware-update/rollback matrix on supported
   hardware; no software-only result closes this item.
7. Reduce the trusted computing base with broker isolation, syscall/ioctl review,
   memory-safe media/rendering paths and exploit mitigations; commission external
   review and recurring red-team work.

These gaps are release-engineering inputs, not optional future polish.

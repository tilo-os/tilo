# Security

Start with **[threat-model.md](threat-model.md)** for the current assurance
boundary, then [architecture.md](architecture.md). The latter describes what Tilo
defends against, what it deliberately does not, and how identity, boot, sandbox
and collaboration fit together. The remaining documents provide details for
specific areas.

## The documents

| File | What it is | Read it when |
|---|---|---|
| [threat-model.md](threat-model.md) | As-built adversaries, trust boundaries, abuse cases, evidence levels, residual risk and priority gaps. | Making or reviewing a security claim |
| [architecture.md](architecture.md) | The security concept. Threat model, identity spine, boot chain, runtime hardening, permissions, collaboration authority, attestation. Ends with what we do **not** claim, and a verified-versus-only-built split. | Always first |
| [boot-chain.md](boot-chain.md) | How the machine boots and what that buys. Owner-enrolled Secure Boot, the unified kernel image, the sealed root. | Touching `os/`, the kernel, or signing |
| [sign-in.md](sign-in.md) | One secret per person: the system unlocks itself on a verified boot, personal data unlocks with the login, and fingerprints, security keys and other devices stand in for the password. Includes development machines that sign what they boot. | Touching login, lock, disk unlock, factors or personal storage |
| [rollback.md](rollback.md) | Bounded rollback: the last few generations may unseal, older ones may not. Operator view plus the contract the boot chain must satisfy. | Changing update or unlock behaviour |
| [composefs-decision.md](composefs-decision.md) | Why Tilo is on bootc's composefs backend rather than ostree, with the evidence. Records that formal stabilisation is unlikely soon and why waiting was the worse risk. | Questioning the backend choice |
| [fuzzing.md](fuzzing.md) | Coverage-guided fuzz targets, CI cadence, and crash-handling policy for hostile package, collaboration, and identity inputs. | Adding an untrusted parser or triaging a fuzz finding |
| [prompt-injection.md](prompt-injection.md) | How content a person did not write is kept from steering an agent, a skill or a shortcut's AI step: the untrusted-content declaration, the label that follows an answer into every plan, the defaults, the negative tests and the open gaps. | Adding an operation that answers outside content, or giving an agent more reach |
| [tinkering.md](tinkering.md) | How a power user gets root-shaped freedom without ambient root: work layers and capability leases. | Working on privilege or the app sandbox |
| [defects.md](defects.md) | The as-built ledger. Every known defect with file:line, severity, and the check that catches a regression. Deliberately separate from the design. | Before claiming something works |

## Two conventions that matter

**The design describes the next version; the ledger describes this one.** If
`architecture.md` says something the code does not do, that is intentional — it
is a target. What the code actually does, and where it falls short, lives in
`defects.md`. Do not annotate the design with current-code caveats; add a ledger
entry instead.

**A check that cannot observe its subject must fail, not pass.** Five separate
verification scripts here have reported success because they could not see what
they were checking — a privileged-only listing, a log on the wrong console, a
firmware without TPM support. Unobservable and absent must produce different
output. `rollback-boot-check.sh` gets this right and is the model.

## Related, outside this directory

- `docs/architecture/security.md` — the earlier workspace-scoped threat model. Still the most detailed account of the remote-input firewall and the per-workspace sandbox; superseded on identity and boot.
- [`design-rationale.md`](design-rationale.md) — the earlier security rationale. Its §6.1 first stated the device-key ≠ account-key invariant this design is built on.
- [`spaces-channels-and-roles.md`](spaces-channels-and-roles.md) — the role/attestation model behind §7's authorship half.
- [`cooperative-redaction.md`](cooperative-redaction.md) — redaction as a cooperative protocol, and why it is not a security mechanism.
- `docs/app-model/sandbox.md`, `docs/app-model/trust.md` — the `.tapp` sandbox and package-signing detail.

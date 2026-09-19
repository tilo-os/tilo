# Framework contract review amendments

Spec: active user goal and docs/superpowers/plans/2026-09-18-cross-platform-framework/framework-contract.md.

Goal: remove ambiguities identified by independent G01 review; no runtime implementation or support certification.

## Global constraints

Preserve unrelated work. Edit only the contract and roadmap README. Do not stage, commit, run builds, or change support status to complete. Use existing authority and headless owners.

### Task 1: Tighten semantic parity, invocation authority and ownership

Read `/tmp/tilo-framework-contract-review.md` for findings and source evidence. Amend `docs/superpowers/plans/2026-09-18-cross-platform-framework/framework-contract.md` and its sibling `README.md`:

1. A package-required semantic capability requires a functional framework implementation on every claimed host even if the general matrix row says optional. Real hardware absence, denied permission or transient service unavailability remains typed. Missing backend is not device absence. Do not silently change app semantic profile per OS. Explicit target-specific administration remains distinct; genuinely optional features must be declared before certification.
2. Operations preserve the authenticated invoking collaborator's authority and delegated scope through async execution, bounded by app/workspace/instance/resource limits. Factory identity, author strings and history ActionContext are not caller authentication. Check access and publication; expiry/revocation invalidate pending work, and regrant does not revive an old operation.
3. Closure immediately revokes delivery/access while cleanup retains needed resources and exclusion leases until native quiescence is acknowledged. Cancellation races and already-committed effects need explicit subsystem semantics; cancel does not undo a completed native write/handoff.
4. Host composition roots may depend narrowly on app widgets/entry points. Reusable framework contracts/services/backends may not depend on app mechanisms. No blanket shell exemption: remaining record-resolution/process/drag ownership leaks have migration owners.
5. Link the contract and execution ledger in README read-first material; historical audit Unsupported language cannot waive current required-feature requirements. Clarify G04 graph acceptance consistently.

Keep the edit concise and cohesive, not appended disclaimers. Validate changed relative links and inspect scoped diff. Report exact edited files and review findings addressed; no tests needed for this documentation-only task. No subagents, staging or commits.

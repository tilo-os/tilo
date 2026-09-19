# G01 framework contract review

Verdict: revise before treating G01 as reviewed/closed. The ownership direction and evidence limits are sound. Two short normative additions would close material ambiguities; this does not require an API redesign or moving the existing headless services.

Reviewed `framework-contract.md`, the platform-extraction audit README, the migration roadmap README and relevant review context. Targeted source inspection covered `HostContext`, action context and semantic dispatch. This is a contract review, not implemented-support certification; no builds or runtime/native tests were run.

## 1. Required app functionality needs to override optional matrix classifications

Priority: P1. `framework-contract.md:47-49,67-73,79-85`.

The matrix makes capture, printer/location, host administration and connected-folder transactions optional on every host, allowing absence due to "host support." The later prohibition on always returning Unsupported names only media/browser/terminal. An implementer can therefore omit a capture or printing backend on a capable, authorized host while retaining the same Camera/printing app source and feature flags, and claim compliance with the O row. This is weaker than the roadmap's global rule that unsupported required functionality is unfinished work (`README.md:71-72`).

Add a universal requirement: the same app/distribution semantic requirements apply across claimed hosts; a package-required capability requires a functional framework implementation even if its matrix row is generally optional. Runtime absence of hardware, user denial or transient service unavailability remains a valid typed outcome. A missing backend cannot masquerade as device absence or be waived by silently changing the package profile for one OS. Target-specific T capabilities remain the explicit exception. State whether a genuinely optional feature such as strong connected-folder transactions is excluded from a particular app's parity promise before certifying that profile.

## 2. Instance authority must not replace the invoking collaborator's authority

Priority: P1. `framework-contract.md:89-105`.

The contract binds a client to a principal during host construction, then requires live checks and the same operation path for UI/actions/accessibility. It never says that an operation must preserve the authenticated *invocation* principal and delegated scope. A shared instance created for an owner can receive an agent/collaborator action; routing it to the same stored client and checking the owner's current permissions would satisfy the literal wording while elevating the caller.

Require service operations to carry the checked invocation authority through async work, intersected with package/workspace/instance/resource limits. Factory identity, an author string and durable `ActionContext` metadata are not caller authentication. A later invocation cannot inherit an earlier operation's grant. Recheck authority at relevant native access and result-publication boundaries, not solely when the request starts; revocation/expiry must invalidate pending work, and immediate regrant must not revive the old invocation.

Concrete source basis: `ui/src/application.rs:47-62,162-164,194-207` keeps host permissions and author/context as separate fields, returns a cloned permissions snapshot, and copies the host binding for callbacks. `crates/collab/actions/src/context.rs:1-6,17-29` explicitly describes human/durable-history metadata rather than authentication. `ui/src/bound_dispatch.rs:2856-2857,2927-2956` separately accepts authority and author for a semantic invocation. Preserve that distinction when adding scoped native clients. These anchors demonstrate why host binding alone is insufficient; they are not a claim that the future service path currently has an exploitable implementation.

## Clarifications worth including in the same edit

- At `framework-contract.md:102-104`, make lease release explicitly follow acknowledged native quiescence where that lease excludes competing writers or owns a selected resource. Closing the app revokes access/delivery immediately, but an asynchronous cleanup owner must retain the resources and necessary exclusion until the worker has actually stopped. Define cancellation races and already-committed effects in subsystem plans; cancellation cannot undo a native handoff or completed write. The existing publication/durability distinction is good.
- Link the active contract from the roadmap's "Read first" list and link its named evidence ledger. The audit remains a historical source-only document; its generic "unsupported implementations fail explicitly" language must not override the newer required-feature contract. The current explicit statements that no matrix entries are certified correctly avoid promising shipped native support. Exact OS/architecture qualification can remain in the ledger, but should become enumerated rather than left at "as applicable" before any certification.
- Clarify the graph boundary in `framework-contract.md:32-36` and roadmap G04 acceptance (`README.md:199`). A host composition root can depend on app entry points/widgets while assembling a desktop; reusable framework contracts/services/backends cannot depend on app-owned mechanisms. `execution.md:32` records shell's direct app dependencies and distinguishes legitimate widget composition from record resolution, process services and drag state requiring extraction. A literal ban on every shell-to-app edge would reject legitimate composition; a blanket shell exemption would hide the service leakage. Require narrow composition-edge classification and finding/removal ownership for remaining app-owned mechanisms.

## Boundaries that already read correctly

- Applications retain one source and semantic feature vocabulary; framework composition owns native selection.
- Headless service crates may remain in place, and core contracts do not acquire renderer/app dependencies.
- Private evidence, owned native handles, explicit trusted-host versus confined policy, and no arbitrary privileged exec are appropriate constraints.
- Native cancellation acknowledgment, no UI blocking, shared action delivery, compatibility preservation and final native/package evidence are explicitly required.
- The document does not certify HostContext production preparation or chooser cancellation. Production preparation, live authority ownership and actual native cancellation are still prerequisites identified by `/tmp/tilo-platform-scoped-services-design.md`; a client accessor or mock test alone cannot close G02.

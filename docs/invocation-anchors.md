# Original invocation anchors

The shared SDK can retain a bounded semantic target from an admitted invocation's
actual encoded action. This is an observation of checked dispatch, not a pointer
event, a new authority grant, a broker success receipt, or presentation evidence.

Apps register pure `RegisteredInvocationAnchor` projectors through
`PublishesResources::invocation_anchor_projectors()`. A projector receives the
actual record loan, exact resolved action address and encoded action. An optional
`with_applied` projector receives the actual locally applied envelope and resulting
committed record. Registration does not execute actions or change participant views.
The SDK rejects ambiguous providers and does not cross another document owner or
accept a forwarded capability before checking its concrete owner boundary.

`ResourceInvocationScope::pre_operation_anchor()` returns the original basis and
pre-operation target only while original custody remains live. The SDK attempts
capture once, after participant-aware encoding and before execution. It never
repairs missing evidence from a later global caret, document frontier or action.

`ResourceInvocationScope::applied_anchor()` returns evidence only after the whole
checked local mutation completes at the captured committed frontier. Failed,
no-op, remote-submit and incomplete paths remain unavailable. Its five-second
monotonic retention is checked on access; retirement cannot renew it. The SDK's
terminal handoff flag means the original terminal command was queued after the
document commands. It does not assert durable owner acceptance or broker success.

Writer projects supported body text insertion and same-block replacement from its
actual participant-aware `RichTextAction`. It checks the real block, Unicode
code-point offsets and record content. Unsupported region/action/encoding cases
return no target. A post-insertion collapsed caret is semantic action geometry;
it is not physical mouse motion and supplies no invented path between targets.

Limits include a 64 KiB encoded action, 8 KiB retained anchor, 256 visited providers
and depth 32, in addition to the existing protocol item and identifier limits.
No observer schedules an action, changes focus/selection, or commits a document.

Correctness coverage uses synthetic admitted scopes through actual SDK dispatch
and real Writer projection. It does not establish model execution, 80 concurrent
agents, frame rate, or captured cursor pixels. A capture consumer must separately
join exact original broker outcome, durable state, owner/frontier and presentation
provenance. See [panorama capture](panorama-capture.md) for the distinct browser
mount allowlist and same-frame capture contract.

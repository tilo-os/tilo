# Certified local execution custody

Person action callers can carry a host-minted execution lease. Two executions
under the same person receive distinct participant views; their actor, device,
room and permissions remain unchanged. Client labels and context do not create
this identity. The broker replaces invoker metadata from its actual Caller.

Cloned callers share retirement. HTTP DELETE, detected idle expiry, endpoint
drop and stdio server drop close the original lease; reconnecting to a live
HTTP session preserves it. An expired session cannot renew itself. Retirement
refuses new queries/invocations and retained observation/cancellation access,
including a request that retained its server before DELETE. Legacy callers
without an execution preserve their existing behavior. Idle expiry remains
lazy at registry operations; there is no new per-session timer.

Original worker invocation custody retains the same lease. Ending a session
therefore invalidates a retained SDK scope without cancelling or rolling back
an already admitted application operation. It does not change an operation's
recorded result to Cancelled. Observation-ended publication is a separate
integration boundary; a consumer must never treat a closed lease as live work.

## Host-local certified door

`AppActionBroker::certified_local_action_host` binds an exact transport-owned
Caller to an already-issued, signed room presentation. The existing verifier
checks the actual principal, root/device keys, room, current membership and
registered policy clock. Missing keys or clock, a different execution, revoked
admission, or a closed run refuses the door. It creates no device, membership,
grant or permission. A name or UUID cannot construct admitted authority.

The door supports numeric peer providers and application/retained-launch aliases.
Aliases retain the original signed admission across asynchronous launch and
recheck it before numeric delivery. Retained launch aliases pin the original
record; application aliases remain app-scoped. Discovery and launch resolution
do not create a second operation or replace the original retry key. A director
requiring an exact app instance must still use its discovered numeric peer.

Standalone's MCP session factory binds each logical HTTP, native or stdio
session to its existing selected-room certificate, original policy clock and
host-minted execution. An unavailable or wrong-audience factory refuses the
session without falling back to a keyless caller. Ordinary workspace and
builtin doors keep their existing routing under that same authenticated caller;
application and retained-launch aliases always use the certified door.

One bounded registration table observes the existing native owner's fences.
Identity/enrollment loss closes the original run lease synchronously. Admission
registers before rereading its original material, so a concurrent fence cannot
refresh an old lease. Presented certificates never select the trusted baseline
used to retire admitted sessions. The table holds at most 512 runs and refuses
overflow; dropping its registrations or runtime closes retained leases. Ambient
filesystem changes are reread when custody is checked, not continuously watched.

Eager and directory actions preserve original signed admission and policy time
through dispatch and retained SDK custody. Any additional foreign-device
restriction on the caller remains restrictive even when the certificate is
native; later cap tightening rechecks the original exact device and current
governing exemption. A signed certificate is never rewritten to carry that bit.

Scoped app deliveries use the original external invocation UUID, matching the
receipt and broker activity. The numeric wire job ID remains separate. Scoped
calls require the existing stableInvocationIds subscription; old subscribers
are refused rather than receiving an ID they cannot acknowledge. Calls without
an execution retain the original numeric-derived UUID behavior. Retrying the
same invocation is not a new operation or a replayed activity event.

## Validation and limits

Focused tests exercise actual signed caller/private-worker custody, eager and
directory dispatch, forged metadata, clock expiry, stricter foreign caps,
retirement during directory wait, same-person participant isolation, stable
UUID delivery/retry, HTTP lifecycle and a real Writer text/image fixture. The
Writer fixture uses synthetic data and a real content-store PNG; it is not a
Photos, MCP, model, frame-rate or captured-cursor proof.

The local activity stream remains observation, not portable authentication.
This path does not publish a pointer, generate motion, register browser
replicas, install a production receiver, or associate a successful presentation.
See [the SDK basis](invocation-locus-basis.md) and
[panorama capture](panorama-capture.md) for the separate target/frame gates.

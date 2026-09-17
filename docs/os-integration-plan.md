# Tilo OS-integration architecture record

**Record type:** Historical planning rationale. Current delivery state is established by the checkout, tests, and release evidence rather than this document.

## Foundation

The unit of synchronization is a personal workspace. An always-on host may pin
its CRDT and provide relay/store-and-forward behavior; direct peer replication is
the same model without a dedicated host. Home-server and enterprise deployments
change where that host runs, not the identity or synchronization semantics.

Device roles, replication intent, and multi-server placement build on the same
collaboration operations, filesystem blobs, and consented remote input. App
serving and compute placement must not create a parallel sync or identity stack.

## Integration principles

- Verify behavior on the real path before treating a subsystem as integrated.
- Enforce capability permissions at both operation application and filesystem
  boundaries; UI affordances are never the only enforcement point.
- Make actions typed, exportable, replayable, and permission-aware so keyboard,
  automation, and visual interaction invoke the same semantics.
- Keep apps thin over shared runtime services while preserving sandbox and
  package-boundary guarantees.
- Prefer local rendering from collaborative state for native peer collaboration;
  surface streaming is a compatibility mechanism, not the semantic model for
  Tilo-native apps.
- Treat desktop polish features as user-facing subsystems with explicit evidence,
  including accessibility, notifications, input, scaling, lock, audio, and power.

## Evidence standard

Every integration claim requires a focused test or reproducible scenario that
exercises the relevant boundary: multi-device convergence and offline delivery;
real permission denial; action enumeration and invocation without rendering;
package installation and launch; peer co-editing with locally rendered state; or
the applicable desktop-session behavior. Branch names, rough completion estimates,
commit hashes, and binary measurements are historical implementation notes and do
not establish present status.

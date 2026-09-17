# Private browser boundary after automatic runtime attachment

## Integration findings

The shared runtime startup change introduced two paths that the earlier private-window tests did not cover:

1. **Child startup failure:** the private launcher retained the parent's runtime descriptor environment. The identity SDK closes its descriptor during child exec, so automatic attachment rejected the stale descriptor before the private live handler ran.
2. **Persistent activity:** an ordinary private launch attached to the normal workspace runtime. The browser disabled its own Autosave, history and session recovery, but navigation Effects still went through the connected broker. Its durable receipts include the page resource label, which can contain the private page title.

## Fix

- Browser uses the same private-mode decision for startup attachment and storage. Both `--private` and the existing `QOS_BROWSER_PRIVATE=1` fallback disable automatic persistent runtime attachment.
- Private live windows construct `App::from_scoped_parts_local`. It preserves the real live wake scope and local action dispatch, starts with a disabled collaboration manager, creates temporary participant/surface identities, and suppresses ambient continuation, presence and floating-surface service discovery.
- The private root still has memory-only state and disabled Autosave. The generic local constructor does not grant storage provenance or disable explicitly installed local document History; callers own their storage policy.
- New private processes clear the parent's two runtime descriptor variables and internal resume marker. Profile and runtime binary selection remain intact. Shared SDK descriptor adoption/provenance corrections belong to the runtime implementation.
- Native accessibility resource dispatch uses the App's local author, so private View actions retain their temporary identity.

## Regression evidence

Logs are in `target/browser-complete-ux/`:

- `task10-cycle82-private-red.log`: child fixture received stale descriptor values `998` and `999`. `private-green.log` and `private-final.log` (same prefix) pass after cleanup; the final fixture also checks resume-marker removal and preservation of profile/binary selection.
- `task10-cycle82-local-red.log`: the ordinary constructor wrote five bytes to an isolated identity socket and delivered an ambient continuation to the private root. Local Effect execution and exact wake ownership were exercised before the failing privacy assertions.
- `task10-cycle82-local-green.log`: the new constructor passes the initial socket/continuation/local-action/wake checks.
- `task10-cycle82-local-ax-red.log`: restoring the old global accessibility actor prevents the private View action from executing (one operation instead of two).
- `task10-cycle82-local-final.log`: final regression passes, including actual accessibility View activation, local Effect execution, live wake ownership, temporary identity/surface ownership, and zero service contact.

After the fixes, `task10-cycle82-browser-final.log` records **234 passed, 3 ignored** across all browser targets, and dependency-inclusive browser Clippy exits0 in `task10-cycle82-clippy-final.log` (14.86s). The shared semantic/accessibility group passes all45 tests in `task10-cycle82-semantic.log` (6.76s).

The socket fixture uses synthetic data and an isolated subprocess with explicit test storage roots. It proves absence of ambient service I/O; it does not simulate successful remote broker admission or claim a native GUI acceptance result.

## Independent source review

The scoped reviewer found no remaining actionable issue in the private mode predicate, child cleanup, local construction, effect routing, wake ownership or accessibility author changes. It traced disabled collaboration through action publication, continuation publication, document traffic and effect receipts: none can reach the persistent runtime through that backend. Review was read-only and performed no GUI or Keychain operations.

Final real-window private isolation/reopen acceptance remains open alongside the separate Chromium Keychain startup stall. The complete ten-feature UX goal is not certified complete.

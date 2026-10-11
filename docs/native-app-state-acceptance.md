# Native app-state acceptance

The native app-state composition is qualified only by a successful
`native-app-state-proof` receipt with `app_state_qualified: true`, no error and
successful cleanup, plus inspection of both `*-shared-state.png` captures.
Transport-only and isolated SDK tests do not qualify this composition.

## Disposable two-process journey

The governed fixture dispatches the existing isolated app-worker mode before
starting its runtime, as the production relay does. App code still runs under
the worker confinement and untrusted-result admission boundary.

This uses the existing `native_pair_host` governed fixture and ordinary signed
thin Chat package. Two headless compositors, separate certified people, separate
native app processes and their device-local stores share the same admitted Chat
record. Alice creates a channel and commits a message before Bob opens Chat.
Both processes then submit edits concurrently. The ordinary semantic actions
must return each expected message exactly once, from the correct channel, on
both devices, attributed to each exact certified invoker. The receipt retains
the bounded semantic message results from each device. A shared resource reference must preserve its target and access.
Each compositor must present a new local app frame after navigating to that
channel. Screenshots supplement the semantic assertions; frame activity alone
cannot establish readable pixels.

No audio server is needed. Context following, media capture and remote input
are not invoked. The existing OS font catalog is installed read-only in a
fixture-owned mount namespace, matching the packaged-native proof. The source
receipt hashes the fixture, compositor, governor, launcher, native Chat binary
and signed package. Synthetic identities and invitation material remain inside
the private temporary profile and are removed, with owned processes, on exit.

Run only in an owned disposable remote builder; keep the ordinary packager and
process lifetime under the selected cargo drop-in’s session lock. Set `CARGO`
to any cargo drop-in that supports the remote execution and receipt retrieval
options below:

```sh
"$CARGO" --no-fallback --no-fetch build --locked \
  -p shell --bin shell --example native_pair_host \
  -p tilo-control --bin tilo-control -p launcher --bin tilo-launcher \
  -p tilo-app-package-cli --bin tilo-app-package -p tilo-app-cli --bin tilo-app \
  -p install --bin tilo-install -p runtime-bundle --bin tilo-runtime-bundle
"$CARGO" --no-fallback --on-builder \
  --fetch native-app-state-proof/journey.json \
  --fetch native-app-state-proof/alice-shared-state.png \
  --fetch native-app-state-proof/bob-shared-state.png \
  run --locked -p shell --example native_pair_host -- \
  --journey --state-proof --out target/native-app-state-proof
```

The output directory must be new. Retrieve only the bounded receipt and
screenshots, including on failure; never retrieve `bootstrap.json`, identity
roots or publisher keys. A failed step keeps `app_state_qualified: false`.
The combined voice/follow journey retains its separate `qualified` field.
Neither field qualifies same-person automatic app opening, offline recovery,
physical display/input behavior or installed package delivery.

## Installed-device handoff to the Pi owner

Use one exact source revision for each architecture's signed shell, launcher,
SDK/runtime and Chat package; record the installed hashes and signed manifests.
Keep actual device repair, packaging and session changes with their existing
owners. Do not copy raw identity roots or replace a running session to execute
this recipe.

1. Identify an existing explicitly approved governed workspace, both currently
   admitted certified device identities, the exact canonical Chat record and
   the current grants. Same identity alone supplies no workspace or input grant.
   If the required room or app authority is absent, stop for exact user approval
   naming the devices, room, resources, role and persistence. The outstanding
   live Files workspace grant is independent and must remain held.
2. Query each owner's normal semantic endpoint with
   `tilo-control --socket <approved-owner-socket> actions list Chat --json --programmatic`.
   Resolve operations by application, room, record, operation and logical
   resource identity. Bound component handles differ between devices and must
   be discovered independently; never send one device's alias to the other.
3. Open Chat normally on device A. In the approved acceptance conversation,
   invoke `new-channel`, then `post` with a unique non-sensitive marker; wait
   for terminal success. Only then open Chat on B, against the same approved
   record. `read-messages` must return that exact channel and marker once on B.
4. Invoke `post` from each device concurrently with distinct markers. Both
   `read-messages` results must contain all three complete messages exactly
   once, with correct channel and history attribution. Invoke `open-channel`
   through each device's own current handle and inspect each local native view.
   An Open receipt, transported payload or tab placeholder is insufficient.
5. With the Pi owner coordinating only their owned app lifetime, close/reopen
   the receiving app and check the same canonical record and retained history.
   For a separately approved temporary offline interval, make distinct edits,
   reconnect normally and require both edits without duplication or reset.
   These outcomes remain pending until observed; this synthetic mode does not
   claim to exercise them.

Use `actions invoke <provider> <id> --args-stdin --wait` with the arguments
returned by that operation's schema, then inspect its terminal result.
Do not use pointer injection, grant changes, durable-file eviction or media
streaming as part of this app-state recipe. Physical screenshots of a user's
actual desktop are outside this source fixture's authorization.

## State boundary

The admitted Chat resource's document, channel/message identifiers, content,
resource references and attributed history are semantic shared state. Package
installation, credentials, runtime endpoints, caches, rendering, focus and
component publication handles are device-local. The fixture navigates each
view independently and does not claim that every app's selection or cursor
state automatically roams, or that an app lacking replication is complete.

## Same-person native continuity regressions

The same fixture has two additional, isolated modes. Both enroll a second
throwaway device through the production joined-device installer. It receives
its own device key and a root-signed member certificate, a public identity
record and the existing fleet metadata; it receives no human root secret or
device-admission capability. The installed certificate paths stay intact.
Each device still explicitly consumes its own signed room invitation. Matching
person identities do not replace room admission or grant remote input.

Use the same component build and owned remote execution above, with a new
output directory and either `--automatic-opening-proof` or `--reconnect-proof`
instead of `--state-proof`. Retrieve the bounded `journey.json` and screenshots
from that directory. Automatic opening never invokes Applications Open on the
second device: it requires a presented local primary Chat frame after the first
device opens Chat. Reconnect explicitly opens both apps, first proves concurrent
native state and resource-reference convergence, then pauses only this fixture's
exact governor process. Both apps must successfully edit the existing channel
while that process is observably stopped. After resuming it, both must read both
messages exactly once, with their certified author and original channel.
The governor is always resumed before cleanup; no revocation or new grant is
issued during this partition. This tests governor unavailability, rather than
physical network isolation of every peer or host worker.

The first real same-person run exposed a production enrollment defect:
`provision_joined_device` wrote the default device certificate but omitted the
identity-specific certificate required by join authorization. Its production
persistence regression now asserts that both paths contain the same verified
certificate, retaining the no-root and no-admission assertions. With that fix,
both native devices joined and both explicitly opened apps converged on the
same channels, concurrent messages, authors and resource-reference access, and
presented fresh local frames.

Automatic opening remains **unqualified** on the same final source
(`same-person-automatic-final-source`): the second device's real observation
has no frames and no admitted window groups. The first device's topology
namespace requests report `the governor is unavailable: Offline`; the governed
host does not serve the existing desktop namespace command. The original
reconnect run also failed: both offline edits completed, but the first
post-resume `read-messages` invocation never reached a terminal result.

With the governance and replay repairs below, the fresh remote
`same-person-reconnect-final-source` journey completes both original
post-resume reads, observes both offline messages exactly once in the original
channel with their certified authors, and cleans up successfully. Its receipt
sets `app_state_qualified`, `same_person_devices_qualified` and
`offline_reconnect_qualified` true, with no error. The exact fixture SHA-256 is
`c622f4971303a6cded8bf38e2720bd62bcdd95148559c8c14c62f98a518c876b`;
the receipt also records actual component and package hashes. The separate full
voice/follow `qualified` field remains false because this mode does not run it.

The fixture never opens the second app manually to rescue automatic opening or
treats a pending action as completed recovery. Screen arrangement, cross-device
input, physical-device continuity and complete physical network isolation are
not qualified by these fixture modes.

### Bounded failure diagnostics

Add `--routing-diagnostics` to the same isolated journey to enable the existing
relay phase tracing and native session logs. The throwaway governor installs a
tracing subscriber only in that opt-in mode, after dispatching its confined
worker modes. No production tracing configuration changes.

When an ordinary semantic action times out, `failed-<operation>.json` retains
its public route, original broker job and invocation UUID, and one read-only
status observation of that same job. It exports neither action arguments nor
results. The original failure is rethrown: there is no re-invocation, longer
wait, alternative record, or qualification based on `Accepted`.

The reconnect diagnostic reproduced job 14 remaining `Accepted` through the
terminal timeout. Chat's subsequent SDK log identified the owner barrier:
`record_receipt_elsewhere` received the governor administration answer, then
failed waiting for that exact signed effect receipt in the device's admitted
room. The SDK reported `signed effect receipt did not reach the native owner
in time; outcome unknown` and disabled collaboration. This was the pre-repair failure; the source and complete reconnect proof below
repair admission and redundant replay reconciliation while retaining the
fail-closed owner barrier. Retrieve only the named public failure JSON,
`journey.json`, and synthetic `alice.log`, `bob.log`, and `host.log`; private
profiles, bootstrap invitations and package signing material remain excluded.

### Governance admission during native replay

The native actor admits authenticated `SyncResp` governance through the existing
`Input::admit` and durable owner before forwarding the same input to the
compositor. It awaits one blocking admission at a time. Current session, selected
custody, membership, revocation and pin checks remain in that production path;
a transport acknowledgement cannot substitute for the signed owner receipt.
This lets receipt admission progress while app replay remains queued for the UI.
The compositor retains its ordinary ordered event stream and wakes after an
actual owner publication. Its event drain yields after one native admission per
UI turn, leaving subsequent events in the bounded FIFO; the existing eight-event
budget remains for inexpensive transport events. Both paths schedule outgoing
recovery only when the actual committed owner generation changed. An unchanged
exact replay must not enqueue another recovery reconciliation; the regression
requires a current pin and actual broker fanout before checking that command
queue. No action, control-socket or
signed-receipt timeout is extended. A regression admits a real signed document
through the production tick, then checks the retained queue and later event
order across successive turns.

An unchanged authenticated replay reuses the committed owner publication only
when the encoded peer state and all outgoing custody are exactly unchanged.
For a recorded exact document, the existing `StatePeer` lookup verifies current
authority, publisher, route, signature and operation identity before copying
mutable peer state. The owner also checks retirement, recovery reservation and
checkpoint obligations before reusing its committed generation; a missing
certificate or governance still follows the ordinary deferred-admission path.
New prerequisites, receipt state and retired outgoing obligations still cross
the durable writer. The replay regression checks the actual generation and
state-file bytes; the relay regression publishes a fresh signed checkpoint and
requires its exact receipts in the actual owner with no compositor event drain.
They exercise `tilo-joined-room` and the real pinned enrollment exchange rather
than a synthetic receipt or helper projection.

The complete reconnect receipt above qualifies this private governor-unavailable
journey on the repaired source. Source regressions alone do not qualify an app
journey, automatic opening or physical-device UX; each requires its own complete
receipt, including ordinary terminal success and cleanup.

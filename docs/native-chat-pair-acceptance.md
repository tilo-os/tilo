# Native Chat pair acceptance

The combined two-native-desktop journey is **not qualified**. Browser/native
resource acceptance and low-level native Opus tests do not qualify native/native
Chat voice, following, Stop, Leave or revocation.

## Reproducer

`scripts/headless-examples/native-pair-journey.py` drives two independent native
headless compositors with different certified people, ordinary signed thin Chat
packages, one private certified governor and separate PipeWire graphs. Capture
consent uses the visible prompt's actual one-shot control. Only generated 700 Hz
and 1100 Hz sources and null outputs are allowed; unexpected audio devices fail.
No hardware microphone, video capture or external account is used. Throwaway
identities, invitations, publisher pins and processes are removed on exit.
Forked private D-Bus helpers are stopped through pidfds only after matching both
this run's exact HOME and runtime directory. `--cleanup-proof` runs a separate,
short real-daemon check: the owned bus exits and an adjacent private bus survives.
Its receipt remains `qualified: false`, with `mode: cleanup-proof`, because it
proves cleanup rather than the native pair journey.

Build and run in an owned disposable builder environment. Set `CARGO` to
`cargo` or any Cargo drop-in configured for that environment:

```sh
"$CARGO" build --locked -p shell -p tilo-control --bin shell --bin tilo-control
"$CARGO" build --locked -p launcher --bin tilo-launcher \
  -p tilo-app-package-cli --bin tilo-app-package \
  -p tilo-app-cli --bin tilo-app -p install --bin tilo-install \
  -p runtime-bundle --bin tilo-runtime-bundle
"$CARGO" run --locked \
  -p shell --example native_pair_host -- \
  --journey --out target/native-pair-proof --pipewire /private/pipewire-payload
```

The disposable environment needs PipeWire utilities, WirePlumber, GStreamer
`opusenc`/`opusdec`, and a matching server payload under `--pipewire`: `usr/bin/pipewire`,
`usr/share/pipewire` and `usr/lib64/gstreamer-1.0/libgstpipewire.so`. The fixture
starts its own socket and session manager per peer; it never connects to the
operator's audio server. SDK package assembly is sequential and uses four jobs.
The output directory must be new. When running remotely, retrieve only
`journey.json`, screenshots and the bounded `*-capture-observation.json`
from the owned task directory. Never transfer `bootstrap.json` or private identity directories into evidence.
Each throwaway profile uses the existing sealed headless secret store at its
own private `TILO_SECRETS_DIR`. MLS remains mandatory; there is no plaintext
fallback when a headless environment has no Secret Service session.

## Observed production boundaries

Initial runs on 2026-10-06, based on `5fc0df4139`, admitted both independent native
peers normally and verified both private generated-source graphs, but did not
present Chat. The Applications Open invocation reports success when queued;
the fixture separately requires a presented primary foreign frame, so this cannot
be mistaken for an opened app.

1. With ordinary empty room policy, app launch preflight times out waiting for
   governor-authorized app authorship. Native membership execution refuses an
   externally governed room, and the relay alone runs no app-serving plane.
2. Delegating Chat authorship alone got as far as the ordinary signed Chat process,
   with Landlock fully enforced. The broker then refused bootstrap with
   `application creator lacks exact current governor signing custody`. An app
   authorship grant does not delegate ownership of the canonical record.

The private governing host now serves the compiled Chat application, grants its
authorship through the existing typed membership command and prepares its primary
record through the trusted application factory with actual governor custody.
Participants receive ordinary invitations and never receive the governor's key.
Terminal audit forwarding uses an authenticated, bounded room command: exact
current admission, workspace read permission and the caller's own person identity
are checked before the governor commits and publishes. The native broker also
waits to observe that exact receipt in admitted signed state.

The repaired journey has presented both ordinary signed Chat applications and
converged a newly created channel over the encrypted collaboration path. Resource
comparison uses the logical resource identity while invocation retains each
desktop's own bound component handle. Shell follow uses the certified live
device-session handle, rather than the person's identity key. Neither correction
changes production access checks. The combined journey is still unqualified.

The live roster exposed another boundary: native membership does not populate
the legacy identity-hello cache. Native app presence now checks the exact room's
current admitted read authority and the authenticated publisher's session handle
instead. The relay stamps co-edit presence attribution from the authenticated
device connection before forwarding; a caller-supplied author is never used as
native authentication evidence.

On candidate `b3ff06c7b6`, Alice's actual tab arrival reaches Bob after this
repair, but the roster still classifies her as invited: its arrival set counts
desktop pointers and legacy document participation, not room-scoped app
lifecycle presence. The governed host also has no handler for the existing
`ClaimNamespace` desktop command; its namespace request reports `Offline`,
and Alice's tab is absent from Bob's admitted topology. Both boundaries must
be completed before shell follow can qualify. No roster flag or missing-tab
assertion is bypassed by this reproducer.

The follow-up routes namespace claims through the authenticated room service
using the exact signed governor hint and current device certificate. The governor
derives ownership from that certificate and records the immutable namespace
under its own custody; it grants no administration. Zero salts, conflicting
owners, stale admission, viewer writes and exhausted budgets are refused.
Publication waits for the admitted registry entry, not its RPC acknowledgement.
The compositor retains tab arrivals by room and authenticated session, preserves
the source app for current read checks, and removes the location on Left, room
unsubscribe or loss of admission. Names never identify a location.

The next private run confirmed both governed namespace claims and Alice's
certified tab arrival in Bob's roster, but Follow timed out before voice. The
relay had been ignoring authenticated workspace `SyncResp` updates. The
follow-up now admits those updates through `StatePeer`, restricts them to
owned contents without changes to governance, and commits and publishes them
through the governor's durable checkpoint path. Cross-room and cross-namespace
writes, governance edits and replay after downgrade or removal are tested.
The reproducer also waits for the remote tab in the admitted workspace and
observes the Follow action's terminal receipt. This run remains unqualified.

`--voice-proof` runs the same certified desktops, normal packages and private
audio graphs through voice, Leave, fresh rejoin consent and removal without
running context follow. Its receipt is explicitly `mode: native-voice-proof`
and remains `qualified: false`, even when all voice steps succeed. It provides
independent evidence and cannot qualify the combined pair.

The independent voice run reaches the same `c_lounge` call on both desktops
with `muted: true` and `capture_active: false`. Alice then receives the visible
one-shot microphone prompt and chooses Allow once. The prompt clears, but her
native session ends with `permission_denied` before active capture is observed.
The bounded capture observation and failure still are retained separately from
private profile material. Opposite-frequency audio, Leave, fresh rejoin consent
and removal have not executed in that run. Subsequent diagnostic runs
present both apps but can terminate before capture with an ended, empty call
report and no issue. `call-status` explicitly returns its preceding snapshot
while requesting an asynchronous refresh; the reproducer waits for the first
service reply but refuses a later terminal state. No capture fence diagnosis
has been established by those later runs.

A source-level race exists when an owner fence pauses an in-flight capture
startup: the old prepared generation reaches installation after the pause and
was classified as permission denial. The follow-up retires that prepared work
and requests reconnect through the existing lease and current-authority checks.
Terminal stop, expired consent, source loss and denied publication still refuse
capture. This correction does not establish the cause of the earlier synthetic
receipt until a new instrumented journey exercises it.

The governed topology run then reached the admitted remote tab and a successful
terminal Follow receipt, but Follow released before navigation completed. A
production regression now covers admission preceding the live widget rebuild:
Follow pins a tab only after focus is actually installed; an absent admitted
target still releases it. The independent synthetic voice run with the startup
correction reached both live muted calls and one-shot consent, then expired the
capture startup deadline while consent and the source remained current. That
receipt does not prove a live permission denial or successful capture. Bounded
installation and owned-stream observations distinguish its remaining boundary.
A generated-only encoder preflight checks the private graph and codec before
native FD custody; it never qualifies a native call. Follow also installs the
complete strip/group/tab focus address, including when the admitted target is
on another strip.

The next combined run completed actual opt-in Follow, Stop sharing and Stop
following, then connected both native calls listening muted. Generated-only
encoder probes passed by node name and by a freshly observed connected socket,
both before and after playback opened. Native playback now emits bounded silent
frames while listening so an idle live output does not stall the private graph.
Capture avoids an asynchronous preroll wait, waits for the original presentation
clock, and uses a stable local monotonic clock instead of the device clock's
startup jump. Complete fresh encoded frames receive a monotonic 48 kHz wire
frame timestamp; device PTS still determines their unchanged 100 ms send lifetime.
A real-encoder jitter regression fails with the preceding mapping and passes
with the correction. Future-presentation and receive-expiry regressions cover
the clock and lifetime boundaries.

Run 38 on 2026-10-07 observed Alice's actual call live, unmuted and actively
capturing after visible one-shot consent. Bob's next unmute produced no prompt
within the bounded wait, so the combined receipt is still unqualified and its
cleanup passed. Opposite-frequency decoding, Leave, fresh rejoin consent and
removal did not execute. Startup expiry is classified as capture failure while
current consent and source authority remain valid; genuine denial still reports
permission denial. A call-state observation before consent now distinguishes
an ended call from a missing prompt. Physical Yoga/Pi testing remains outstanding.

Run 40 recorded Bob's call ended with an empty channel and no issue before
consent, while Alice again reached active capture. Neither output-observation
loss nor registry-refresh failure was reported. Chat's explicit status read
could overlap its automatic poll: the shared transport cancels the earlier
status task, and this fail-closed client turns that cancellation into Leave.
The follow-up coalesces those reads onto one in-flight refresh in Chat, keeping
actual service errors terminal. Public call-effect and failure regressions pass;
the next native run must establish whether this closes the observed boundary.
Run 39 was interrupted during package assembly and produced no acceptance receipt.

The voice-only run 42 observed Alice actively capturing and the first genuine
encoded packet accepted by the carrier in about 0.2 ms. Bob still ended before
consent with no reported issue. Query coalescing therefore does not qualify or
establish the cause of this boundary. Run 41 separately ended with `source_lost`
after its output observation became non-current. These receipts retain their failed
status and completed cleanup. Chat's `call-status` now retains a fixed
`clientIssue` category after automatic cleanup, clears it on a new explicit
command and never exports private transport error text. The service's own
`reported` snapshot remains separate. A public-effect regression covers cleanup,
fresh Join and error redaction. Runs 43 and 44 retained `command_refused`
after cleanup; run 44 identified the local service's `call_not_ready` response
to Bob's unmute. Both runs observed Alice actively capturing and a promptly
accepted carrier packet; neither qualified bidirectional audio.

A further production regression covers unmute during a controlled reconnect.
The service returns the current non-live snapshot instead of a terminal RPC
error, queues no consent and remembers no future capture request. Recovery stays
muted; a new explicit unmute still requires fresh consent. An expired lifecycle
still refuses the command. The generated fixture requests unmute only while the
observed call is live and retries no permission refusal. The next native run must
validate this boundary; the complete voice journey remains outstanding.

The sustained-source preflight exposed a fixture failure independent of Chat:
the GStreamer provider stopped after 34 encoded frames. The fixture now uses
PipeWire's `pw-cat` with a private generated PCM source, no automatic output
link, and a 64-buffer preflight through both ordinary and fresh-FD paths. The
source never reads hardware. Recording requires successful bounded EOS and
exactly 96,000 samples. Tone energy is measured in native 20 ms frames, retaining
the original threshold; coherent energy across an entire recording incorrectly
rejected legitimate decoder phase correction. Correct-tone, wrong-tone and
noise controls distinguish this measurement boundary.

Production playback previously reset its sample clock to each delayed poll,
reducing playback rate when polling did not divide 20 ms. It now preserves the
20 ms schedule across small delays and resets after a missed whole frame,
without replaying outage backlog or extending frame deadlines. A real GStreamer
AppSrc/AppSink regression fails on the preceding path and passes on the fix.
Receive admission also compares asynchronous arrivals against a fresh local
clock observation, retaining the carrier's original arrival and expiry.

Private native runs 58 and 59 decoded both opposite-frequency tones after two
separate visible one-shot consents. Run 59 measured 99.917% and 99.929% frame
energy, observed both actual capture/playback streams on each desktop, and
proved Alice's Leave removed both streams. Bob then retained only playback
while Chat's preceding snapshot still reported active capture; the next stream
precondition timed out. This is an unresolved peer-leave recovery boundary,
not successful Leave/rejoin/removal acceptance. Cleanup passed. Runs 60 and 61
ended before capture with carrier retry exhaustion; rebuilding the host example
alone did not rebuild the launched shell, so newly added teardown diagnostics
were absent. Subsequent diagnosis must build the shell and host together. Run 62 did so
and observed complete carrier retirement transitions, then an observation
with no advertised default output. The follow-up preserves an already bound sink
only while the same daemon cookie, node and serial remain observed; a new
default, missing sink, reused node or changed daemon still retires it.

Run 64 rebuilt the working signed-app path with the exact-output correction
and reproduced bidirectional audio (99.558% and 99.575% tone energy), Alice's
Leave, and Bob's missing capture precondition. Its finite diagnostics show the
remaining carrier retired and capture pause completed. Source inspection found
a recovery cycle: the backend kept treating the retired startup as reconnecting,
while the service waited for Live before resuming the existing capture lease.
The follow-up acknowledges an idle generation when pausing. It starts no device,
sets no readiness, renews no lease and retains terminal/expiry fences; the
service alone restarts under the original current consent, lifecycle and source.
All 26 focused production media/fence regressions pass. Run 67 rebuilt the
actual shell and completed the independent voice journey: both peers decoded
the remote generated tone before and after rejoin (99.458%–99.893% tone energy),
both Leave operations removed capture and playback, both rejoined the original
call muted with inactive capture, fresh visible one-shot consent reopened each
microphone, and governor removal closed Bob's streams. Cleanup passed. Its
`mode: native-voice-proof` receipt has no error and remains `qualified: false`
because Follow and the combined pair were not exercised. Runs 65 and 66 stopped
before desktop launch because the direct builder command lacked the toolchain
PATH or shared-cache wrapper; the successful run used the configured remote Cargo runner.

Run 68 added explicit semantic Mute: Bob's microphone stream disappeared while
his playback stream remained, with no permission request. Unmute then required
a fresh visible one-shot consent and restored real remote audio. Both Leave
operations, muted rejoin, fresh consent, bidirectional decoding and governor
removal passed again. Five complete 96,000-sample recordings contained
98.156%–99.871% expected-tone energy. Cleanup passed; the independent receipt
still cannot qualify Follow, the combined pair or physical devices.

## Everyday Chat acceptance gaps

The combined harness now also checks ordinary native posting into two different
conversations, bounded readable history on both admitted peers, and convergence
of a typed live resource address with its existing record provider. Sharing an
address grants no access. Run 62 passed these assertions through the real signed native applications.
It also passed semantic opt-in context navigation and both Stop controls, then timed out joining
voice after an observation advertised no default output and retired the bound
sink. The exact cause of that observation remains unconfirmed. The combined receipt remains unqualified. A retained headless screenshot then exposed a missing Follow presentation
assertion: shell focus landed on an unavailable peer-window record while the
other local Chat reported the followed conversation. The harness now requires
the exact admitted target tab to paint an actual signed local app frame.
A minimal routing experiment used the existing distinction between
governor-committed documents and stewarded sealed channels. Run 63 reached
the native SDK history-confirmation guard, where Bob stopped before presenting
a frame (`native component history was not confirmed before its factory`).
The unqualified routing experiment was reverted. Resolving the record/history
handshake needs coordination with the SDK owner; no history confirmation or
current admission check was bypassed. Run 69 repeated both native conversation/read and resource-reference checks, then failed the strengthened assertion: the exact followed tab had no actual signed local frame. Its bounded presentation receipt records `actual_local_frame_presented: false`; cleanup passed. Follow presentation remains undelivered.

| Journey | Current evidence / remaining gap |
| --- | --- |
| Correct conversation and send | Runs 62/69: both native messages read in their intended conversations |
| Retry and reconnect | Idempotent action tests exist; ordinary native outage/retry journey outstanding |
| Presence and Follow | Certified roster and semantic navigation/Stop observed; exact followed frame was unavailable, run 69 failed the strengthened exact-frame presentation check |
| Readable history | Runs 62/69: bounded native reads passed; history/catch-up UI remains with its separate owner |
| Live resource access | Landed browser/native journey; runs 62/69 native reference/provider convergence passed |
| Voice, mute and Leave | Runs 67/68: bidirectional native audio, explicit Mute/fresh Unmute consent, both Leave operations, muted rejoin and removal passed independently |
| Physical devices | Yoga/Pi acceptance outstanding; no physical microphone or public access enabled |

`journey.json` remains `qualified: false` until every step completes: presented
native Chat on both peers; opt-in context navigation; Stop and explicit follow
release; muted joining; visible fresh capture consent; decoded opposite-frequency
audio on both outputs; Leave removing audio streams; fresh rejoin consent; and
host removal closing the removed peer's streams. Follow, voice, Stop, Leave,
rejoin and revocation remain acceptance requirements until a complete successful
receipt exists. Physical Yoga/Pi journeys also remain unqualified.

The two existing lower-level native audio proofs now fail when the Opus encoder
is unavailable instead of printing SKIP and passing. They still use test media
identity/authority and must not be described as governed native Chat acceptance.

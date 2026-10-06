# Continuity and protocol interoperability

Status: 2026-10-05. This inventory distinguishes code present in the repository
from interoperability observed on physical devices. A simulator passing is not
a claim that every receiver or phone works.

## Existing journeys and owners

| Journey | Existing production path | Boundary and next verification |
|---|---|---|
| Send a nearby file | Files `nearby_send` → shell `broker_services::nearby_transfer` → resumable content transport | Same-identity certified peer; session/workspace-bound opaque source and destination handles; receiver consent. Exercise interruption, destination collision, grant revocation and retry using two isolated sessions. |
| Copy here, paste there | `device_fabric` clipboard history and clipboard portal | Shareable entries only; secret MIME/fields and primary selection excluded; 256 KiB and 50-item limits. Prove a secret copy never enters the replicated snapshot and disabling continuity withdraws sharing. |
| Resume work | `personal_continuity`, app continuation schemas, relay continuation store | Root-derived private same-identity lane; logical resource positions and safe launch intents, no credentials or local paths. Prove sleeping-device catch-up preserves local focus and resolves the resource through the existing app action. |
| Play on another Tilo device | `device_fabric` media handoff and live audio routing | Target-side consent, certified capabilities, scoped routes, teardown on policy/peer/focus changes. Music owns Spotify playback/device controls; this layer owns generic outputs and routing. |
| Project to a receiver | shell `projection` → `tilo-projector` | Existing AirPlay, Cast, FCast and Miracast adapters. Capture remains compositor/portal-owned. Receiver discovery grants no Tilo peer identity, workspace access, clipboard access or remote input. |

The UI crate split should consume these existing broker surfaces; it should not
move pairing, replication or helper-process ownership into widgets. This work
changes the projection broker, the existing Nearby receive-decision path and
Files' semantic decline contract. Music's active Spotify implementation remains
the owner of Spotify sessions and playback.

## First slice: private AirPlay authentication failure and retry

The broker already hands the explicitly entered PIN/password to the Doubletake
sender through `DOUBLETAKE_CODE`, not argv. Sender output can subsequently become
a user-visible session error. Both stdout and stderr now redact exact occurrences
of that submitted value before queueing diagnostics. The sender environment
removes the broker's `TILO_PROJECTION_CREDENTIAL` and any ambient `DOUBLETAKE_CODE`
before setting the one supplied for this attempt. Diagnostic retention on exit
uses the same eight-line tail as the running session.

The regression journey starts a real child process using the production runner:
a rejected credential emits diagnostics on both streams, fails without a ready
marker, and reports useful errors with the credential redacted. A fresh explicit
code retries, publishes readiness, then disconnects and clears session markers.
This verifies the broker boundary, not cryptographic receiver authentication.
Exact-value redaction does not claim to remove transformed/encoded secrets or
credentials independently loaded by the helper from its persistent store.

No relay/fabric enum, ALPN, JSON command or resource/action schema changes. Older
peers keep their existing negotiated protocols. No new trust or device grant is
created. The existing image-encrypted AirPlay test remains the cryptographic
transport check; physical receiver compatibility needs a separately authorized
lab run.

## Interrupted receive: decline after restart

Files exposes the `decline` action on a recovered incoming transfer in
`AwaitingDestination`, including when no writable destination is available. The
action is now declared destructive, so semantic callers confirm deletion of
retained partial bytes through the normal action system. The runtime now resolves
that decision against the durable inbox instead of requiring the vanished
in-memory offer. It removes only the selected
checkpoint and metadata, records cancellation, and uses the existing exact-peer,
request-and-transfer-bound cancellation message. Notification is best-effort:
a sender being offline cannot block deletion of unwanted local bytes.

The broker still checks continuity policy and the authenticated Files connection,
workspace and receiving-direction handle. No destination or new pairing grant is
needed to decline; accepting and resuming continue to require an approved
writable destination. A later sender re-offer requires a fresh receiver decision.
Fixture coverage exercises the actual durable store across reopen, offline
notification, unrelated-transfer preservation, and Files' published semantic
action. This is not a claim of a live two-device transport test.

## Fresh-consent retry of an interrupted receive

A durable sender retry/restart creates a new offer request ID. Previously,
accepting that offer hit the store's exact-request resume check and could not
reuse the old checkpoint. The accepted-offer path now permits a new request ID
only for the same certified source and unchanged transfer ID, name, kind, size
and full content hash. It truncates uncheckpointed bytes, durably records the new
request binding and returns the committed prefix/hash. Ordinary resume remains
exact-request bound. Receiver frame admission continues to reject old request
IDs. Declining a fresh matching offer also removes the retained checkpoint;
another peer or a changed manifest cannot use decline to erase it.

The test journey reopens sender/receiver stores, uses a fresh consent/request
binding and approved destination, then streams the remaining bytes over two real
Iroh endpoints. Full-hash installation preserves an existing same-name file and
uses the immutable sender spool even after the original source is edited. Store
tests separately cover uncheckpointed tail truncation, persisted rebinding,
stale-request rejection, changed peer/manifest rejection and decline cleanup.
This is a production transport/store fixture, not a full headless Files or device
enrollment ceremony and not a physical-device interoperability claim.

## Compatibility candidates

| Protocol | Primary implementation or documentation | Integration decision |
|---|---|---|
| AirDrop | [OpenDrop](https://github.com/seemoo-lab/opendrop), GPL-3.0; [OWL](https://github.com/seemoo-lab/owl) supplies Linux AWDL | Valuable experimental file adapter. OpenDrop documents missing peer verification and automatic receive acceptance. Do not launch its stock receive command as Tilo's receiver. First implement a receiver-consent transaction and peer-verification design, bounded archive staging, and capability-bound installation. Start with the receiver's explicitly enabled Everyone mode; do not extract Apple ID credentials or bypass Contacts Only. AWDL hardware/driver support is a separate gate. |
| AirPlay | [Doubletake](https://github.com/omarroth/doubletake), existing pinned image helper; [UxPlay](https://github.com/FDH2/UxPlay), GPL-3.0 receiver | Continue existing encrypted sender, PIN/password and stored-pairing support. Receiver mode is a separate scoped feature: opt-in advertisement, on-screen PIN, local capture/playback consent and explicit stop. UxPlay offers PIN and client restrictions; audit those options before embedding. Preserve upstream license notices and the existing pinned asset hashes. |
| Google Cast | [Google sender/receiver model](https://developers.google.com/cast/docs/overview); [PyChromecast](https://github.com/home-assistant-libs/pychromecast), MIT reference | Existing `fcast-sender-sdk` and vendored Cast protocol own projection. Use the reference for protocol/receiver behavior research rather than adding a second controller. Next test selected receiver binding, transport authentication behavior, disconnect and URL serving. Never equate LAN discovery or a self-signed TLS connection with certified Tilo identity. Protected media remains the licensed application's responsibility. |
| Android Auto | [OpenAuto](https://github.com/f1xpl/openauto), GPL-3.0 head-unit emulator over aasdk | Exploratory bench-only adapter. Separate phone projection from Tilo-to-car casting. Audit USB/Wi-Fi transport, codec licensing, consent and input limits before packaging; no auto-start microphone/input forwarding or car changes in this slice. |
| CarPlay | [Apple's CarPlay programme](https://developer.apple.com/carplay/); [node-CarPlay](https://github.com/rhysmorgan134/node-CarPlay), MIT USB-adapter driver | Exploratory: app entitlements and vehicle integration are distinct surfaces. Apple directs vehicle-system integration through MFi. node-CarPlay demonstrates H.264/PCM and input via particular Carlinkit adapters; this is an adapter driver, not proof of a standalone authenticated receiver. Audit the adapter, USB isolation, microphone/input consent and license provenance on an authorized bench. Do not copy authentication keys or defeat accessory authentication; no purchase, implementation or certification claim yet. |

These external links describe upstream projects, not endorsement or a decision
to ship them. Pin and audit a particular revision before importing code; retain
license notices and source obligations for any distributed component.

## Ordered follow-up slices

### Bounded acceptance checklist

This is a capability assessment, not a declaration that continuity is complete.
The implementation column names existing production code; the validation column
separates automated fixtures from physical-device observations.

| Journey | Already implemented | This branch | Remaining implementation | Validation still required |
|---|---|---|---|---|
| Nearby transfer between certified Tilo devices | Files resources/actions and opaque connection/workspace-bound handles; approved source/destination descriptors; explicit receive consent; immutable file/folder staging; chunk transport, checkpoint hashes, collision-safe install, cancel/retry and policy teardown | Confirmed local decline of recovered partials; fresh-consent retry rebinding for an unchanged exact-source object | No further missing general-transfer code established in this assessment; additional gaps must come from the remaining journey checks. | Two-Iroh-endpoint resumed-byte fixture plus store/broker tests; physical paired-device disconnect/reconnect, grant revocation and large-file/folder journeys remain unobserved. |
| Shared clipboard | Same-identity fabric snapshots, bounded shared history, sensitive-content exclusion, imported provenance and policy withdrawal; explicit history activation rather than remote writes replacing the live selection | Existing privacy/history tests exercised; no new clipboard engine | No established missing code for the current history-sharing contract. Apple Universal Clipboard interoperability is a separate, unimplemented adapter. | Existing runtime policy/provenance tests are fixtures. A live certified two-session copy → history activation → paste journey still needs end-to-end validation; two Wayland clients on one compositor do not prove this. |
| Resume work | Private continuity checkpoints, structural/generic app contracts and app-specific metadata; continuation suggestion/accept/dismiss; resource-position launch and preserved local focus | Existing semantic suggestion tests exercised; no new app resume implementation | No established missing general transport code. App-specific semantic fidelity must be assessed against each app's declared contract; Music owns playback state. | Semantic tests prove offer/accept/dismiss, not sleeping-device catch-up. Live paired-session resource resolution, restart catch-up and position fidelity remain to validate. |
| AirPlay/Cast projection | Compositor-owned capture, receiver discovery/selection, existing encrypted AirPlay helper and PIN/password input, Cast/FCast control and disconnect | Explicit credential isolation/redaction with reject/retry/ready/disconnect subprocess regression | No second Cast/Spotify controller. Opt-in AirPlay receiver mode would be new code, outside the existing sender journey. | FCast socket and discovery fixtures exist; AirPlay helper regression uses a substitute child process. Assembled-image encrypted interoperability and physical AirPlay/Cast receivers remain unverified here. |
| AirDrop interoperability | No AirDrop adapter, AWDL lifecycle or AirDrop action surface found | Primary-source candidate/license/security roadmap only | Substantial missing code: package/pin the audited adapter, AWDL discovery/session lifecycle, selected external peer transaction, bounded transfer staging through Files consent and sandbox, cancellation/errors, and identity-verification design. Stock automatic receive is not an acceptable integration. | Adapter simulation must precede an authorized AWDL-capable hardware lab. This is not blocked only by physical testing; the adapter and security integration still need implementation. |

Acceptance for the retry slice: immutable peer/name/kind/size/hash must match;
fresh consent and an approved destination precede request rebinding; old request
frames remain stale; only checkpointed bytes survive; receiver installation
verifies the full hash and preserves an existing same-name file. No protocol,
pairing or persistent access-grant changes are required.

1. Nearby-send journey: two certified isolated peers, explicit receive destination,
   interruption/resume, destination collision, revoked grant and source teardown.
   Reuse Files actions and opaque handles; adapters cannot accept arbitrary paths.
2. Clipboard and resume: prove sensitive payload exclusion, policy disable,
   resource-position fidelity, sleeping-device catch-up and local focus retention.
3. Casting: authenticate/select a test receiver, capture only the user-selected
   source, retry wrong PIN/password, disconnect all workers, and check playback
   on an authorized receiver. Extend encrypted simulator coverage before hardware.
4. AirDrop lab adapter: AWDL feasibility, verification and consent gates first;
   bounded one-file transfer through the same Files broker second. An external
   peer never receives certified-device clipboard, resume or workspace authority.
5. Vehicle protocols: capability inventory and isolated bench capture/input tests
   before any device integration, persistent grant or vehicle deployment.

For every adapter expose discovered resources, connect/send, cancel/stop and
inspectable failure through the existing semantic control surface. Negotiate a
separate versioned adapter protocol when needed; never append variants to a
stable postcard relay enum. Discovery, pairing, capture consent and filesystem
consent are separate decisions with explicit revocation and expiry.

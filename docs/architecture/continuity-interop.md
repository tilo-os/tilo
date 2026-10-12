# Continuity and protocol interoperability

Status: 2026-10-08. This inventory distinguishes code present in the repository
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
changes the projection broker, the existing Nearby receive-decision path,
Files' semantic decline contract and an explicitly gated AirDrop lab adapter. Music's active Spotify implementation remains
the owner of Spotify sessions and playback.

## First slice: private AirPlay authentication failure and retry

The shell sends the explicitly entered PIN/password inside the connect request
on the projector's control socket, never in an environment variable or argument,
and both ends wipe it from memory once used. The broker hands it to the
Doubletake sender through `DOUBLETAKE_CODE`, the only interface that sender has;
that one child process is the only place it is an environment variable. Sender
output can subsequently become a user-visible session error. Both stdout and
stderr now redact exact occurrences of that submitted value before queueing
diagnostics. The sender environment removes any ambient
`TILO_PROJECTION_CREDENTIAL` and `DOUBLETAKE_CODE` before setting the one
supplied for this attempt. Diagnostic retention on exit
uses the same eight-line tail as the running session.

The regression journey starts a real child process using the production runner:
a rejected credential emits diagnostics on both streams, fails without
reporting the session connected, and reports useful errors with the credential
redacted. A fresh explicit code retries, reports the session connected, then
stops when the service asks it to.
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
| Shared clipboard | Same-identity fabric snapshots, bounded shared history, sensitive-content exclusion, imported provenance and policy withdrawal; explicit history activation rather than remote writes replacing the live selection | Real-Iroh root-certified session fixture: runtime export/import, explicit activation, secret/primary exclusion, no imported re-export, and observed wrong-root frame rejection; no new clipboard engine | No established missing code for the current history-sharing contract. Apple Universal Clipboard interoperability is a separate, unimplemented adapter. | The paired fixture drives the compositor selection store and fabric pump, not two running Wayland clients. Physical copy → semantic activation → application paste and policy withdrawal remain unobserved. |
| Resume work | Private continuity checkpoints, structural/generic app contracts and app-specific metadata; continuation suggestion/accept/dismiss; resource-position launch and preserved local focus | Real-Iroh same-root request/checkpoint fixture with exact Browser metadata, preserved local focus, semantic suggestion acceptance and real launcher consumption; no new app resume implementation | No established missing general transport code. App-specific semantic fidelity must be assessed against each app's declared contract; Music owns playback state. | The paired fixture verifies late-session catch-up, semantic acceptance and launcher staging. Two live compositor/app processes, durable restart and app-specific restored position remain to validate. |
| AirPlay/Cast projection | Compositor-owned capture, receiver discovery/selection, existing encrypted AirPlay helper and PIN/password input, Cast/FCast control and disconnect | Explicit credential isolation/redaction with reject/retry/ready/disconnect subprocess regression; required same-connection Cast DeviceAuth before control or media | No second Cast/Spotify controller. Opt-in AirPlay receiver mode would be new code, outside the existing sender journey. | FCast socket and discovery fixtures exist; AirPlay helper regression uses a substitute child process. The production Cast rejection journey proves the authentication gate; synthetic private-authority fixtures qualify successful verification. Manufacturer-authenticated hardware, selected compositor capture and decoded display output remain unqualified. The cached image encrypted AirPlay run failed the optional codec prerequisite. |
| AirDrop interoperability | No release-enabled AirDrop feature or AWDL lifecycle | Debug-only Files action through its authenticated broker lane; immutable sealed staging, independently entered certificate pin on a compositor-owned consent sheet, scoped status/cancel and supervised helper; real TLS broker fixtures and pinned OpenDrop production receiver wire oracle | AWDL discovery/interface lifecycle; physical receiver verification method; per-chunk progress; secure optional receiving. Release availability remains false. | Nine mandatory OpenDrop wire/staging tests; native broker trust/revocation/rotation/cancellation and Files semantic-action fixtures. Six composed live compositor/signed Files/pinned OpenDrop journeys now observe consent, exact bytes, wrong pin, cancellation, decline/retry, certificate rotation and fresh-consent recovery. Physical Apple/AWDL interoperability remains unobserved; no Contacts Only credentials or network changes are attempted. |

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

### Receiver and source prerequisites for the next acceptance run

The existing test endpoints are disposable loopback receivers, not identified
physical devices. `cast-session-proof.py` binds `127.0.0.1` on an ephemeral port
and supplies a DeviceAuth error to the production projector. Run its default
mode to witness challenge-first rejection, no readiness/control/media URL, and
clean disconnect. The legacy transport mode cannot qualify the current
manufacturer-authenticated sender. A successful Cast run needs an explicitly
selected receiver whose certificate proof and current signed revocation data
satisfy the production verifier; synthetic test authorities are not a production
receiver option.

`airdrop-transfer-proof.py` starts the pinned OpenDrop wire receiver on loopback
and drives signed Files through the native headless compositor. Its exact-byte,
wrong-pin, cancellation, decline/retry and certificate-rotation witnesses remain
separate from physical Apple/AWDL acceptance. It requires matching prebuilt Files,
runtime and host binaries plus the pinned external receiver dependencies; it
must not configure network interfaces or use an unattended receiver.

The image contains `/usr/libexec/tilo-doubletake-test-receiver`, used by
`os/tests/airplay-encrypted-session.sh` with synthetic sender input and an explicit
fixture PIN. Its bounded phases are codec round trip, pairing/setup, encrypted
media and teardown. The complete optional codec candidate still has no qualified
image-trusted signature: sign-only run 73209 failed at STS credential acquisition
before KMS public-key retrieval. No key mismatch was established. Signing,
publication and device activation remain held; neither an unsigned candidate nor
an independently upstream-authenticated loader diagnostic closes this gate.

A selected-source journey additionally requires an attended, unlocked source
session, its existing workspace and app publication, a functioning renderer, and
explicit compositor-owned capture selection. Verify the semantic action is
published and inspect the selected resource before connecting. Observe receiver
output and teardown separately; transport counters alone cannot prove pixels.
Coordinate device-specific workspace and renderer prerequisites with their
owners before using a physical endpoint. No authorized physical receiver is
recorded in this assessment, so device discovery or an old address cannot
substitute for that prerequisite. This acceptance work changes no enrollment,
persistent access grant, display configuration or playback controller.

### AirDrop wire adapter and lab prerequisites

`tools/protocols/airdrop_sender.py` is an embedded internal transport component.
The Files action is unavailable in release builds. A debug session must explicitly
set `TILO_AIRDROP_LAB=1`, have `/usr/bin/python3`, enable continuity file sharing,
and have an unlocked, attended local person before the broker advertises it.
The lab uses a manually selected literal IP and port; it performs no discovery,
AWDL setup, interface changes or Apple credential extraction. It performs `/Discover`, `/Ask`, and chunked `/Upload`
with a gzip/newc CPIO containing one regular basename, capped at 16 MiB. The
source is an already approved read-only descriptor; it never opens a host path.
The staged bytes are immutable for the attempt. The sender uses a literal
selected IP endpoint, TLS 1.2 or newer, and the exact leaf-certificate SHA-256
pin before every HTTP request connection, including reconnects. An inspect
operation only observes a certificate: it sends no HTTP data and grants no
trust. The broker must obtain independent approval/verification of that pin;
blindly feeding inspection output to send is not pairing. AirDrop trust never
becomes a Tilo root/device certificate or authority for clipboard/workspaces.

Receiver `/Ask` refusal stops before upload. Response bodies are capped at
64 KiB, socket inactivity at 15 seconds, and the standalone attempt at 60 seconds.
No source names, remote strings, content, credentials or paths are printed in
errors. No receiver process, discovery service, Apple identity records or
persistent trust store ships with this component. The broker imposes a 65-second
helper deadline, kills and reaps only its owned child on cancellation, clears the
child environment and marks every inherited descriptor close-on-exec before
passing its scoped descriptors. One active worker and 32 short-lived status
records bound resource use. A disconnect cannot release the worker permit before
the child finishes. Hydration/staging occurs in the supervised child so a blocked
remote filesystem read cannot stall the compositor. The resulting Linux memfd is
sealed against writes, growth and shrinkage and reopened read-only. The broker
independently checks its length and hash before presenting consent.

Files publishes `send-airdrop` on current regular-file resources only after the
authenticated host advertises lab availability. Its only caller parameters are
`address` and `port`; source selection stays in the app-owned action payload.
The broker derives the local person from admitted identity, opens the source
under current Files grants, and binds opaque transfer handles to the connection
and channel. No application-supplied certificate, person or persistent-trust
claim is accepted. Existing Nearby JSON tags remain unchanged; these commands
are additive local JSON messages, not peer postcard or Capnp schema changes.

The compositor uses the shared portal text input and semantic actions. It shows
the exact source basename, byte length and endpoint and asks for a SHA-256 leaf
fingerprint obtained from an independently trusted receiver console/channel.
The field accepts exactly 64 hexadecimal digits (the SHA-256 of the DER leaf
certificate). The observed inspection pin is neither displayed as proof nor
prefilled. Wrong
input remains on the sheet with an accessible validation error and sends no
HTTP request. Send once consumes the decision; receiver refusal stops before
upload, and certificate rotation fails before HTTP. A new attempt needs fresh
verification. No generic Retry action reuses AirDrop consent.

The sheet is withdrawn on decline, disconnect, cancellation, screen lock,
privacy withdrawal, source-grant revocation or source inode replacement. Policy,
presence and source identity are rechecked while staging, awaiting consent and
sending. Cancellation cannot retract bytes already transmitted. Status uses
existing Files transfer projections; byte progress is zero until completion,
not an invented per-chunk percentage. Receiving and persistent pairing are
outside this slice. No trust becomes a clipboard/workspace capability.

The compatibility fixture imports the unmodified OpenDrop production handler at
`11fe7ba7861093b302bc0637e8cb10adf2d29337` from a separate temporary GPL-3.0
checkout. This repository copies none of its implementation. It extracts only our
bounded safe test archive into an isolated temporary directory; OpenDrop's stock
auto-accepting receiver is never a proposed desktop service. The fixture must
fail if the checkout/dependencies are missing or the revision is changed, rather
than silently skip interoperability. Reproduction:

```sh
python3 -m venv /tmp/tilo-opendrop-venv
/tmp/tilo-opendrop-venv/bin/pip install libarchive-c==5.3 zeroconf==0.148.0 ifaddr==0.2.0 Pillow==11.3.0
git clone https://github.com/seemoo-lab/opendrop /tmp/tilo-opendrop-fixture
git -C /tmp/tilo-opendrop-fixture checkout 11fe7ba7861093b302bc0637e8cb10adf2d29337
/tmp/tilo-opendrop-venv/bin/python tools/ci/test_airdrop_sender.py --opendrop-checkout /tmp/tilo-opendrop-fixture
```

Prerequisites are Linux with memfd sealing, Python 3, OpenSSL for ephemeral
fixture certificates, system libarchive, and permission to bind loopback sockets. No AWDL is involved in this
fixture. The physical lab additionally needs an explicitly authorized test
receiver in Everyone mode, an independently verifiable peer-certificate method,
a Linux adapter/driver supporting monitor mode and injected frames, approved
OWL/AWDL interface lifecycle with IPv6 and mDNS, legal channels 6/44/149, and a
plan for Wi-Fi interruption. OWL documents exclusive adapter use and unsuitable
ordinary virtualized Wi-Fi. Interface creation, channel changes, BLE wake-up or
phone settings require separate approval; no commands doing those are included
in this work. Contacts Only/Apple credential extraction is outside this slice.

Ownership remains continuity transport, pairing and Files capability integration;
Music retains Spotify playback and UI owns its toolkit split. These fixtures add
no UI framework refactor, casting controller, persistent grant or deployed service.


### Native journey evidence and reproduction

The composed AirDrop proof runs the actual headless compositor, provisioning its
supported throwaway certified identity and bootstrapping a new workspace. A live
owner grants read access only to its private fixture folder. The actual installer
verifies the signed thin Files package against a matching signed runtime bundle;
no package-size, signature, bubblewrap or Landlock checks are disabled. Files must
present a real frame and publish its public folder/send/cancel/retry actions.
The independently derived receiver fingerprint is typed into the compositor's
focused shared control through canonical `tilo-control` input operations.

`scripts/headless-examples/airdrop-transfer-proof.py` observes six journeys:
exact extracted bytes after consent, wrong-pin metadata exclusion, cancelled
consent with no HTTP requests, public cancellation while `/Ask` is held (no
`/Upload`), receiver decline/generic retry without reused consent, changed
certificate rejection before metadata, and a deliberate fresh-consent recovery.
It fails on missing prerequisites or observations. Certificates, runtime signing
identity, application registry, profile, grants, receiver and owned process groups
are removed on completion. JSON evidence includes artifact hashes and a real
presented Files frame; it contains no credentials. Files publishes a read-only `status` action on every visible transfer, including
terminal transfers. It returns the broker-issued phase, total and confirmed byte
counts with no paths, filesystem authority or network work; expired rows fail
closed. The native proof reads awaiting consent, sending, cancellation/failure and
completed byte counts through that action. AirDrop confirms bytes at completion;
per-chunk byte progress remains unimplemented.

Build these artifacts with the pinned toolchain before running. Set `CARGO` to
your authorized Cargo drop-in for remote builds:

```bash
$CARGO build --locked -p shell -p file-explorer -p launcher -p install -p app-cli \
  --bin shell --bin file-explorer --bin tilo-launcher --bin tilo-install --bin app \
  --example serve-test-registry
$CARGO build --locked --profile thin-app --features file-explorer/thin \
  --config 'build.rustc-workspace-wrapper="/ABSOLUTE/CHECKOUT/tools/ci/rustc-thin-app-wrapper.sh"' \
  --config 'env.TILO_THIN_BIN_CRATES="file_explorer"' \
  -p runtime-dylib -p file-explorer --bin file-explorer
```

Run on the remote builder using its matching `libstd` and supported OS
`tilo-runtime-bundle` binary, and the disposable OpenDrop Python environment
specified above. A remote source snapshot may have no `.git`; record the full source
commit before syncing, passing it explicitly. Artifact hashes distinguish rebuilt
candidates from that source base.

```bash
python scripts/headless-examples/airdrop-transfer-proof.py \
  --source-commit FULL_SOURCE_COMMIT --opendrop /path/to/pinned-opendrop \
  --runtime-bundle /path/to/tilo-runtime-bundle \
  --libstd /path/to/exact-toolchain/libstd-HASH.so \
  --evidence target/airdrop-native-proof.json
```

### Cast lifetime defect and authentication boundary

The native loopback Cast journey found that `connect_cast_device` returned a device
whose SDK runtime owner had already been dropped. TLS connected and the projector
reported ready, but its aborted worker never transmitted media `LOAD`. The helper
now returns and retains `CastContext` until session teardown. The existing FCast
wire regression uses this production helper, rather than constructing a separate
SDK context in the test, so the runtime-lifetime defect is observable in CI.
Music continues to own Spotify playback; no new playback controller is introduced.

`scripts/headless-examples/cast-session-proof.py` runs the prebuilt production
projector service (`tilo-projector serve` in an isolated runtime directory, driven
by its `connect`/`disconnect` socket clients) against an independent bounded
loopback receiver. Its default mode
observes the DeviceAuth challenge, supplies an authentication error, and requires
disconnect without a `Connected` event, control messages or a media URL. The explicit
`--legacy-transport-only` mode retains the earlier LOAD/HLS/STOP witness for a
baseline binary. Neither mode establishes decoded display output or physical
receiver interoperability; the historical transport success was unauthenticated.

```bash
$CARGO build --locked -p tilo-projector --bin tilo-projector
python3 scripts/headless-examples/cast-session-proof.py \
  --projector target/debug/tilo-projector --evidence target/cast-session-proof.json
```

#### DeviceAuth implementation and compatibility limits

The checksum-verified SDK patch verifies TLS 1.2/1.3 handshake signatures against
the presented leaf with Rustls/ring. The self-signed transport certificate policy
is followed by a required DeviceAuth exchange on that exact connection before
CONNECT, status, launch or media messages. Each attempt owns a fresh unpredictable
16-byte OS-random nonce and consumes its pending state once. The proof covers the
nonce followed by the exact TLS leaf DER. Cancellation and command-channel closure
win even when a valid reply is simultaneously available. TLS negotiation and
authentication each have a five-second deadline.

Public receiver anchors (Cast/Eureka) and the separate signed-revocation anchor
(Cast CRL) are private constants in this verifier. They do not enter OS, browser,
user or Tilo collaborator trust stores and create no pairing records, accounts,
ACLs, capture consent or persistent grants. Manufacturer identity establishes
neither ownership nor capture authorization. HLS encryption is not claimed.

The anchors and retained schemas come from Google's Open Screen repository at
revision `1c526caf732881bd0aa1ab9479c36861bb225ba6`, fetched through ordinary
certificate-validated HTTPS and matched to its trust-store includes. DER hashes
below pin the exact certificate bytes, embedded as UTF-8/LF Rust byte arrays in
`chromecast_auth/authorities.rs` without binary-source exceptions. This records repository provenance; it
is not independent verification of a Google-signed trust bundle or Git commit
signature. `LICENSE.OpenScreen` retains the full BSD-3-Clause license.

| Protocol-private purpose | Certificate DER SHA-256 |
| --- | --- |
| [Cast Root CA](https://chromium.googlesource.com/openscreen/+/1c526caf732881bd0aa1ab9479c36861bb225ba6/cast/common/certificate/cast_root_ca_cert_der-inc.h) | `809af14700b3fe2611ad597eb1584d6354313b64ccdb3390f097fa3e5826d6ca` |
| [Eureka Root CA](https://chromium.googlesource.com/openscreen/+/1c526caf732881bd0aa1ab9479c36861bb225ba6/cast/common/certificate/eureka_root_ca_der-inc.h) | `caf6d1e37b532203e01a76bf07187bb731ccd38801565ab2211a2c0ab7f3bc46` |
| [Cast CRL Root CA](https://chromium.googlesource.com/openscreen/+/1c526caf732881bd0aa1ab9479c36861bb225ba6/cast/common/certificate/cast_crl_root_ca_cert_der-inc.h) | `8b77c60291f9d62764613083f49750b1c5caac2d9efbe5a28edf301f9d84831d` |

Tilo deliberately requires more than the pinned Open Screen compatibility
defaults: fresh matching nonce, SHA-256 proof, and current signed revocation are
mandatory. RSASSA-PKCS1-v1_5 is required (including its wire default); proof hash
must explicitly select SHA-256. Verified certificate links use SHA-2 RSA with
2048–8192-bit keys; SHA-1 links fail closed. Rustls/WebPKI enforces the certificate
path, with additional key-size, validity, usage and critical-extension checks.
Leaf digitalSignature and issuer keyCertSign/CA constraints are required;
clientAuth EKU is required if EKU is present. All verified certificates, including
anchors, must be current. The TLS leaf must be current with at most four days
remaining. Screen projection rejects audio-only policy OID
`1.3.6.1.4.1.11129.2.5.2` anywhere in the actual verified path, including its anchor.

Exactly one authenticated version-0 revocation list is accepted. Missing,
malformed, stale, future, invalidly signed, ambiguous or revoking bundles fail
closed; no network fetch or persistent/cache trust shortcut exists. The proof
frame is limited to 64 KiB, certificates to 16 KiB and intermediates to eight.
The authenticated connection expires at the earliest TLS leaf, identity path,
CRL signer path or CRL expiry. Monotonic and wall-clock checks guard every outgoing
frame and wake an idle controller; session faults trigger existing resource
cleanup. Reconnect requires fresh authentication.

These limits can reject legacy and uncertified software receivers. No silent
fallback, arbitrary environment trust loader, TOFU, second Cast controller or
Spotify controller is provided. A future explicitly pinned/PIN-authorized
software-receiver mode needs a distinct trust label. Music owns audio casting
and Spotify playback; this consumer requires screen capability.

The isolated tests include the exact production verifier source and exercise
synthetic root/intermediate/device chains, valid connection-bound proof,
wrong issuers/signatures, TLS-leaf substitution, nonce replay, all certificate
validity boundaries, path/usage constraints, revocation and capability failures,
bounded malformed frames, timeout, disconnect and cancellation races. Independent
wire fixtures admit a synthetic authenticated receiver only through private test
authorities and verify production anchors reject it before control. These tests
install no production authority or Tilo peer permission. The production rejection
journey is separate evidence. Successful manufacturer-authenticated hardware,
selected compositor capture and decoded physical output remain unqualified.

### Current immutable image AirPlay codec gate

The current qualified x86 image from successful build `72677` is
`registry.tilo.so/tilo/tilo-ci:main-12f4dfb-x86_64`, digest
`sha256:7a733a3f1a9fc79ca96f61a667f1b813c91251e822d93a999a5a9c240984cbce`
(source `12f4dfb2d535f0ea9c64278b66a9bef435d2e4b3`). Its encrypted-session
test was actually invoked in a disposable read-only, network-isolated container.
It failed plugin discovery: `openh264enc is not available`. This supersedes the
earlier cached-image prerequisite result; it is not an encryption pass.

The production `tilo-extension` consumer was also invoked against that image's
exact RPM base fingerprint
`8742f72a2365f83e615ae3e3782cfeba809a31287212f49d8a32a14363048c09`, copying only
its public catalog/trust key into an ephemeral layout. The exact-base manifest
request returned `Unpublished` (“Not available for this version of Tilo yet”).

A concrete candidate was then prepared with the existing supported
`tools/extensions/resolve.py` and `media-codecs/recipe.toml`, inside that exact
immutable image. Dedicated temporary volumes preserve shared-source labels;
DNF has temporary bookkeeping storage and the image's RPM database remains
read-only. All upstream RPM signatures, pinned signing-key fingerprints,
package compatibility rules and HTTPS artifact hashes remain enforced.
Both parts resolved: 15 artifacts in `tilo-codecs` and one matching Mesa artifact
in `tilo-codecs-mesa`. The unsigned candidate's SHA-256 is
`bc87df63715dbc947013d132702c055dd6d32ad4a91654cb971e464b1e9fe7c2`;
its revision is `20261006071645` and `resolve.py check` passes.

The encoder uses publisher-served `openh264-2.6.0-3.fc44.x86_64.rpm`, 445460 bytes,
SHA-256 `ddd3123c507f08d9ff1962371ada5dc555742dfa16d4543f87a53c1303c9da6c`,
from `https://ciscobinary.openh264.org/openh264-2.6.0-3.fc44.x86_64.rpm`.
The resolver refused Fedora's downgrade to an HTTP redirect. The GStreamer
plugin is `gstreamer1-plugin-openh264-1.28.1-1.fc44.x86_64`, compatible with the
base under the existing rule. Tilo redistributes neither publisher's RPM bytes.

Publication is explicitly held until the candidate is qualified end to end.
This is a release hold, not an automatic permission-review rejection. The
original workflow re-resolves before signing, so an unsigned candidate's raw
SHA cannot guarantee the package set a later workflow would publish. The
signed-artifact qualification mode and material-content pin documented in
`os/usr/share/tilo/system-extensions/README.md` separate testing from promotion
and reject material drift. Neither an unsigned manifest nor a locally invented
signing key can substitute for the image-trusted signature.

An existing genuinely signed software part from base
`52984c1a2ac5df71385f3bbb321c85e21e019bb62ab06ed1a63613d90d43011e`, revision
`20261003202922`, was admitted by the unchanged consumer on this image after
signature, upstream RPM and compatibility checks. Its twelve packages include
the matching GStreamer 1.28.7 OpenH264 plugin but omit Cisco's codec library.
The loaded `libopenh264.so.8` is the image's Fedora `noopenh264` 2.6.0 stub:
`WelsCreateSVCEncoder` returns 1 with a null encoder and `WelsCreateDecoder`
returns 3 with a null decoder. Correct ABI and successful plugin discovery do
not establish usable codec initialization. The real encrypted sender fails
before setup because the encoder cannot initialize. Production admission now
rejects this incomplete runtime, and the encrypted-session test first requires
thirty actual synthetic H.264 frames to encode and decode successfully.

The same signed fixture successfully encoded and decoded thirty 160×96 I420
frames through HEVC (`x265enc`/`libde265dec`) and VP8 (`vp8enc`/`vp8dec`), with
691200 decoded bytes and zero pixel error for each. Its x264 H.264 encoding
succeeded, while OpenH264 decoding failed initialization. These are independent
codec results, not AirPlay authentication, encryption or physical-receiver
qualification. No extension has been activated on a device. A genuinely signed
complete candidate still needs production admission, OpenH264 round trips,
the actual encrypted AirPlay session and selected-source capture before an
AirPlay success claim or feed-promotion recommendation.

# MoQ voice and media lifecycle

> Mirrored from the Tilo Cloud repository (`docs/superpowers/specs`). Relative links to sibling Cloud documents resolve in that repository.

Date: 2026-09-21. Status: proposed design; no currently shipped standards-based MoQ capability is assumed. Parent: [Reliable workspace](2026-09-21-reliable-workspace-design.md). Existing requirements: [MoQ voice](../../moq-voice.md).

## Decision and scope

Use IETF Media over QUIC Transport for voice publication/subscription. Browsers connect through WebTransport; the first native adapter uses the same qualified WebTransport profile so it shares endpoint behavior. A native QUIC profile can be enabled only when it proves interoperability on the exact same MOQT version and voice-object profile.

The initial protocol target is `draft-ietf-moq-transport-21`, published 2026-09-08, with a separately versioned `tilo-voice/1` media payload profile. Pin library/package revisions and negotiated draft in the qualification manifest. Advertising that version requires byte-level interoperability proof; do not label `moq-lite`, custom Iroh media messages, or a superficially similar API as standards-based MOQT.

The feasibility task evaluates an existing conforming implementation behind narrow session/publish/subscribe interfaces rather than implementing QUIC or all of MOQT inside Cloud. Current upstream projects provide candidates, not qualification evidence. If no maintained implementation passes the target revision, voice remains unavailable and durable workspace rollout may proceed; changing the pinned revision requires a recorded design update and full interoperability run. There is no “close enough” compatibility fallback.

The selected source candidate is `moq-dev/moq` commit `3aedaf3503146c664479f6fc53a50b36bb5e816d`, containing Rust `moq-net` 0.2.22 and browser source with Draft 21 support. Pin both sides to this source revision. Published `@moq/net@0.3.5` came from an earlier commit and lacks Draft 21 despite sharing the source package's version string. Explicitly offer only `moqt-21` and disable WebSocket support; upstream defaults are not the product's protocol policy. Preserve MIT/Apache license notices in the bundled source closure.

Source inspection identifies two integration gates: expose the standard authorization fields and admission hooks needed by this grant protocol, and enforce actual hard byte/age limits around the library's live cache. A URL bearer token is not a substitute for the specified device proof. A cache's soft target or protection of its newest group is not proof of the limits below. Any maintained fork must be small, source-pinned, reviewed, and included in byte-level interoperability tests. Source inspection establishes feasibility inputs, not successful network or audio qualification.

First release: voice-only group calls, at most eight participants, four concurrent publishers qualified at minimum, system-default audio output, browser and supported native clients. Video, screen sharing, recording, transcription, telephony and background mobile calling are separate features. Their future tracks must not reuse voice assumptions without their own profile and privacy review.

MOQT can carry arbitrary objects, but is used for media here. Durable workspace commands and CRDT records use delivery v3. Separate WebTransport sessions keep media failure and bulk-history flow control from owning each other's lifetimes. They share certified host identity and membership authority, not a generic message queue.

## Capability and ownership

Expose `mediaV1` from the native runtime only after native service, protocol, codec and browser checks pass. Its browser contract is a versioned capability snapshot plus `join`, `leave`, `setMuted`, `selectInput` and lifecycle subscription. Every call binds workspace, call ID, call epoch, participant/device and current user intent. Subscription snapshots expose safe metadata, levels and health, not keys or raw PCM.

The native room owns durable call definitions, roles, moderation policy and call epoch. Connected participant presence is a lease. Persisting an eternal “online” row is incorrect. Browser/native media controller owns capture, codec, subscription and playback resources. One controller exists per participating device/call; switching workspace does not silently join another call.

Native app surfaces provide the call controls and semantic actions so mouse, accessibility and automation use one capability boundary. Local microphone permission still requires the platform's user consent. An agent or remote participant cannot enable another device's microphone by changing shared state.

## Join and leave state machine

States: `unavailable`, `idle`, `preparing`, `joining`, `live`, `reconnecting`, `leaving`, `ended`, with independent local mute and capture-error status.

1. User chooses Join. Check supported WebTransport, capture permission, Opus encode/decode, AudioContext playback and input device. Show selected input and local mute intent before publishing.
2. Request a media grant through authenticated native control. Verify workspace role and call epoch. Durable call-creation/moderation commands use the normal invocation ledger; acquiring/renewing a short presence lease is idempotent by participation ID.
3. Open a dedicated WebTransport/MOQT session at the signed media endpoint. Authenticate the grant and device proof before any publish/subscribe. Establish a bounded subscription set and only then start permitted capture/publication.
4. Publish one Opus track for this device and subscribe to allowed other tracks. A listening-only member can subscribe without microphone access. Show live status only after actual media readiness; transport connection alone is insufficient.
5. On mute, stop publishing speech and drain stale encoder buffers; keep participation and subscription alive. Do not continue sending microphone samples hidden behind a UI mute flag. Device removal stops capture and shows a local input issue without disconnecting the workspace.
6. On Leave, immediately stop capture and publication, invalidate the local participation generation, unsubscribe, close streams and release tracks/AudioNodes. Best-effort native leave shortens the lease, but local stop cannot depend on network success. A delayed reconnect result must not rejoin after Leave.

Changing input preserves explicit user mute intent and waits for new capture readiness. The default on initial join is muted until the user enables the microphone. A transient reconnect while the same controller remains alive preserves that user's current microphone intent; browser reload starts unjoined and muted. Sign-out/identity change leaves calls and settles cleanup before reusing identity custody. No surprise automatic microphone start after reload.

## Authentication, revocation and privacy

Native authority issues a signed proof-of-possession grant binding certified root/device, room, channel/call ID, call epoch, opaque namespace prefix, relay audience, publish/subscribe directions, issue/expiry and membership/revocation generation. Lifetime is at most 60 seconds; renew after 20 seconds while participation remains intended. The relay verifies a fresh session challenge signed by the device, not possession of a bearer URL alone. Grant material is carried inside authenticated setup/control messages and excluded from URLs/logs.

Encode the application grant/proof in the pinned MOQT revision's supported authorization mechanism using a versioned `tilo-media-v1` proof domain; do not add arbitrary control-message discriminants to a claimed standards implementation. The interoperability gate includes challenge binding, namespace authorization and renewal. A library that cannot expose these hooks does not qualify. Grant renewal uses the authenticated native authority through available delivery-v3 control; a failed data carrier may use HTTPS for this small request without tearing down a healthy media session.

Room read permits subscription and room write permits publication for the first release, subject to call policy and local capture consent. The relay checks namespace/direction on every publish/subscribe request and enforces active grant validity during forwarding. Membership updates immediately revoke affected grants; deny access on inability to validate current authority beyond grant expiry. Target active revocation enforcement within one second when the host is healthy; expiry bounds a failed notification path to 60 seconds.

Presence lease is 15 seconds, refreshed every five seconds. A browser crash or network loss expires presence without altering membership. All renewal, reconnect and leave messages carry participation generation so stale activity cannot resurrect a prior call instance. If the host restarts and loses ephemeral lease state, reissue grants and create a fresh media session; durable call ID/policy remains. Ended call epochs never accept old grants.

The selected first-release privacy model is a **trusted media relay on the certified workspace host**. Transport encryption terminates there; it can read media objects. No media recording or payload logging is enabled. Product documentation must say this accurately and must not label calls end-to-end encrypted between participants. This explicit design choice requires review with the design, not an unnoticed TLS/proxy implementation change. End-to-end object encryption and membership rekeying are a separate future capability.

Use opaque namespace components rather than email addresses, task text or room names. Limit publishers/subscribers by verified call roster and capacity. One device cannot publish in another device's namespace. Rate-limit unauthenticated setup and fail malformed object metadata before allocating media buffers.

## Voice object profile

| Parameter | Initial qualification value |
| --- | --- |
| Codec | Opus mono, 48 kHz sample clock, 20 ms audio frames |
| Target bitrate | 32 kbit/s per publisher; adapt within 16–48 kbit/s under congestion |
| Track identity | Opaque room/call-epoch/participant/publication-generation namespace plus audio track |
| Object | One encoded frame with profile version, sequence, media timestamp and duration |
| Group | 100 ms / five sequential audio objects; increment group ID monotonically within publication |
| Delivery | Reliable MOQT subgroup streams; independent groups allow abandoning expired data without waiting for an old group |
| Sender queue | At most 100 ms of encoded speech; expire oldest unsent objects when exceeded |
| Relay live cache | At most 300 ms and 16 KiB per publisher, whichever comes first; no disk media cache |
| Receiver target buffer | Start at 60 ms, adapt within 40–120 ms according to measured jitter |
| Receiver hard buffer cap | 200 ms per subscribed track; drop stale objects and report discontinuity |
| Object size | At most 4 KiB including profile metadata |

These are the chosen initial profile parameters to test. If codec/network measurements cannot meet the latency gate, revise the profile and fixtures before rollout; do not silently tune different clients to incompatible object layouts. Pin the exact codec payload mapping alongside MOQT revision. A media profile field cannot change the meaning of a MOQT control message.

New subscribers start at the current live group; no historical catch-up. Publication generation changes after publisher restart, so reused local sequence numbers cannot mix old and new audio. Receiver validates sequence/duration, discards duplicates, and uses codec loss concealment where frames arrive too late. Do not assume wall clocks are synchronized: playout derives from media timestamps and local receive/jitter estimation. Clock drift correction is bounded resampling, not unbounded buffer growth.

### Revision 2026-09-22 (media hardening, measured)

Primary delivery is now one Draft 21 OBJECT_DATAGRAM per 20 ms object (single-object group, Object ID 0, END_OF_GROUP): no retransmission and no head-of-line blocking. The datagram payload is a redundancy bundle: the newest `tilo-voice/1` object followed by one earlier object (two while the client observes sustained loss), each byte-identical to its stream form and self-delimited by its payload length; at most three objects and 4 KiB. The relay validates every bundle before forwarding and forwards per datagram with a 50 ms / 8-entry send buffer. Five-object group streams remain the fallback and stay accepted by every receiver. The receiver target buffer is the 95th percentile of the frame-delay spread over the last ~2 s plus 10 ms, within 20–160 ms, trimmed back by dropping a quiet frame when the playout offset exceeds the target by 1.5 frames; one or two missing frames are concealed by repeating the last frame at falling gain. The 16–48 kbit/s bitrate adaptation and DTX are not implemented (WebCodecs offers no transport loss feedback in Chromium 153 and the encoder output correlation assumes one packet per input frame). See the media-hardening evidence (`task-media-harden-*`) for per-profile numbers.

Capture uses a tested native path or browser AudioWorklet/available encoding API; API existence alone does not prove Opus support. Prefer transferable bounded buffers and avoid per-frame copies through React. Playback and capture must not block canvas input. Echo cancellation/noise suppression use supported capture constraints and report effective configuration rather than claiming unavailable processing.

Retransmission is useful only before a playout deadline. Group delivery timeout/reset handling must map to the pinned MOQT implementation's actual semantics. A publisher/relay abandons a stream only according to that protocol, reports the resulting gap, and continues with current groups. Saved voice attachments, if separately offered, use durable content upload and do not use these expiration rules.

## Reconnect and shared resource behavior

A call transport failure moves only the call controller to reconnecting. Stop adding captured speech to a disconnected send queue; optionally maintain local capture solely for an immediate authorized restart, without publishing or storing samples. Try reconnect with 250 ms exponential jitter capped at two seconds for ten seconds, then back off to five seconds with a visible “Call disconnected” state. Stop retries after 60 seconds and release capture; require explicit Rejoin. Membership/permission failure ends retries immediately.

Rejoin authenticates fresh grants as needed and subscribes at the live edge. Never replay an outage's microphone backlog. If data delivery switches to HTTPS, a healthy MoQ session can continue; if QUIC itself is blocked, the call reports unavailable while documents/actions continue through HTTPS. Do not send speech over the document replay stream or restore WebSockets as a hidden workaround.

Separate connections do not eliminate shared CPU/network contention. Apply per-host media budgets and per-workspace bulk limits; cap document snapshot transfer during calls when measured bandwidth is constrained. Give small live edits and command status timely service too. Congestion adaptation lowers bitrate and expires obsolete media rather than borrowing unlimited RAM. Measure fairness with the actual transport libraries instead of assuming a priority API is sufficient.

## Required proof and delivery boundary

Before product integration, prove two real browsers and a native client exchange intelligible bidirectional Opus through the selected relay and exact wire revision. Then qualify at least four publishers and eight participants with the actual service limits. Record browser/OS, CPU, codec path, device/capture settings, protocol and artifact hashes.

Healthy same-region target: median mouth-to-ear at most 150 ms, p95 at most 250 ms. Use acoustic/electrical loopback; internal timestamps alone do not qualify audible latency. Report frame loss, late drops, underruns, duplicates, queue depth and recovery intervals. Fault matrix includes 1% and 5% induced loss, variable jitter, bandwidth reduction, relay restart, permission/device change, tab suspend/resume, Leave racing with reconnect, revoked membership, and document snapshot transfer during a call. Degraded-network figures are reported separately from healthy latency gates.

Browser support is demonstrated capture-to-playback for each advertised browser release. Missing QUIC/codec/permission support is an explicit capability failure. Do not make the whole workspace unsupported because voice is unavailable.

Implementation boundaries: dedicated runtime media controller and codec adapters; a versioned native media service injected with roster/grant authority; protocol-library adapter and relay live cache; native semantic call controls; safe Cloud capability validation; ingress configuration from the [rollout specification](2026-09-21-reliable-workspace-rollout-design.md). Keep MOQT dependencies optional for local-only/headless paths that do not expose voice.

Reference: [IETF MOQT draft 21](https://www.ietf.org/ietf-ftp/internet-drafts/draft-ietf-moq-transport-21.html) defines publish/subscribe over QUIC/WebTransport with streams, datagrams, priorities and partial reliability. The Opus grouping, queues, permissions and recovery policy above are this product's proposed profile, not claims that the draft supplies them automatically.

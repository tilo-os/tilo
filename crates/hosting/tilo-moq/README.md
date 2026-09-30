# Native MoQ voice and live adapter

`tilo-moq` carries the `tilo-voice/1` object profile and, behind `relay`, the
Tilo voice and `LIVE_V1` relays and native clients. They speak `moq-lite`
(draft 06) using the published `moq-net` crate, driven directly over
`web-transport-quinn`, the WebTransport implementation the host's shared
HTTP/3 endpoint already uses. Default builds only compile the codec. This
crate does not own room policy, identity keys, capture consent, the
workspace document transport, or the native action registry.

## Carrier

Every route (`/moq/voice`, `/moq/live`) is an HTTP/3 extended CONNECT that
negotiates the WebTransport subprotocol `moq-lite-06`. Before any MoQ stream,
the client opens one bidirectional stream and presents its grant there (the
admission preface): `version u8 = 1 | length u16 BE (1..=8192) | grant`. The
host checks version and length before allocating, reads the grant within
five seconds, admits or refuses it, and answers one status byte (`0`
admitted, `1` denied). Only after `ADMITTED` does the client start
`moq-lite`. The stream then stays open as the host's control channel: a
later `2` (revoked) or its end tells the client to close. A session that
skips or botches the preface is closed with WebTransport code `0x7400` and
never reaches MoQ or the host authority.

Grants never travel in URLs: URLs reach access logs, browser history and
developer tools, and browsers cannot attach headers to a WebTransport
CONNECT. `src/carrier.rs` is the normative description; the browser side is
`crates/browser/tilo-browser-runtime/web/moq-carrier.mjs`.

## Voice relay

The host implements `RelayAuthority`. Its admission method consumes the
preface grant and returns its own private `SessionAuthorization`
implementation after verifying the native authority's grant and fresh device
challenge proof. It must supply exact opaque namespace paths and recheck
current permission, expiry, revocation, call epoch, host boot, and
participation generation through `current`. No remote request may construct
that implementation. The callback must use current committed local authority
and must not block on network I/O.

`Relay::serve_session` lets the host's shared QUIC router hand over a
WebTransport session it answered with `moq-lite-06` on an exactly validated
route (`Relay::validate_route` on the raw CONNECT fields). It runs the
preface, the shared eight-session budget and the authority loop; the MoQ
driver stays in that future. `Relay::serve` is the standalone listener: at
most 16 pending handshakes, each bounded by `HANDSHAKE_BUDGET`.

Every admitted publisher writes into a private ingress origin scoped to its
exact namespace. The relay copies its `audio` track into the shared origin
and is the only path to subscribers: each datagram bundle and each object is
validated (`voice::validate_bundle`, `voice::validate`), a group holds at most
five objects, at most three groups are copied at once (a newer group evicts
the oldest instead of failing the session), and retention is 200 ms, so late
audio is dropped rather than queued. Caches are byte-bounded (64 KiB per
publisher, 512 KiB shared).

Exact subscription scopes are fixed at admission; a prefix-scoped subscriber
(`subscription_prefix`) is offered every publisher of its call and observed
against exactly those namespaces every 100 ms. Established sessions tolerate
`Unavailable` for at most 900 ms since the last confirmed observation, capped
by the verified grant deadline; explicit denial ends the session on the next
observation. On revocation the relay stops forwarding the session's
publication at once, sends the revocation notice, and closes the transport
when the client has closed or after `GOAWAY_GRACE` (1 s).

## Datagram delivery and redundancy bundles

Browser publishers send each 20 ms object as one `moq-lite` datagram
(single-object group) instead of five-object group streams: no
retransmission and no head-of-line blocking. The datagram payload is a
*bundle*: the newest `tilo-voice/1` object followed by up to two earlier
objects (`MAX_BUNDLE_OBJECTS` = 3), each byte-identical to its stream form
and self-delimited by its payload length. A receiver that lost the datagram
carrying object N recovers it from the next datagram 20 ms later. Group
streams remain the fallback and are still accepted everywhere; the native
client drains both channels and deduplicates by object sequence.

## Voice wire layout

All multibyte fields are little-endian. Total object size is at most 4096 bytes.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 1 | Version, exactly 1 |
| 1 | 1 | Flags, exactly 0 |
| 2 | 2 | Reserved, zero |
| 4 | 8 | Sequence |
| 12 | 8 | Media timestamp in 48 kHz samples |
| 20 | 2 | Duration, exactly 960 samples |
| 22 | 4 | Sample rate, exactly 48000 |
| 26 | 1 | Channels, exactly 1 |
| 27 | 1 | Reserved, zero |
| 28 | 4 | Nonzero encoded Opus payload length |
| 32 | variable | One raw Opus packet |

The native integration test invokes the existing browser JavaScript codec and
compares actual encoded/decoded bytes, including values beyond JavaScript's
safe integer range. Missing Node fails that qualification test explicitly.

## Live relay

`live::LiveRelay` carries ephemeral presence and ordered ops for
`tilo-live/<room>/<publisher>`. A publisher announces its namespace broadcast
(`presence` datagrams) and one broadcast per record it writes,
`<namespace>/ops/<record>`, with one ordered track `ops` (at most 16 open).
The relay re-publishes each member as one egress broadcast with `presence`
and on-demand `ops/<record>` tracks, validating and rate-limiting every
frame. A quiet ops track is sealed with a group holding one empty frame so a
late subscriber is not replayed delivered frames; readers skip empty frames.
`live::peer::Peer` is the native participant; `live::fallback` bridges the
same relay over plain HTTPS.

## Validation

```sh
cargo test -p tilo-moq --test voice
cargo test -p tilo-moq --features relay
```

The real network tests use localhost WebTransport and pin the generated leaf
certificate. They do not disable TLS verification or mock the network carrier.
The endpoint proof identities are isolated fixtures, never native workspace
authorization or a claim of real microphone/acoustic latency qualification.

## Isolated endpoint

```sh
target/debug/moq-proof --bind 127.0.0.1:0 \
  --cert /explicit/localhost.cert.pem --key /explicit/localhost.key.pem \
  --identities /private/isolated-identities.json
```

The identities file must have mode `0600` on Unix. It contains
`{"identities":[{"proof":[32 random byte values],"publish":"call/publisher",
"subscribe":["call/other"],"expiresUnixMs":123}]}`. Use opaque lowercase hex
components for the browser adapter. The numeric deadline must be later than
startup and at most 60 seconds away. A listening-only identity uses `null` for
`publish`. These exact secret fixtures are local protocol-test credentials;
they do not implement the native production grant and device challenge proof.

The process rejects nonloopback binds. It serves `/moq/voice` and prints
`READY` followed by JSON with `url`, `protocol: "moq-lite-06"` and
`identityMode: "isolated-proof-only"`. `COUNTERS` JSON reports content-free
session and forwarding counts. Ctrl-C closes the listener and all session
tasks. Neither credentials nor media objects are printed. The listener allows
32 concurrent streams per direction and supplies its own TLS identity.

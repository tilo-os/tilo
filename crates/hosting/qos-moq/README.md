# Native MoQ voice adapter

`qos-moq` carries the `tilo-voice/1` object profile and, behind `relay`, the
source-pinned `moq-dev/moq` Draft 21 WebTransport relay. Default builds only
compile the codec. This crate does not own room policy, identity keys, capture
consent, the workspace document transport, or the native action registry.

The host implements `RelayAuthority`. Its admission method consumes the
standard SETUP authorization bytes and returns its own private
`SessionAuthorization` implementation after verifying the native authority's
grant and fresh device challenge proof. It must supply exact opaque namespace
paths and recheck current permission, expiry, revocation, call epoch, host boot,
and participation generation through `current`. No remote request may construct
that implementation. The callback must use current committed local authority
and must not block on network I/O.

The intended native adapter wraps the private `RelaySession` from
`qos-session-peer::media_authority`; no dependency on session-peer is introduced
here, avoiding a host/transport dependency cycle. A successful isolated proof
does not establish this production integration.

`VOICE_PATH` is `/moq/v21/voice`; the relay rejects other paths and query
strings. `Relay::serve_request` allows a shared host QUIC router to dispatch
an already parsed MoQ request while retaining the same admission checks,
shared eight-session budget and expiry maintenance. The host must bound
transport/SETUP handshakes before dispatch and cancel its retained request
tasks at shutdown. `Relay::serve` is the standalone listener helper.

Exact subscription scopes are fixed at admission. A later participant join
requires an authorized reconnect with a freshly computed exact namespace set.
Production integration should observe a host-owned roster revision and refresh
the connection while preserving local mute state. A call prefix alone is not
authority to receive every namespace below it. The eventual public ingress
must route both MoQ and delivery-v3 on the shared host listener; this isolated
endpoint does not install that router.

## Datagram delivery and redundancy bundles

Browser publishers send each 20 ms object as one Draft 21 OBJECT_DATAGRAM
(single-object group, Object ID 0) instead of five-object group streams: no
retransmission and no head-of-line blocking. The datagram payload is a
*bundle*: the newest `tilo-voice/1` object followed by up to two earlier
objects (`MAX_BUNDLE_OBJECTS` = 3), each byte-identical to its stream form and
self-delimited by its payload length. A receiver that lost the datagram
carrying object N recovers it from the next datagram 20 ms later. The relay
validates every bundle (`voice::validate_bundle`) before any subscriber can
observe it and forwards per datagram with a 50 ms / 8-entry send buffer, so
late audio is dropped rather than queued. Group streams remain the fallback
and are still accepted everywhere; the native client drains both channels and
deduplicates by object sequence.

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

## Validation

Run plain Cargo in the native checkout, using its existing target directory:

```sh
cargo test -p qos-moq --test voice
cargo test -p qos-moq --features relay --test relay
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

The process rejects nonloopback binds. It serves `/moq/v21/voice` and prints `READY` followed by JSON with
`url`, `protocol: "moqt-21"` and `identityMode: "isolated-proof-only"`. The
protocol field declares the configured version; tests additionally assert the
actual negotiated session version. `COUNTERS` JSON reports only `accepted`,
`rejected`, `expired`, and `active` session counts. Ctrl-C closes the listener
and all session tasks. Neither credentials nor media objects are printed.

The relay uses the opt-in upstream hard voice model limits, exact directional
scopes and the complete-frame profile validator. Idle model expiry runs every
20 ms; host authorization is rechecked every 100 ms and before attaching
origins. The admission queue is capped at 16 pending handshakes with an
eight-second handshake budget (`HANDSHAKE_BUDGET`) and a five-second SETUP
budget (`SETUP_BUDGET`), sized so a 600 ms RTT satellite path can complete
after one loss recovery; eight session tasks can run concurrently. Production
host listeners should also restrict the QUIC stream count, as the proof
endpoint does (32), and supply their own certified TLS identity.

An authority may distinguish temporary observation contention from denial with
`AuthorizationStatus`. Admission always requires current authority. Established
sessions may tolerate `Unavailable` for at most 900 ms since the last confirmed
current observation, capped by the verified monotonic grant deadline. Repeated
unavailable observations cannot extend that deadline. Explicit denial closes on
the next observation. This bounds revocation delay during an owner commit without
treating an ordinary short commit as a disconnected call.

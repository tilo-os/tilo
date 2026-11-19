# Tilo patches: datagrams follow the handshake's answer; plain requests close their connection

crates.io `iroh-relay` 1.0.3, verbatim (see `PROVENANCE.md`) but for the
changes below, each marked `TILO PATCH` in the source.

## A client sends before the relay confirmed it (`src/client.rs`, `src/client/conn.rs`, `src/protos/handshake.rs`)

A client that cannot authenticate with TLS key material (a browser, whose
WebSocket exposes no exporter, or any `ws://` relay) answers the relay's
`ServerChallenge` with a `ClientAuth` and then waited for
`ServerConfirmsAuth` before `ClientBuilder::connect` returned. Nothing it
sends depends on that confirmation: the server reads the frames behind the
`ClientAuth` only once it confirmed it, in order, from the same WebSocket.
So the wait cost every relay connection a round trip before its first
datagram, which for a page entering a workspace is the QUIC Initial of its
session: a fresh visitor waited one more round trip for everything after.

`ClientBuilder::send_before_confirmed(true)` makes `connect` return as soon
as the `ClientAuth` is written (`handshake::clientside_unconfirmed`). The
connection's stream reads the server's answer as its first frame
(`handshake::confirmation`): a confirmation is passed over, a denial is the
stream's error, as a dropped connection is. A server that confirmed through
key material, with no challenge, is answered as before. It is off by
default, so `connect` still reports a denial itself unless asked;
`vendor/iroh` turns it on for its relay actor, which redials on a stream
error as on a failed dial.

## A plain request closes its connection (`src/server/http_server.rs`)

The relay keeps a connection only once it is a relay WebSocket:
`handle_connection` aborts any other 30 s after it was accepted
(`ESTABLISH_TIMEOUT`) and logs it at error level as a connection that "did
not reach established state within timeout". Every iroh endpoint homed on a
relay probes its latency there (`GET /ping`) now and then, over HTTP/1.1
keep-alive, and a reverse proxy in front of the relay (Caddy, as tilo.sh runs
it) keeps the upstream connection of such a request for its next one. So
each probe left a connection the relay aborted half a minute later, under the
proxy: one error in the relay's log every 30 to 90 s per endpoint homed on it,
and a request the proxy put on that connection as it was aborted failed.

`RelayServiceWithNotify::call` now answers every request other than a
WebSocket upgrade it accepts (a probe, a health check, a 404, a refused
upgrade) with `Connection: close`, so hyper closes the connection behind the
answer and nothing is left for the deadline to abort. A relay WebSocket is
answered as before.

## Built as an rlib only (`Cargo.toml`)

As in `vendor/iroh`: two path builds of a `cdylib` in one Cargo run write
the same output file. Nothing in Tilo loads the cdylib.

## Upstream

Prepared for upstream issues/PRs against `n0-computer/iroh` (not opened):

> **A relay aborts a pooled probe connection at its establish deadline.**
> Behind a reverse proxy that keeps upstream connections alive, every
> `GET /ping` leaves a connection that `handle_connection` aborts 30 s later
> and logs as an error. Proposed: answer requests other than an accepted
> relay upgrade with `Connection: close`.

> **A relay client waits a round trip for `ServerConfirmsAuth` before its
> first datagram.** Without key-material auth (browsers, `ws://`), the
> client answers the challenge and then blocks on the confirmation, though
> the server reads frames sent behind `ClientAuth` in order once it
> confirmed. Proposed: an opt-in `ClientBuilder::send_before_confirmed`
> that returns after writing `ClientAuth` and reads the confirmation (or
> denial) as the stream's first frame; iroh's relay actor enables it, so a
> dial over a relay saves a round trip.

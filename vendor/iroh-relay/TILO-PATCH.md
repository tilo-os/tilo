# Tilo patch: datagrams follow the handshake's answer

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

## Built as an rlib only (`Cargo.toml`)

As in `vendor/iroh`: two path builds of a `cdylib` in one Cargo run write
the same output file. Nothing in Tilo loads the cdylib.

## Upstream

Prepared for an upstream issue/PR against `n0-computer/iroh` (not opened):

> **A relay client waits a round trip for `ServerConfirmsAuth` before its
> first datagram.** Without key-material auth (browsers, `ws://`), the
> client answers the challenge and then blocks on the confirmation, though
> the server reads frames sent behind `ClientAuth` in order once it
> confirmed. Proposed: an opt-in `ClientBuilder::send_before_confirmed`
> that returns after writing `ClientAuth` and reads the confirmation (or
> denial) as the stream's first frame; iroh's relay actor enables it, so a
> dial over a relay saves a round trip.

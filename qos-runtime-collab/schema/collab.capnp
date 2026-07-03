@0xc418c22ce00b8d8d;

# qOS collaboration broker — public Cap'n Proto contract.
#
# This is the wire surface app SDK clients talk to. The broker is
# an in-tree app (`apps/collab-broker`); when an app is launched
# under qOS, the launcher hands the app a connected Unix socket
# whose other end speaks this protocol.
#
# Streaming caveat (capnp-rpc 0.26): there is no first-class
# server-push streaming primitive. We model "stream" as a long-lived
# capability the client polls with `read @0`. Each `read` returns a
# single op (or a remote-cursor update). The server side blocks the
# `read` promise until a value is available; the client invokes
# `read` again in a loop. This is also how capnproto's own examples
# (`calculator.capnp`) model unbounded fan-out. Back-pressure is
# implicit (no `read` outstanding = no delivery).

interface CollabService {
  # Join (or create-if-absent) a channel by id, signing the join as
  # `identity`. The wire `identity` is opaque bytes; the broker
  # interprets it as a qos-collab-types serialised identity record
  # (postcard-encoded). We pass bytes rather than a typed struct
  # because the identity format is qos-collab-types' domain and we
  # don't want this schema to drift every time that crate adds a
  # field.
  joinChannel @0 (channelId :Text, identity :Data) -> (handle :ChannelHandle);
}

interface ChannelHandle {
  # Submit a single signed op. `op` is a qos-collab-types
  # SignedOp (postcard-encoded). The broker validates, fan-outs to
  # other subscribers, and persists.
  send @0 (op :Data) -> ();

  # Open a subscription. Returned `Stream` capability yields ops
  # (postcard SignedOp bytes) one per `read` call.
  subscribe @1 () -> (stream :Stream);

  # Open the presence channel for this membership. Returned handle
  # owns cursor publish + remote-cursor subscription for this
  # channel only.
  presence @2 () -> (handle :PresenceHandle);
}

interface Stream {
  # Pull the next op. Promise resolves when one is available. The
  # caller is responsible for invoking `read` in a loop; the
  # underlying flow-control is implicit (no outstanding `read` =
  # nothing delivered).
  read @0 () -> (op :Data);
}

interface PresenceHandle {
  # Publish this user's cursor position into the channel.
  publishCursor @0 (x :Float32, y :Float32) -> ();

  # Subscribe to remote-cursor updates for the channel. Stream
  # is server-pulled (see `Stream` above).
  subscribeRemoteCursors @1 () -> (stream :CursorStream);
}

interface CursorStream {
  # Pull the next remote-cursor update. Returns the user id (as a
  # short string id), the x/y position. Promise resolves when an
  # update is available.
  read @0 () -> (userId :Text, x :Float32, y :Float32);
}

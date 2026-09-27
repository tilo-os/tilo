//! Local process transport preamble shared by launcher and broker.
/// Length-prefixed launcher handoff consumed before Cap'n Proto starts on a
/// collaboration descriptor. The broker binds it to `SO_PEERCRED` and a
/// compositor-created one-shot launch record.
pub const COLLAB_LAUNCH_PROTOCOL_VERSION: u16 = 1;
/// Prefix that separates a launcher-authenticated connection from a raw
/// Cap'n Proto diagnostic client. It is deliberately not an authority: the
/// broker still verifies the peer process against the compositor's one-shot
/// record before it accepts the following frame.
pub const COLLAB_LAUNCH_MAGIC: [u8; 4] = *b"QCL1";

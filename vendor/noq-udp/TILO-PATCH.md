# noq-udp: control messages read at any alignment

`cmsg::decode` asserted that a payload type is no more aligned than the
control-message header, then read it aligned. musl's `cmsghdr` is 4-byte
aligned, while the `timespec` of `SCM_TIMESTAMPNS` (which `UdpSocketState`
turns on for every Linux socket) is 8-byte aligned. So a static musl build,
the `tilo` command as it is released, panicked on the first packet it
received. The decoder now reads the payload with `ptr::read_unaligned` and
no longer asserts its alignment; glibc builds read the same bytes as before.
The encoder's assertion stays: what it writes (ECN bits, packet info, GSO
segment sizes) is at most 4-byte aligned.

Marked `TILO PATCH` in `src/cmsg/mod.rs`, with a test there
(`tilo_unaligned_tests`).

Upstream-ready description: "cmsg: decode payloads unaligned. On musl
`cmsghdr` is 4-byte aligned, so the `timespec` of `SCM_TIMESTAMPNS` is not
8-byte aligned and the alignment assertion in `decode` panics on the first
received datagram. Reading with `ptr::read_unaligned` is correct for any
alignment."

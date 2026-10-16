# Flatpak share pipeline — testing guide

The Flatpak share pipeline covers everything from capturing a shared
foreign Wayland surface to fanning encoded video out to workspace peers
over a bounded per-subscriber substream:

```
smithay surface commit
   → SurfaceTracker (DMABUF direct-import or SHM readback)
   → SharedFlatpakSession (codec negotiated with the viewers)
   → Encoder (media::RealtimeEncoder: VA-API / NVENC / V4L2 hardware,
     then royalty-free VP9/AV1 software, then Cisco openh264 for H.264)
   → SurfaceStreamSender (drop-oldest bounded queue per subscriber)
   → wire (dedicated substream + Iroh gossip announces)
   → PeerView (media::RealtimeDecoder + wgpu upload + composite)
```

Peers advertise what they encode and decode (`NeighbourMsg::MediaCapabilities`,
with every heartbeat). A share uses AV1, then VP9, when this machine encodes
it in hardware and every viewer decodes it in hardware; otherwise H.264. A
viewer that never advertised pins H.264. HDR uses a 10-bit hardware encoder
(AV1 or HEVC Main10) every viewer can decode, else the share runs in SDR.

## Quick reference

| I want to test... | Command | Where it runs |
|---|---|---|
| End-to-end pipeline on loopback | `scripts/test-flatpak-share.sh e2e` | anywhere |
| Software encode + negotiation | `scripts/test-flatpak-share.sh sw` | anywhere (VP9/AV1 need no GPU) |
| Hardware encode, dmabuf import, 10-bit HDR decode | `scripts/test-flatpak-share.sh hw` | self-skips without listed hardware |
| DMABUF direct-import path | `scripts/test-flatpak-share.sh dmabuf` | anywhere |
| QUIC substream framing + backpressure | `scripts/test-flatpak-share.sh substream` | anywhere |
| Two-netns real network | `sudo scripts/two-netns-share.sh` | bare-metal Fedora |
| Frame-link shim | `scripts/test-flatpak-share.sh shim` | anywhere |
| Side-by-side demo video | `scripts/test-flatpak-share.sh demo` | anywhere |
| **Everything the current host can run** | `scripts/test-flatpak-share.sh all` | mixed |

The test binaries need the GStreamer runtime plugins: `gstreamer1-plugins-good`
(libvpx), `gstreamer1-plugins-bad-free` (VA-API, libaom, parsers) and
`gstreamer1-plugin-dav1d`. Hardware tests look at the registry: the VA-API
and NVCODEC plugins list an element only when a device reports the
capability, so a test runs exactly where the hardware is.

## Environment matrix

| Path | Requires | Stock Tilo image | With the additional-codecs pack |
|---|---|---|---|
| VP9 / AV1 software | libvpx, libaom (dav1d decode) | ✅ | ✅ |
| AV1 hardware (AMD, Intel Arc+) | VA-API with AV1 encode | ✅ where the GPU has it | ✅ |
| H.264 / HEVC hardware encode | VA-API driver built with those codecs, or NVENC | ❌ (Fedora Mesa ships them disabled) | ✅ |
| H.264 software encode | Cisco `openh264` GStreamer plugin | ❌ | ✅ |
| NVENC / NVDEC | CUDA usable by the session | depends on driver state | depends on driver state |

## The two-netns test

`scripts/two-netns-share.sh` (existing) sets up two network namespaces
bridged by a veth pair, then spawns the share host in nsA and the
viewer in nsB. Frames flow over the veth link; input events flow back
over a second UDP port. Real network stack, no in-process shortcuts.

Requires `sudo` because the checked-in driver uses `ip netns add` and needs
`CAP_NET_ADMIN`. A host which permits unprivileged user namespaces can also
run the same two test roles in child network namespaces without host root.

The always-on UDP-loopback shape of the same test (`cargo test -p shell
--test it --locked in_process_udp_pipeline_delivers_frames_and_input_in_order`)
is what unprivileged CI relies on; it exercises the framing + backpressure +
decode pipeline without needing kernel netns.

## The side-by-side demo

`scripts/test-flatpak-share.sh demo` runs the `flatpak-share-demo`
binary with the defaults from the previous review round (10 s, 720p,
80 ms latency, 2 Mbps throttle). Output:

- `target/comparison.mp4` — side-by-side video (H.264 where this machine
  can encode it, else AV1 or VP9) you play in any media player. Left = source, right = same content through real encoder →
  real shim → real decoder.
- `target/comparison-frame150.png` — mid-video still if you want to
  eyeball latency without a player.

Knobs it accepts (pass after `--`):

```
--duration <secs>          # default 10
--width <px>               # default 1280
--height <px>              # default 720
--fps <n>                  # default 30 (software encoder ceiling)
--latency-ms <ms>          # default 80
--throughput-kbps <kbps>   # default 2000
--out <path>               # default target/comparison.mp4
```

For a visibly-drop-shaped run: `--throughput-kbps 500`. The stats
strip at the bottom of the output surfaces drops + effective bitrate
so you can compare configurations without a stopwatch.

## What each test actually covers

- `flatpak_share_e2e` — pipeline composition + backpressure isolation:
  encoded frames land in the receiver's queue, input events flow the
  other way, drop-oldest kicks in under load, decode path yields
  actual pixels.
- `flatpak_share_encoder_probe` — sessions start under every preference,
  software preferences never report hardware, and a legacy viewer joining
  an AV1 share switches it to H.264 with a new stream announcement.
- `dmabuf_direct_capture_path` — SurfaceTracker routes DMABUF vs SHM
  correctly; the DmabufDirect counter increments off the zero-copy
  path, ShmReadback off the CPU-copy path.
- `surface_substream_e2e` — QUIC-substream substrate: framing, drop-
  oldest per subscriber, gossip vs substream split, viewer teardown
  releases the subscription.
- `two_netns_share` — same pipeline over real UDP loopback (in-process,
  runs everywhere) and over an actual netns pair. The namespace roles compile
  only with `--features netns-e2e`, fail closed when their driver variables
  are absent, and run under `sudo scripts/two-netns-share.sh`.
- `hardware_encode` — every hardware encoder the registry lists
  round-trips through the shell's encoder and decoder; a GPU-tiled dmabuf
  reaches the VA-API encoder without a copy; 10-bit HDR encodes report
  PQ on decode. `hdr_decode_pipeline` proves 10-bit samples survive every
  decoder (hardware and dav1d) from a native 10-bit source.
- `frame-link` unit + proptests — latency shim delays frames by the
  right amount, token-bucket throttle limits throughput, drops
  accumulate when the ceiling is exceeded.

## Diagnostics on a live run

`PeerSurfaceMetrics` exposes:

- `chosen_encoder` (`VaApi | NvEnc | V4l2 | RoyaltyFreeSoftware |
  CiscoOpenH264`) and
  `capture_path` (`ShmReadback | DmabufDirect`)
- `encode_wallclock_ms_p50` / `p99` — smooths over a small ring
- `bitrate_bytes_per_sec_ewma` — EWMA of encoded output over ~5 s
- `keyframes` — running count of IDRs emitted
- `frames_received` / `frames_dropped` — receiver-side counters

These roll up into the OSD overlay when `TILO_UI_DEBUG=1` is set and
`debug_assertions` are on.

## Known limits

- Software encoder ceiling is 30 fps — `target_fps` is clamped under a
  software encoder, lifted under hardware.
- A stock image has no H.264 encoder at all (Fedora Mesa ships H.264/HEVC
  encode disabled and Cisco's openh264 is an opt-in download), so a share
  with a legacy viewer, projection and workspace control need the
  additional-codecs pack there.
- radeonsi's AV1 encoder pads the coded size without signalling the
  display size; receivers crop to the announced size.
- DMABUF direct-import capture: detection landed; fd extraction from
  the smithay commit hook lands on a separate branch. Until then, the
  `DmabufDirect` counter reflects "we saw a DMABUF" not "we streamed
  the fd zero-copy through the encoder."
- Two-netns pair via `ip netns exec` won't run in rootless podman.

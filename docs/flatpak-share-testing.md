# Flatpak share pipeline — testing guide

The Flatpak share pipeline covers everything from capturing a shared
foreign Wayland surface to fanning encoded H.264 out to workspace peers
over a bounded per-subscriber substream:

```
smithay surface commit
   → SurfaceTracker (DMABUF direct-import or SHM readback)
   → SharedFlatpakSession
   → Encoder (openh264 SW / VA-API / NVENC — chosen at start via probe)
   → SurfaceStreamSender (drop-oldest bounded queue per subscriber)
   → wire (dedicated substream + Iroh gossip announces)
   → PeerView (decode + wgpu upload + composite)
```

Different pieces need different environments to exercise. This guide
maps the test matrix + the scripts that automate the annoying bits.

## Quick reference

| I want to test... | Command | Where it runs |
|---|---|---|
| End-to-end pipeline on loopback | `scripts/test-flatpak-share.sh e2e` | distrobox |
| openh264 software encode | `scripts/test-flatpak-share.sh sw` | distrobox |
| VA-API hardware encode | `scripts/test-flatpak-share.sh vaapi` | distrobox (skips on non-H.264 hw) |
| NVENC hardware encode | `scripts/test-flatpak-share.sh nvenc` | build in distrobox, run on host |
| DMABUF direct-import path | `scripts/test-flatpak-share.sh dmabuf` | distrobox |
| QUIC substream framing + backpressure | `scripts/test-flatpak-share.sh substream` | distrobox |
| Two-netns real network | `sudo scripts/two-netns-share.sh` | bare-metal Fedora |
| Frame-link shim | `scripts/test-flatpak-share.sh shim` | distrobox |
| Side-by-side demo video | `scripts/test-flatpak-share.sh demo` | distrobox |
| **Everything the current host can run** | `scripts/test-flatpak-share.sh all` | mixed |

## Environment matrix

Not every backend can be exercised on every host. This is the
compatibility table for what you get out of a laptop dev box.

| Backend | Requires | On this host (AMD 780M + RTX 4070) |
|---|---|---|
| openh264 SW | nothing | ✅ runs everywhere |
| VA-API | `/dev/dri/renderD*` + H.264 encode profile | ❌ Radeon 780M mesa driver ships JPEG+VP9+AV1 encode only; probe self-reports unavailable |
| NVENC | `/dev/nvidia*` + `libnvidia-encode` + `libcuda` | ✅ RTX 4070 but rootless podman drops CUDA capability — **must run compiled binary on host** |
| DMABUF direct import | smithay DMABUF handler + wgpu import | Detection tested; end-to-end requires a client sending a DMABUF (e.g. a real Flatpak'd app under the compositor) |
| Two-netns real network | `CAP_NET_ADMIN` (sudo) | ✅ works on bare-metal, rootless podman blocks it |

## The NVENC-on-host runbook

Rootless podman doesn't grant CUDA the capability it needs — even with
`/dev/nvidia*` bind-mounted, `cuInit` returns "operation not permitted".
Building fine in distrobox produces a binary that fails on encoder init
inside the container but runs fine on the host.

Two paths, depending on whether your distrobox matches the host's
ffmpeg ABI:

### Matching-ABI container (Fedora 44)

When the distrobox and the host are on the same Fedora version, their
libavcodec/libavutil/libswscale SONAMEs match and the compiled binary
dlopens the host's shared libs directly. No shim.

Create it once:

```
podman pull registry.fedoraproject.org/fedora-toolbox:44
distrobox create --name tilo-app-44 \
    --image registry.fedoraproject.org/fedora-toolbox:44 \
    --nvidia --yes
distrobox enter tilo-app-44 -- bash -c '
    sudo dnf install -y \
        https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-44.noarch.rpm \
        https://mirrors.rpmfusion.org/nonfree/fedora/rpmfusion-nonfree-release-44.noarch.rpm
    sudo dnf install -y --skip-unavailable \
        gcc clang clang-devel make nasm pkgconf-pkg-config \
        libxkbcommon-devel libseat-devel libinput-devel libudev-devel libdrm-devel \
        mesa-libgbm-devel mesa-libGL-devel mesa-libEGL-devel pipewire-devel \
        ffmpeg ffmpeg-devel libva-devel libva-utils capnproto capnproto-devel \
        fuse3-devel dbus-devel harfbuzz-devel freetype-devel glibc-devel \
        systemd-devel wayland-devel wayland-protocols-devel \
        xz-devel zlib-devel bzip2-devel kernel-headers rust cargo
'
```

Set `TILO_DISTROBOX=tilo-app-44` when running
`scripts/test-flatpak-share.sh`. The nvenc subcommand then builds the test
binary and runs it bare on the host — `ldd` resolves everything against
`/lib64` naturally.

### Different-ABI container + `LD_LIBRARY_PATH` shim

If the container's ffmpeg ABI differs from the host's, the runner:

1. Compiles the test in distrobox with `--features nvenc`
2. Extracts the ffmpeg .so files from distrobox into a scratch dir
3. Symlinks the host's `libnvidia-encode.so.1` + `libcuda.so.1` into
   the same scratch dir
4. Runs the compiled test binary on the host with
   `LD_LIBRARY_PATH=<scratch>:/lib64`

Result identical either way — real bytes come out of real hardware.

### Numbers to expect on an RTX 4070 Laptop

- 720p sustained: 250–370 fps
- 1080p sustained: 150–170 fps
- First NAL emitted after ~2 frames (encoder look-ahead)
- Worst-case single-frame wall-clock: < 10 ms

### Host CUDA state gotcha

If every NVENC test panics with `cuInit(0) failed -> CUDA_ERROR_NO_DEVICE`
even though `nvidia-smi` shows the GPU present, CUDA userspace has
gotten wedged from the driver. Reload `nvidia_uvm` (`sudo rmmod
nvidia_uvm && sudo modprobe nvidia_uvm`) or reboot. This is unrelated
to the build path.

## The two-netns test

`scripts/two-netns-share.sh` (existing) sets up two network namespaces
bridged by a veth pair, then spawns the share host in nsA and the
viewer in nsB. Frames flow over the veth link; input events flow back
over a second UDP port. Real network stack, no in-process shortcuts.

Requires `sudo` because the checked-in driver uses `ip netns add` and needs
`CAP_NET_ADMIN`. A host which permits unprivileged user namespaces can also
run the same two test roles in child network namespaces without host root.

The always-on UDP-loopback shape of the same test (`cargo test -p shell
--test it --locked in_process_udp_pipeline_sustains_10_fps_and_bounded_latency`)
is what unprivileged CI relies on; it exercises the framing + backpressure +
decode pipeline without needing kernel netns.

## The side-by-side demo

`scripts/test-flatpak-share.sh demo` runs the `flatpak-share-demo`
binary with the defaults from the previous review round (10 s, 720p,
80 ms latency, 2 Mbps throttle). Output:

- `target/comparison.mp4` — side-by-side H.264 you play in any media
  player. Left = source, right = same content through real encoder →
  real shim → real decoder.
- `target/comparison-frame150.png` — mid-video still if you want to
  eyeball latency without a player.

Knobs it accepts (pass after `--`):

```
--duration <secs>          # default 10
--width <px>               # default 1280
--height <px>              # default 720
--fps <n>                  # default 30 (SW encoder ceiling)
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
- `flatpak_share_encoder_probe` — Auto resolves to the highest-priority
  available backend (NVENC → VaApi → Openh264); explicit choices honour
  or fall back cleanly; session `chosen_encoder` reflects reality.
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
- `vaapi_hardware_encode` / `nvenc_hardware_encode` — real bytes come
  out on real hardware. Self-skip on hosts without an H.264 encode
  profile; the NVENC file documents the on-host runbook.
- `frame-link` unit + proptests — latency shim delays frames by the
  right amount, token-bucket throttle limits throughput, drops
  accumulate when the ceiling is exceeded.

## Diagnostics on a live run

`PeerSurfaceMetrics` exposes:

- `chosen_encoder` (`Openh264 | VaApi | NvEnc`) and
  `capture_path` (`ShmReadback | DmabufDirect`)
- `encode_wallclock_ms_p50` / `p99` — smooths over a small ring
- `bitrate_bytes_per_sec_ewma` — EWMA of encoded output over ~5 s
- `keyframes` — running count of IDRs emitted
- `clock_offset_ns` — sender→receiver monotonic-clock offset learned
  from the `SurfaceStreamStart` handshake; `end_to_end_latency_ms`
  returns `None` until it's populated (a wrong number is worse than
  a missing one)
- `frames_received` / `frames_dropped` — receiver-side counters

These roll up into the OSD overlay when `QUICK_UI_DEBUG=1` is set and
`debug_assertions` are on.

## Known limits

- SW encoder ceiling is 30 fps — `target_fps` is silently clamped to
  the software ceiling under `Openh264`, lifted under hw backends.
- Radeon 780M (AMD Phoenix) mesa driver has no H.264 encode profile;
  Auto correctly falls through to openh264.
- DMABUF direct-import capture: detection landed; fd extraction from
  the smithay commit hook lands on a separate branch. Until then, the
  `DmabufDirect` counter reflects "we saw a DMABUF" not "we streamed
  the fd zero-copy through the encoder."
- Two-netns pair via `ip netns exec` won't run in rootless podman.

# miracast

A Miracast (Wi-Fi Display) **source** in Rust: mirror a screen to a TV,
projector or display adapter over Wi-Fi Direct.

- **WFD information elements** (`ie`): build the elements a source announces,
  read a sink's RTSP port and role from its elements.
- **RTSP negotiation** (`source`, `rtsp`, `params`, `modes`): the source side
  of messages M1 to M8 and the M16 keep-alive, capability exchange, video
  mode selection against the sink's CEA/VESA tables and H.264 levels, AAC or
  LPCM audio, IDR requests, source- and sink-initiated teardown.
- **Wi-Fi Direct groups** (`p2p`, feature `networkmanager`): find the sink,
  form a group through NetworkManager announcing the source's elements, learn
  the addresses. The group is bound to the D-Bus connection that asked for it.
- **Media** (`media`, feature `gstreamer`): capture a PipeWire node (what the
  ScreenCast portal grants) or a test pattern, encode H.264 with the best
  encoder present (VA-API, NVENC, V4L2, OpenH264, x264), add AAC or LPCM
  audio, and send MPEG-TS over RTP.
- **WFD LPCM muxing** (`ts`): an MPEG-TS muxer that writes LPCM the way WFD
  sinks such as Microsoft's Wireless Display Adapter expect (`stream_type`
  0x83 in `private_stream_1` with the WFD LPCM headers), which
  general-purpose muxers do not.
- **A strict test sink** (`sink`): negotiates as a TV would, checks each
  message, receives the RTP stream and reads the transport stream, to test a
  source end to end without hardware.

## Example

```rust
use std::net::TcpListener;
use std::time::{Duration, Instant};
use miracast::media::{AudioInput, GstMediaConfig, GstMediaFactory, VideoInput};
use miracast::source::accept_sink;
use miracast::{Source, SourceEvent, SourceOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(("0.0.0.0", miracast::ie::DEFAULT_RTSP_PORT))?;
    // ... form the Wi-Fi Direct group with `miracast::p2p::connect` ...
    let stream = accept_sink(&listener, Instant::now() + Duration::from_secs(30), &|_| true, &|| false)?
        .expect("the sink connected");
    let mut media = GstMediaFactory::new(GstMediaConfig::new(
        VideoInput::TestPattern,
        AudioInput::DefaultMonitor,
        8_000,
    ))?;
    Source::new(stream, SourceOptions::default())?.run(&mut media, &mut |event| {
        if let SourceEvent::Playing(plan) = event {
            println!("streaming {} to {}", plan.mode.name(), plan.sink_rtp);
        }
    })?;
    Ok(())
}
```

## Features

| Feature          | What it adds                                   | Needs                          |
|------------------|------------------------------------------------|--------------------------------|
| `networkmanager` | `p2p`: Wi-Fi Direct groups                     | NetworkManager, a tokio runtime |
| `gstreamer`      | `media`: capture, encoding, MPEG-TS/RTP        | GStreamer 1.20+ and its plugins |

The protocol modules have no dependencies.

At run time the media path needs GStreamer's base and good plugins,
`mpegtsmux` and `rtpmp2tpay` (bad), `pipewiresrc` for screen capture, an H.264
encoder and, for AAC, an AAC encoder (`fdkaacenc`, `avenc_aac` or
`voaacenc`).

## Testing

`cargo test --all-features` (or `t test`, see `t.toml`) runs the protocol tests, group formation against
a simulated NetworkManager, and the media tests, which encode a live test
pattern and send it to the test sink. The media tests need the GStreamer
plugins above with an H.264 and an AAC encoder, and fail without them. The
screen-capture test also needs `pipewire`, `wireplumber` and `pw-dump`; it is
skipped without them unless `MIRACAST_REQUIRE_SCREEN_CAPTURE=1` is set.

## Status

Used by [Tilo](https://code.tilo.so/tilo/tilo)'s screen projection. Not yet
published to crates.io. Not covered: HDCP, UIBC (input back from the sink),
R2 features such as direct streaming, and acting as a sink.

This is an independent implementation of the Wi-Fi Display specification;
it contains no code from other Miracast implementations.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Unless you explicitly state
otherwise, any contribution intentionally submitted for inclusion in this
crate by you, as defined in the Apache-2.0 license, shall be dual licensed as
above, without any additional terms or conditions.

# One live stack between devices

Everything that streams between the devices of a workspace (a call's voice and
pictures, a shared window's pictures, a viewer's input back into it, a web
page's display list) travels one transport: the workspace's `tilo-live`
session, a `moq-lite` broadcast per device per scope, sealed under the
workspace's keys, copied by a relay that cannot read it and admitted from the
roster and the device certificates. Encoders and capture stay where the seat
is (the session, an app's process); they publish tracks into that session.

A Tilo app's window is never streamed: each member's device opens the same
resources through the one record API and draws them itself. In a browser the
page renderer writes the window from its semantics tree (8067d0a2ebc).

## The stacks, and what happens to each

| Stack | What it does | Verdict |
|---|---|---|
| `crates/core/tilo-live` | Sealed `moq-lite` session per device per scope, the relay, admission from certificates and the roster. Tracks: presence, voice, camera and screen layers, a window's pictures (`window/<tab>/high`), a window's page (`page/<tab>`), the activity note. | **The one transport.** Gains the share control track (below). |
| The neighbour link's window messages (`SurfaceStreamStart`/`End`, `SurfaceViewRequest`, `SurfaceKeyframeRequest`, `RemoteInput`, `MediaCapabilities` in `neighbour-wire`; the network actor's `SurfaceStreamProtocol` ALPN, its control forwarding and `media_caps`; `Cmd::SendControl` in `session-client`) | A second path beside `tilo-live`: who views a shared window, what it decodes, its input and keyframe requests, a page's announcement, the end of a share. | **Goes.** All of it rides `tilo-live`'s share track. |
| `crates/engines/tilo-callstream` | RGBA↔access-unit encode and decode over `media`, the damage throttle, the surface decoder, the MP4 muxer. No transport of its own any more: the frame transport it once had is already gone. | **Stays** as the codec layer the window share, the projector, Remote and capture studio publish and decode with. |
| `crates/desktop/window-share` | Owner side: stage captured frames, encode them for what the viewers decode, the input registry. | **Stays**, owner-side capture and encode only. Its frames are published as `tilo-video/1` objects on `window/<tab>/high`; its input arrives on the share track. |
| `crates/engines/paint-stream` | A web page as a layered display list: the engine stays on the device that runs it, viewers draw items and patches with their own Vello. | **Stays**, as the payload of `tilo-page/1` on `page/<tab>`. The page renderer covers Tilo views in a browser; it does not cover a third party's page whose JS heap and cookies must stay on its host, and a picture of such a page shows whatever the page masks. So foreign web pages keep travelling as display lists, never pixels. |
| The desktop session's `calls/` and `live.rs` | Calls and the workspace's live session on the session's own iroh endpoint. Already `tilo-live` objects end to end. | **Moves into the daemon** with the endpoint (the session-as-daemon-client work owns it). Once the window share speaks only to a `LiveSession`, nothing of it is tied to the session's endpoint. |
| `crates/engines/tilo-capture` | Capture only. | **Stays.** |

## The share track

One track per device, `share`, published by every live session and read of
every device it hears, like presence. Each object is one postcard
`tilo_live::share::Said { to, control }`, sealed for the workspace's members,
so the relay forwards what it cannot read; a reader keeps only what is
addressed to its own device. Who said it is the device whose key sealed it.

| Control | From → to | Replaces |
|---|---|---|
| `View { views, hardware, software }` | viewer → host, again every 2 s while it views | `SurfaceViewRequest` and `MediaCapabilities`: which windows it shows at what size, what its hardware and software decoders take. The host names the viewer on its bar and admits its input from this. |
| `Input { tab, event }` | viewer → host | `RemoteInput`. |
| `Keyframe { tab, seq }` | viewer → host | `SurfaceKeyframeRequest`: a picture went missing. |
| `Page { tab, app_id, width, height }` | host → viewer | A page's `SurfaceStreamStart`. Pictures need none: a keyframe's header says codec, size and colour. |
| `Ended { tab }` | host → viewer | `SurfaceStreamEnd`. |

Input is sealed for the workspace's members, the same as the window's own
pictures: whoever may watch the window may read what is typed into it.

## Against the stories

- **A new workspace, people invited ad hoc.** Admission is the roster and the
  grants: a device joins the workspace's live session with its certificate,
  and only devices on the roster open what is sealed there. Nothing about the
  share asks for a second admission.
- **A Linux app streamed to the others, input sent back.** The host's session
  captures and encodes the window and publishes `window/<tab>/high`; viewers
  ask for that track, say `View` on the share track and send `Input` there.
  The host replays it on a seat of its own, only while the person allows it.
- **Tilo apps render natively.** A `LocalApp` tab is opened on every device
  from the same records; a browser gets it as semantic HTML. No pixels.
- **Browser viewers.** A page joins the same live session as one more
  device, asks for the window's `page/` and `window/` tracks and draws
  whichever the host publishes; its input goes on its own share track.
- **Calls and screen share.** Voice, camera and screen layers are already
  `tilo-live` tracks under the call's keys.

## Order

1. The window share's control and input move onto the share track; the
   neighbour messages, the network actor's surface protocol and control
   forwarding, `media_caps` and `Cmd::SendControl` are deleted with it.
2. The live session moves into the daemon with the endpoint; the window share
   and calls keep speaking to a `LiveSession`, wherever it runs.

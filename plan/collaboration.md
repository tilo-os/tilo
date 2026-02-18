# Collaboration Layer

## Overview

The collaboration layer enables real-time P2P workspace sharing. Users invite peers to individual workspaces (zone clusters), where they can see each other's cursors, view streamed application windows, and optionally interact with remote apps.

## Networking: Iroh

### Why Iroh over libp2p
- Simpler API, fewer moving parts
- Built-in NAT traversal (hole punching via relay servers)
- Identity = Ed25519 public key (NodeId)
- QUIC-based transport (encrypted, multiplexed)
- Designed for exactly this use case (local-first P2P sync)

### Connection Flow
```
1. User A generates invite: NodeId + Macaroon token
2. User A shares invite (QR code, link, or direct message)
3. User B connects to User A's Iroh endpoint
4. Handshake: User B presents Macaroon, User A validates
5. Bidirectional QUIC streams established for:
   - Automerge sync messages (reliable, ordered)
   - Video frames (unreliable, low-latency)
   - Input events (reliable, ordered)
   - Presence updates (unreliable, frequent)
```

### Iroh Endpoint Setup
```rust
let secret_key = load_or_generate_key();
let endpoint = Endpoint::builder()
    .secret_key(secret_key)
    .discovery_n0()       // Use n0 relay for NAT traversal
    .bind()
    .await?;
```

## Shared State: Automerge CRDTs

### What is Synchronized
- Zone positions, sizes, names
- Deck positions, sizes, active tabs
- Tab metadata (title, command -- NOT pixel data)
- User cursors (position, color, name)
- Permission grants (who can do what)

### Sync Protocol
```
Local mutation -> Automerge document change -> Generate sync message
  -> Send over Iroh reliable stream -> Remote peer receives
  -> Apply sync message to their Automerge document
  -> Workspace state updated -> Canvas re-renders
```

### Document Structure
```json
{
  "workspace_id": "uuid",
  "zones": [
    {
      "id": "uuid",
      "name": "Project Alpha",
      "x": 500, "y": 300,
      "width": 800, "height": 600,
      "decks": [...]
    }
  ],
  "cursors": {
    "node_id_hash": {
      "x": 150, "y": 200,
      "name": "Alice",
      "color": "#ff6b6b"
    }
  }
}
```

### Conflict Resolution
Automerge handles concurrent edits automatically:
- Two users move the same zone: last-write-wins per field
- Two users add zones simultaneously: both zones appear (set semantics)
- User disconnects and reconnects: sync catches up from divergence point

## Window Streaming

### Damage-Aware Protocol

Unlike game streaming which brute-forces 60fps compressed video, QuickDesktop only sends pixels that actually change:

1. **Trigger**: Wayland `wl_surface.damage` event from the compositor
2. **Capture**: DMA-BUF via PipeWire (zero-copy from GPU memory)
3. **Encode**: GStreamer pipeline with Region of Interest
   - Static content: high-quality, infrequent updates (perfect text)
   - Dynamic content: real-time encoding of damaged rectangles only
4. **Codec**: AV1 (SVT-AV1 low-latency mode)
   - Screen Content Coding tools for sharp text and solid colors
   - Massively better than H.264 for desktop content
5. **Transport**: WebRTC data channels over the Iroh QUIC connection
   - Packet loss on static screen: ignore (no retransmit needed)
   - Packet loss on typing update: retransmit aggressively

### Bandwidth Characteristics
| Scenario | Bandwidth |
|----------|-----------|
| Static screen (reading) | ~0 kbps (keepalive only) |
| Typing in editor | ~50-200 kbps (small damage rects) |
| Scrolling document | ~2-5 Mbps (large damage area) |
| Playing video | ~10-20 Mbps (full surface, 30fps) |

### GStreamer Pipeline (Conceptual)
```
pipewiresrc -> videoconvert -> svtav1enc (preset=12, tune=0)
  -> rtpav1pay -> webrtcbin
```

## Remote Input

### Input Event Protocol
```rust
enum RemoteInput {
    MouseMove { x: f64, y: f64 },
    MouseButton { button: u32, pressed: bool, x: f64, y: f64 },
    KeyPress { keycode: u32, pressed: bool },
    Scroll { dx: f64, dy: f64, x: f64, y: f64 },
}
```

### Coordinate Translation
```
Guest click at (canvas_x, canvas_y)
  -> Subtract deck position: (canvas_x - deck_x, canvas_y - deck_y)
  -> Scale to Wayland surface coordinates
  -> Send RemoteInput::MouseButton to host
  -> Host validates region + permissions
  -> Inject into Smithay seat for the target surface
```

## Presence

- Cursor positions broadcast at ~30Hz via unreliable Iroh streams
- Rendered as colored arrows/dots on the remote peer's canvas
- Name labels appear on hover
- Cursors fade after 5s of inactivity, disappear after 30s

## File Sharing (Future: tarpc)

### Remote Procedure Calls
```rust
#[tarpc::service]
trait FileService {
    async fn list_dir(path: String) -> Vec<DirEntry>;
    async fn read_file(path: String) -> Vec<u8>;
    async fn write_file(path: String, content: Vec<u8>) -> Result<()>;
    async fn stat(path: String) -> FileStat;
}
```

### Transparent Access
The file manager asks `fs.list_dir("/project")`:
- If local: calls `std::fs::read_dir`
- If remote: serializes request, sends over Iroh, remote host executes and returns result
- Result: browse remote files without downloading them

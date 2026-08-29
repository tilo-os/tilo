# Roadmap

## Where things stand

Single-monitor, single-user is feature-complete: strip tiling
(`Workspace → Strip → WindowGroup → Tab`), drag-to-detach tabs, undo/redo,
collaborative cursors, peer-shared tabs (H.264 over Iroh), per-workspace
Bubblewrap sandbox with portal-mediated grants, and FUSE-projected workspaces.

Shipped stack: `smithay` compositor · `vello`/`wgpu` paint · the CRDT-native
`ui` toolkit (`automerge` state) · Iroh collaboration and workspace hosting ·
`iroh` FS · ed25519 identity + signed invite tokens · Bubblewrap + seccomp
sandbox · Fedora bootc image.

## Toward daily-driver

- Multi-monitor — per-output active workspace, HiDPI per output
- Accessibility — AT-SPI tree
- Audio routing — shared microphone, per-workspace policy
- Notifications
- Session-lock + power-management hooks
- Full DRM hardware paint (vello → DMA-BUF zero-copy) on bare metal
- Hardware video encode (VAAPI) for surface streaming

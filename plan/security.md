# Security Architecture

## Threat Model

Three specific scenarios we defend against:

1. **Malicious Guest**: A user joins a workspace and attempts to access files outside the shared project, snoop on non-shared windows, or inject harmful input
2. **Compromised App**: A valid application has a vulnerability that a remote attacker exploits to break out of the sandbox
3. **Man-in-the-Middle**: An attacker intercepts P2P traffic to inject fake input events or steal file data

## The Four Rings of Defense

### Ring 1: Identity & Authentication

**Cryptographic Identity**
- Every user has an Ed25519 keypair generated on first boot
- User ID = hash of public key (same as Iroh NodeId)
- Stored at `~/.config/quick-desktop/identity.key`

**Capability-Based Access (Macaroons)**
- When User A invites User B, A generates a signed Macaroon token
- Token contains caveats (constraints):
  ```
  resource = workspace_alpha
  permissions = [view_screen, inject_input, read_fs]
  expiration = 2026-02-16T18:00:00Z
  max_input_rate = 120/sec
  ```
- Every RPC/stream request is validated against the Macaroon
- Tokens can be attenuated: A gives B a token with full access, B can create a restricted copy to share with C

**Connection Handshake**
```
1. Iroh QUIC connection established (encrypted by default)
2. Guest sends Macaroon token
3. Host validates: signature, expiration, caveats
4. If invalid: connection dropped immediately
5. If valid: streams opened per capability
```

### Ring 2: Filesystem Jail

**Namespace Isolation**
- Remote file operations are restricted to the shared project directory
- The FUSE/tarpc layer maintains `Map<UserId, Vec<AllowedPath>>`
- Any path traversal attempt (e.g., `../../etc/passwd`) is normalized and checked
- Access denied returns `EACCES`

**Default: Read-Only**
- Remote users have read-only access by default
- Write access is an explicit per-user, per-folder toggle
- All write operations by remote users are audit-logged

**Bubblewrap (Application Sandbox)**
- Each app spawned by the compositor runs inside bwrap:
  - Read-only binds: `/usr`, `/lib`, `/lib64`, `/bin`, `/sbin`
  - Read-write: only the project directory at `/project`
  - Hidden: `/home` (ephemeral tmpfs instead)
  - Network: `unshare --net` (loopback only, no internet)
  - Wayland socket: only the compositor's internal socket

### Ring 3: Input Firewall (Compositor Seat Filters)

**Hit Test Verifier**
```
Remote input arrives -> Transform to local coordinates
  -> Region check: Is (x,y) inside a shared window rect?
  -> Z-order check: Is the window visible and on top?
     (Prevents clickjacking via transparent overlays)
  -> If all pass: inject into Smithay seat
  -> If any fail: drop silently
```

**Keyboard Scrubber**
- Deny list: all system shortcuts from remote users
  - `Ctrl+Alt+F*` (TTY switch)
  - `Super` key (DE launcher)
  - `Alt+Tab` (window switcher)
  - `Ctrl+Alt+Delete`
- Focus guard: keystrokes only injected if a shared window has keyboard focus
- If host focuses a private window, remote keyboard stream is instantly muted

**Rate Limiting**
- Max 120 input events/second per remote user
- Burst detection: >10 clicks in 100ms triggers temporary block
- Prevents input flooding attacks

### Ring 4: Application Sandboxing

**Native Apps**: Bubblewrap containers (see Ring 2)

**WASM Plugins (Future)**: Wasmtime runtime
- Limited API: `draw_pixel`, `insert_text`, `request_file_read`
- No filesystem access, no network access
- Memory-safe by construction

**Flatpak Apps**: Portal-mediated access
- File access via document portal
- Screen capture via screencast portal (PipeWire)
- No ambient authority

## The Panic Button (Host Override)

**Physical Supremacy**
- If the host moves the physical mouse or types a key, all remote input is blocked for 500ms
- The host always has priority over remote users

**Kill Switch** (`Super+Escape`)
Instantly:
1. Terminates all P2P connections
2. Revokes all Macaroons
3. Unmounts any FUSE drives
4. Locks the screen

## Encryption

**In-Transit**
- All P2P traffic encrypted via QUIC (TLS 1.3 / Noise Protocol with ChaCha20-Poly1305)
- Iroh handles this automatically

**At-Rest (Future: Cloud Backup)**
- Files stored in S3/JuiceFS are client-side encrypted
- The S3 bucket only sees encrypted blobs
- Decryption keys exist only on authorized devices

## Security Matrix

| Attack Vector | Defense | Component |
|--------------|---------|-----------|
| Unauthorized access | Ed25519 keys + Macaroon tokens | Iroh / Auth module |
| File deletion/theft | Read-only FUSE mounts + namespaces | Bubblewrap / tarpc |
| Input injection | Seat filtering + region checks | Smithay compositor |
| Malicious code execution | Containerization + network isolation | Bubblewrap / Flatpak |
| Traffic snooping | End-to-end encryption | QUIC / Noise Protocol |
| Host takeover | Physical input priority + kill switch | Compositor logic |
| Clickjacking | Z-order verification | Compositor scene graph |
| Privilege escalation | Capability attenuation (Macaroons) | Auth module |

# Architectural Plan: The "Layer Above" (Collaboration & Permissions)

This document outlines the design for the Desktop Environment (DE) layer that manages collaboration, network synchronization, and app permissions for the QuickDesktop ecosystem.

## 1. Core Philosophy
- **Compositor as Orchestrator:** The DE (`quick-gnome-shell`) is the sole owner of the network stack and security policy.
- **Apps as Reactive Clients:** Apps (`quick-ui` based or legacy) receive "Collaboration Context" from the DE but do not manage P2P connections or permission enforcement themselves.
- **Cluster-Centricity:** Collaboration is scoped to a `Cluster`. All apps within a cluster share the same collaborator list and base permissions.

## 2. The Collaboration Protocol (`zquick_collaboration_v1`)
We will implement a custom Wayland protocol extension to bridge the DE and apps.

### Interface: `zquick_collaboration_manager`
- `get_collaboration_context`: Returns a `collaboration_context` object.
- `subscribe_events`: App registers to receive remote cursor and collaborator updates.

### Interface: `zquick_collaboration_context`
- **Events:**
    - `collaborator_joined(id, name, color)`
    - `collaborator_left(id)`
    - `remote_cursor_moved(id, x, y)`: Relative to the app's surface.
    - `permissions_changed(manifest_json)`: Updates to sandboxing/capabilities.
- **Requests:**
    - `sync_crdt(data)`: App pushes local CRDT changes (Automerge chunks) to the DE for broadcast.
    - `get_manifest`: Request full permission/sandbox details.

## 3. Adapters

### A. QuickUI Native Adapter (`quick-ui` integration)
The `quick-ui::App` loop will be extended to:
1.  **Auto-Discovery:** At startup, detect and bind to the `zquick_collaboration_v1` extension.
2.  **State Integration:** 
    - Automatically populate `state.collab.cursors` from protocol events.
    - Hook into `automerge` to pipe local changes through `sync_crdt`.
    - Provide a `permissions()` helper for widgets to check if they can access certain paths.
3.  **Visuals:** Render remote cursors natively within the Vello scene for zero-latency feedback.

### B. Legacy App Adapter (`quick-legacy-multiplexer`)
For apps that are not collaboration-aware (e.g., standard Wayland/Iced apps):
1.  **Compositor-Side Cursors:** The DE draws remote cursors *over* the app window.
2.  **Input Multiplexing:** The DE manages which user's input is currently being sent to the app's single pointer/keyboard focus (e.g., "Last Mover Wins" or "Explicit Floor Control").
3.  **Environment Emulation:** The adapter provides a fake "Home" or "Project" directory based on the Cluster's `permitted_paths` (handled via `bwrap` in `quick-gnome-shell`).

## 4. DE Implementation (`quick-gnome-shell`)
1.  **Network Integration:** 
    - `P2PClient` (libp2p) will be updated to handle `CRDTUpdate` messages in addition to `CursorUpdate`.
    - It will route these messages between machines based on the `Cluster ID`.
2.  **Security Enforcer:**
    - `Container` (bwrap) will be initialized with the `Cluster`'s permissions.
    - The DE will monitor app behavior and potentially revoking access if a cluster's security policy changes.

## 5. Next Steps
1.  **Define Protocol:** Create the XML definition for `zquick_collaboration_v1`.
2.  **Implement DE Server:** Add the protocol handler to `quick-gnome-shell/src/network/wayland.rs`.
3.  **Update quick-ui App:** Add the client-side binding logic.
4.  **Verification:** Run two `responsive_gallery` instances via the shell and verify that typing/cursors sync via the DE's P2P layer rather than direct app-to-app communication.

# Tilo product focus

The **main-focus apps** below are the priority — for building, fixing, and
demoing. Everything else in `apps/` (dev/system tools: disks, hex-editor,
db-client, logs, api-tester, snippets, firewall, vpn, containers, backups,
permissions, remote, imager, themes, store, monitor…) is secondary for
now: keep them working, but do not prioritize them over the focus list.

## Focus apps + intent

| App | Intent / goals |
|---|---|
| **YouTube** (tube) | YouTube client now; later PeerTube et al. |
| **Notes** | Block/outline editor. Its widgets are reused by Calendar (attach notes to events). |
| **Word processor** | Rich text; shares annotation/comment primitives with PDF + ebook. |
| **Spreadsheet** (sheets) | — |
| **Email** (mail) | — |
| **Terminal** | Also becomes a reusable embeddable widget. |
| **Files** (file-explorer) | — |
| **Browser** | WPE WebKit with built-in content filtering; extension packages remain inspectable until WebKit exposes activation APIs. |
| **Presentation** (slides) | — |
| **Calculator / graphing** | A *normal* calculator that also happens to do proper 2D/3D graphing — must feel at home as a calculator, not a graphing tool wearing a calculator hat. |
| **Typst editor** | — |
| **Video editor** | — |
| **Music** | Custom streaming: free YT-Music + downloads/caching + **our own library layer** for cross-device sync. (Spotify a fallback question.) |
| **Calendar** | Planify-style — between calendar and to-do. Reuse the Notes app/widgets to attach notes to events. |
| **Contacts** | — |
| **PDF reader** | Full signing + annotation + comments; later maybe full PDF *editing* (nothing does this natively on Linux — probably for a reason). |
| **Camera** | QR support now; full camera later (with phone support). |
| **Chat / video call** | Bespoke — see "Chat" below. Office/work needs it; it's collaborative + workspace-bound. |
| **RSS** (feeds) | Optional Gemini support. |
| **E-book** | Annotations/bookmarks/comments — shared primitives with PDF + word processor. |
| **2FA TOTP** | — |
| **Passwords** | Encrypted user-facing vault plus origin-bound credential broker for app sign-ins. |
| **Weather** | — |
| **Maps** | OSM-based; car/bike/foot nav (OSMand / Organic Maps-like); strong public transport (oeffi / public-transport-enabler-like); a custom crowdsourced real-time traffic + route-data layer over OSM. |

## Shared primitives to consolidate

- **Annotation / comment model** — PDF, ebook, word processor all need it. One shared model + selection HUD.
- **Notes widgets** — reused by Calendar (event notes). Notes is a dependency, not just an app.
- **Terminal-as-widget** — embeddable, reused by the IDE and anywhere a shell is needed.

## Chat / video — DECIDED: bespoke, over MoQ

Not Matrix. Rationale: Tilo already provides most of what Matrix buys (identity,
ad-hoc P2P, permissions), and Matrix's home-server + auth model is
architecturally incompatible with our ad-hoc P2P model — integrating it would be
customization hell.

- **Chat**: channels tied to workspaces; integrates the permission system;
  real-time + persistent messages; edits + edit history; emoji reactions; full
  media + file sharing via the FUSE FS.
- **Video**: extremely low latency; P2P via **iroh + a MoQ impl that supports
  iroh**; client-server via standard relays; conferencing, mute, audio-only,
  permissions. **No traditional screen-share** — everyone is (or can be) on the
  same workspace, so instead use the native collaboration layer to "show stuff":
  clients follow the host's actions, with customizable follow settings (e.g.
  auto-scroll with them).

## Browser engine

The browser embeds WPE WebKit behind `ui`'s webview interface. This provides a
maintained, non-Chromium engine that can render offscreen inside the Tilo shell.
Ad blocking uses WebKit content filters compiled from standard filter lists.
Extension packages can be downloaded and inspected, but activation remains
disabled until the embedded WebKit API exposes a supported runtime path.

# Tilo product vision — apps & system surface

Everything we ship must be usable for most users. Apps start empty and operate
on user state. Design is governed by `crates/sdk/ui-core/HIG.md`; per-app open issues are tracked in
the current [`ui-ux-overview.md`](ui-ux-overview.md) release ledger; this
document is the canonical scope + intent.

The current adoption audience is **tinkerers**: people willing to explore an
unreleased system and inspect how it works, but who should not need to be app or
OS developers. This affects first-run prominence—not capability tiers. Terminal
and system tools are easy to discover without displacing ordinary work. The
long-term product remains a general-purpose OS, so early technical affordances
must not harden into a builder-only information architecture.

On a fresh system, the empty Applications palette therefore explains and ranks
the installed surface instead of presenting one undifferentiated alphabetical
wall. **Start here** is Browser, Files, Notes, Terminal, Personal Settings, and
App Store. Everyday communication and planning follow, then Work and Media;
specialized administration appears under **System tools** but remains fully
searchable. Typing always ranks by the query rather than these defaults. This is
presentation, not an edition split: every installed app remains available and
the ordering can later become personal as usage history exists.

## Standard apps

- **YouTube client** (Tube) — later also PeerTube et al.
- **Notes**
- **Word processor**
- **Spreadsheet**
- **Email client**
- **Terminal**
- **Files**
- **Video player**
- **Browser** — engine still open; goals in priority order:
  1. be a *serious* browser (so likely not Ladybird or WebKit; Servo TBD),
  2. built-in / supported **ad blocking**,
  3. independence from Google/Chromium,
  4. proper **extension support** (i.e. Manifest v2).
- **Presentation** software.
- **Calculator / graphing** — a normal calculator that *coincidentally* also does
  proper 2D/3D graphing and feels at home either way.
- **Typst editor**.
- **Video editor**.
- **Music streaming** — custom: free YT-Music + downloads/caching + our own library
  layer for cross-device sync. (Alternative considered: Spotify/similar.)
- **Calendar** — inspiration: GNOME *Planify* (between calendar and to-do). Reuse the
  **Notes** app / its widgets so users can attach notes to an event.
- **Contacts**.
- **PDF reader** — full signing, annotation, comments. Later maybe full PDF *editing*
  (no Linux tool does this natively — probably for a reason).
- **Camera** — QR support now; full camera app later (once phone support exists).
- **Chat / video call** — needed because (1) office/work settings expect Zoom/Teams/
  Slack-class tooling, and (2) it's collaborative: chat is part of a *workspace*.
  - **Decision: bespoke chat + conferencing over MoQ** (not Matrix). Reasoning: we
    already have most of what Matrix buys, and its model (home servers) is
    architecturally incompatible with our ad-hoc P2P + auth model — bolting our auth
    onto Matrix would require extensive customization.
  - Chat features: channels tied to workspaces (details TBD); permission-system
    integration; real-time + persistent messages; edits + edit history; emoji
    reactions; full media support (via FUSE fs); FUSE fs file sharing.
  - Video call: extremely low latency; P2P via **iroh + a MoQ impl that supports
    iroh**; client-server via standard relays; conferencing, mute, audio-only,
    permissions. **No traditional screensharing** — everyone is (or can be) on the
    same workspace, so instead use the **native collaboration layer to "show stuff"**:
    clients follow the host's actions with customizable settings (e.g. auto-scroll
    with them).
- **RSS reader** — optional Gemini protocol support.
- **E-book reader** — annotations, bookmarks, comments (shares primitives with the
  PDF reader and word processor).
- **2FA TOTP client**.
- **Passwords and secrets** — encrypted credential custody, password generation,
  exact-origin app autofill, and explicit once/session consent through the
  system credential broker.
- **Weather**.
- **Maps** — OSM-based; car/bike/foot navigation (Organic Maps / OsmAnd class); strong
  **public-transport** support (à la oeffi / public-transport-enabler); a custom
  crowdsourced real-time traffic + route-data aggregation layer on top of OSM.

### Deliberately later (existing FOSS is currently better)
- **Photos** — eventually ours; for now Immich is the tool. Wants: face/object
  detection, full-text search (objects/events/descriptions/people/dates) with proper
  complex-query support, galleries + shared galleries, dedup, simple RAW editing
  (rotate/flip/basic filters), timeline-oriented.

### Maker software (need not be separate apps)
Code editor/IDE, git client, container/distrobox manager, API testing tool
(Postman/Insomnia/Bruno class), database client (psql/mysql/sqlite frontend),
diff/merge tool, text-snippet tool, hex editor.

## System surface (regroup freely — not necessarily one app each)

- **Settings** — network, mouse/keyboard, time, users, printers (visual previews of
  all print jobs + real printer status/settings — placement TBD), external account
  management (email/WebDAV/Nextcloud/…), Tilo identity management (transfer to/from
  device), update manager.
- **Themes** — browse, change, customize, create themes.
- **Firewall**.
- **Software store** — open questions: how does installing work? how are apps
  distributed? role of Flatpaks? how does proper app isolation work?
- **Remote machines** — start remote desktop sessions (to other Tilo: render locally,
  stream data only!); VNC + SSH. Goal: operate your own NAS/homeserver,
  which runs your selected workspaces in "relay mode."
- **Backups / snapshots** manager.
- **ISO / installation creator** — install arbitrary .isos; install Tilo (to USB or
  disk; option to make a USB a permanent install); customizable install options
  (user creation etc.) for a nearly headless install. The prepared choices are
  applied from the installation medium; disk selection, destructive consent,
  device-local secrets and recovery-key acknowledgement remain on the target.
- **Activity monitor**.
- **Disk utility**.
- **Log viewer** — graphical, systemd logs etc.
- **VPN client** — WireGuard + OpenVPN; optional Tor.

# qOS app review — 2026-07-28

Comprehensive UI/UX/functionality review of every app, graded against `ui/HIG.md`
and `docs/product.md`. Method: build each app, shoot every screen headless
(`--shots`), **open each PNG and inspect it critically**, drive the real
interactions, and record findings. No grading off empty-state shots. This file is
the ledger of what's wrong; fixes are tracked in `apps/MISSING.md`.

Status legend: 🔴 major (unusable / mock / wrong chrome) · 🟠 real defects ·
🟢 genuinely good · ⚪ not yet inspected.

---

## Systemic issues (hit many apps — fix at the toolkit/discipline level)

### S0 — the split that predicts app quality
Apps that read **real system/OS state** (settings, file-explorer, monitor) are
genuinely good — real data is always there, so they show it. Apps that need **user
content that doesn't exist yet** (chat messages, calendar events, camera photos,
backup history, api collections) mostly **faked it** instead of showing empty states.
The fix effort concentrates on the second group: start empty + wire the real backend.

### S1a — the precise tell: **does the LIVE constructor seed sample data?**
Code-tracing all apps revealed the exact fault line. Apps that confine sample data to
`--shots`/tests and read the real backend in their live constructor are genuinely real
(containers→podman, disks→lsblk/udisks2, logs→journalctl, git_client→git2,
mail→AccountStore-empty, contacts→disk vCards). The offenders **seed sample content in
their live constructor** (`feeds` `sample::store()`, `firewall` `sample_config()`,
`code_editor` `sample_project::build()`, `db_client` `with_sample()`, `diffmerge`
`samples::*`). Fix is often small + mechanical: don't seed in the live path; start
empty on the real backend that ALREADY EXISTS.

### S6 — 🔴 SAFETY: firewall shows fake state but writes real
`firewall` never reads the system's actual zones/rules (no `firewall-cmd --list`/`nft
list` anywhere) — it displays a fabricated ruleset, yet `apply()` runs real
`firewall-cmd` in Live mode. So you'd be realizing a made-up ruleset on the host from a
security tool. Must read/import real current state before it can display or apply.
Treat as high priority.

### S1 — 🔴 Pervasive fake/demo data in content apps (the biggest issue)
Multiple apps ship **hardcoded sample content** instead of starting empty on real
data. Confirmed by inspection: **chat** (fake workspace "Acme Design", fake people
Devon/Sora/Aria, fake messages, fake voice huddle, fake reactions), **calendar**
(fake events "Design review"/"Team standup", fake projects Work/Personal/Health,
fake iCloud calendars), **camera** gallery (fake photos "Golden hour"/"City lights"
with fabricated 4032×3024/18.3 MB metadata). These *look* impressive but violate the
product rule "no mocks — genuinely usable, start empty." Fix: the LIVE path must show
honest empty states and operate on the real store/backend; sample data may exist ONLY
behind `--shots`/demo flags. (Settings does this correctly — real system state, honest
empty states — use it as the model.) Suspected also in: music, mail, feeds, tube,
contacts, notes — to confirm during the sweep.

### S2 — 🔴 Immersive apps wearing the content-app sidebar (wrong chrome class)
**Camera** wears a generic desktop nav rail (Camera/Scan/Gallery/Settings + its own
logo glyph) when a camera must be an immersive full-bleed viewfinder with floating
controls. Immersive apps (camera, video player, maps, games) are a distinct chrome
class from content apps and must NOT get the sidebar. Being codified into `ui/HIG.md`
+ camera redesigned. Check video-player and maps for the same mistake.

### S3 — 🟠 Missing-glyph "tofu" from unshipped font ranges
**Settings** Wi-Fi signal bars (U+2581–2588 block chars) rendered as blank `.notdef`
boxes — no glyph in the shipped fonts. Fixed (→ MDI wifi-strength icons). Sweep for
other block-char / exotic-Unicode usage that will tofu on the real font stack (this is
a per-app risk wherever an app draws its own "meter"/"bars" with Unicode).

### S4 — App-drawn chrome that the OS should own
Chat appears to draw its own far-left workspace/server rail (AD/QC/WJ switcher).
Per the app↔chrome boundary, workspace switching is OS/shell (the wheel disc), not an
app concern. Confirm and, where found, remove app-owned window/workspace chrome.

---

## Per-app findings

### camera — 🔴 major (redesign in flight)
Out-of-place desktop nav rail on an immersive app (S2); fake gallery photos +
fabricated metadata (S1); Settings pane is a narrow card column floating in empty
space; app-drawn logo glyph. Live `cargo run -p camera` does use real GStreamer v4l2
capture (good). Redesign underway: full-bleed viewfinder, floating controls, empty-by-
default gallery, settings-as-sheet, HIG immersive-class section.

### settings — 🟢 good (reviewed + fixed, landed 28f77f8c)
Docked sidebar w/ search on top, no collapse button; narrow = separate list/content
screens w/ back arrow (HIG-correct); cards align on one grid; real data throughout
(real email account, battery, host, PipeWire device, timezone); honest empty states.
Fixed: Wi-Fi signal tofu (S3), a leaked accounts-store path, fabricated Users/Accounts
sample → honest empty states. 198 tests green. Follow-ups: stale committed shot
fixtures; Permissions&Roles editor gated on the permissions service.

### chat — 🔴 major (fake data + backend gap)
Visually strong + feature-rich (channels, voice huddle overlay, threads, reactions,
media attachments) BUT entirely fake demo content (S1) and a likely app-drawn
workspace rail (S4). Per product.md chat is bespoke + MoQ — the UI shell exists but
must start empty and wire to the real collab/MoQ backend (real-time + persistent
messages, edits+history, reactions, FUSE media/file-sharing). Currently a mockup.

### calendar — 🟠 good structure, fake data
Planify-style smart-lists (Inbox/Today/Upcoming/Completed) + projects + multiple
calendars + Month/Week/Day/Agenda + an event modal with a rich-text NOTES section
(matches the "attach notes to events" vision). But all events/projects/calendars are
fake sample data (S1) — needs a real event store + empty state. Structure is a keeper.

### calculator — 🟢 good (minor)
Real 2D graphing with correct math (rational asymptote / parabola / sqrt / sine all
plot correctly), function editor with fraction rendering, RAD/DEG — matches the
"calculator that also graphs" vision. Minor: draws its own "Calculator" app-name+logo
header (S5); pre-loads 4 sample functions (borderline — a tool seed, not content, but
consider starting minimal).

### api_tester — 🟠 good structure, verify it's real
Postman/Insomnia-class: collections (Users API / Auth / Webhooks), env vars
(`{{base_url}}` resolved), Params/Headers/Auth/Body tabs, response viewer
(Pretty/Raw/Preview) + history with status codes. BUT the response body is canned
demo ("Ada Lovelace / Computing Pioneer / joined 1843-10-01") — MUST verify "Send"
performs a REAL HTTP request and the response reflects it (start with empty
collections / no canned response), not a mock. Sample collection is borderline (S1).

### S5 (systemic) — 🟠 apps draw their own app-name/logo header
camera, calculator (and chat's "Acme Design") render an in-app title + logo. The OS
owns window chrome (titlebar / app identity) — apps must not draw it. Sweep + strip.

### backups — 🟠 good structure, fake data
Comprehensive snapshot manager (Timeline/Sources/Destinations/Schedules, stat cards,
per-snapshot Restore/Verify/Delete, usage bars) matching the "snapshot & backup
manager" vision. But all fake (58.1 GB protected, Seagate 2TB / Backblaze B2 dests,
the whole timeline, "1 failed"). Needs a real backend (btrfs/restic) + empty
onboarding. App-drawn header/logo (S5). Cards align (fine).

### file-explorer — 🟢 good (real data), internal debt
Shows the REAL home filesystem (real folders + dates), clean list/grid, places
sidebar (Favourites/System), search, sort. No fake data. Internal debt: ~1122-LOC
bespoke places-sidebar (fixed 180px, no narrow/responsive) — should adopt ui NavSplit
(per ui/UPSTREAMING.md). Folder Size shows "—" (consider item counts). Visually fine.

### ebook — 🟠 good reader, fake sample book
Sepia paper + serif body + TOC + colored annotations (bookmark/highlight/note w/ ch/¶
refs) + reading progress — matches the "annotations/bookmarks/comments" vision and
shares annotation primitives with PDF/word-processor. But ships a fake book ("The
Lantern-Keeper's Almanac" / Wren Halloway) + demo annotations (S1) — live app should
open to an empty library and real EPUBs. Draws its own title header (S5).

### imager (Install Media) — 🟢 comprehensive, verify real detection
Matches the "ISO/installation creator" vision closely: Write-ISO / Install-qOS,
Live-stick / Persistent / To-disk modes, user creation (hostname/user/pass/locale/
keyboard/tz), disk layout (Whole/Sep-home/Btrfs), LUKS + auto-login toggles, SAVED
PROFILES + "Export unattended install config", and a "type the device name to
confirm" safety gate. Target drives shown look real (SanDisk USB + the host's actual
Samsung NVMe w/ EFI+fedora partitions). MUST drive-verify: real block-device
enumeration (lsblk) + real ISO listing vs seed. App-drawn header (S5). The pending
"wizard flow" rework is debatable — the dense single-screen is arguably better for a
desktop installer than a forced step-wizard.

## Full-sweep verdict table (driven + code-traced)

| App | Grade | Verdict |
|---|---|---|
| contacts | 🟢 | Real disk vCards, empty on live, sample=shots. Minor S5 header. |
| code_editor | 🔴 | Mock: edits a hardcoded in-memory "ferris-cli", **no open/save of real files** (no portal, no fs). Real highlight/terminal but unusable as an editor. |
| containers | 🟢 | Real podman/distrobox shell-out; sample only offline/shots. Dup title (S5). |
| db_client | 🟢 | Real SQLite + **real PostgreSQL & MySQL** (sync `postgres`/`mysql` drivers): live connect, catalog introspection, real queries; honest error on refused/failed auth. Empty on live, sample=shots. |
| diffmerge | 🟠 | Real diff + 3-way merge engine; auto-opens a sample conflict on live. |
| disks | 🟢 | Real lsblk/smartctl/udisks2; refuses to fabricate; honest empty on failure. |
| feeds | 🔴 | Real RSS/Atom/**Gemini** worker EXISTS but live boots fake articles (`sample::store()`). Pure S1 — small fix. |
| firewall | 🔴 | **S6 safety**: shows a fabricated ruleset it never read; Apply writes it to real firewall-cmd. |
| git_client | 🟢 | Real git2 on the actual repo; honest empty state. |
| hex_editor | 🟢 | Real file bytes + inspector; gap: no in-app Open (CLI path only). |
| logs | 🟢 | Real journalctl JSON reader + live-tail; sample only if journald empty. |
| mail | 🟢 | Empty on real AccountStore, real MIME; IMAP/SMTP behind `--features live` (off default). |
| maps | 🟠 | Real OSM/OSRM/Overpass/Nominatim; immersive chrome CORRECT. Live tiles broken (rustls bug); no traffic layer. |
| monitor | 🟢 | Real sysinfo; genuine host processes. Copy-PID no-op; minor layout. S5 header. |
| music | 🟢 | Live starts EMPTY (test-enforced); real YT-Music behind `--features live`. |
| notes | 🟢 | Real Automerge block editor, empty on live. Good base for calendar reuse. |
| osk | 🟢 | Real `zwp_virtual_keyboard` injection + FUTO swipe; clean render. |
| pdf_viewer | 🟢 | Real pdfium; drove real doc — render/search/annotate/comment/sign. Needs pdfium native lib. |
| permissions | 🟢 | Real capability model, empty authored set on live; wired to OS registry. |
| remote | 🔴 | **Worst**: seeds 8 fake machines into the CRDT on live launch; SSH/VNC transport stubbed (fakes terminal + desktop). |
| sheets | 🟢 | Real formula engine + real `.xlsx`/`.ods` codecs (calamine); empty on live. |
| slides | 🟢 | Real deck editor, one blank slide on live; sample=shots. |
| snippets | 🟠 | Real store but seeds a 14-item starter library on live (borderline — presets vs content). |
| software_store | 🟠 | Real flatpak/Flathub backend, but a hardcoded sample catalog w/ **fake ratings** is the live storefront. |

| store | 🟠 | Real registry+Flathub backend + real install, but shopfront half-wired: **dead search box + dead filter chips**, placeholder icons, no scroll, `"[All]"` literal, stray "update" debug label. |
| terminal | 🟢 | Fully real PTY (`$SHELL`), selection, SGR, OSC52. Title→OS chrome. No issues. |
| themes | 🟢 | Real theme apply (writes shared settings.json compositor reads). Nits: app-drawn header (S5), idle redraw. |
| totp | 🟢 | Real RFC-6238 + XChaCha20 vault + portal QR import; starts empty. Nit: header (S5). |
| tube | 🟠 | Real YT backend (`--features live`), empty default, no fake fallback. But **dead "Invidious instance" control** (comment lies it works); app header (S5). |
| typst_editor | 🟢 | Real Typst compiler end-to-end (math renders, PDF export). Clean chrome. |
| video_editor | 🔴 | Real timeline/CRDT engine but mock shell: fake project seeded LIVE, **no media import at all**, preview never real video, **export is a stub w/ dev-TODO text in the dialog**. |
| video-player | 🟠 | Real GStreamer playback + honest test-pattern, but **wrong chrome**: persistent top bar (not immersive §1e) even in fullscreen. |
| vpn | 🔴 | Simulation dressed as real: fake tunnels seeded live, **fake Connect toggle**, simulated throughput, **dead Import buttons**, no portal, real exec off. |
| weather | 🟢 | Real Open-Meteo, HIG §1b correct, units from Settings. Ships 4 real default cities. |
| word-processor | 🟢 | Real `.docx`/`.odt` round-trip (qdoc), drove real files, starts blank. Nit: Save-As no portal. |

## Tally (45 apps)
- 🟢 **~24 genuinely good** — real backends, empty-on-live, samples gated to `--shots`.
- 🟠 **~14 real defects** — mostly S1 (live-seeded) or dead controls / minor chrome.
- 🔴 **7 major** (+ camera, now fixed): chat, code_editor, feeds, firewall, remote, video_editor, vpn.

## Fix plan (prioritized)
1. **firewall safety (S6)** — read real zones/rules (`firewall-cmd --list`/import) before displaying or applying. Do first.
2. **The 7 reds → real or honestly-empty:** vpn + remote (real WireGuard/OpenVPN / SSH+VNC, or start empty + mark simulation clearly), video_editor (real media import via portal + real preview/export, or gate honestly), code_editor (real open/save via portal), feeds (delete the live seed — the real RSS/Atom/Gemini worker already exists), chat (real MoQ/collab backend + empty channels).
3. **S1 live-seed sweep (mechanical, high-ROI):** stop seeding sample data in live constructors — feeds, firewall, remote, video_editor, vpn, snippets, software_store, store, diffmerge, backups, calendar, ebook, api_tester, chat. Move the seed behind `--shots` (the good apps already do this). Often a 1-line change.
4. **Dead controls:** store (search + filter chips), tube (Invidious setting), store detail sheet.
5. **Chrome:** video-player immersive §1e; strip app-drawn name/logo headers (S5) across ~11 apps; land the shared `ui` sidebar/card unification (Card/CardGrid built) + migrate the bespoke-sidebar offenders (file-explorer 1122 LOC, backups, firewall).
6. **Feature gaps vs product.md:** maps live-tile fix + traffic; portal Open/Save-As where missing (word-processor, typst, code_editor, hex_editor); mail/music/tube `--features live` default-on decision.

<!-- SWEEP IN PROGRESS. 10 apps inspected by hand (above).

<!-- SWEEP COMPLETE (45 apps): 10 inspected by hand (detailed above), 35 driven +
code-traced by the review fleet (verdict table above). The two worst safety/mock
findings (firewall, remote) were re-verified by hand in source. -->

---

## Methodology note
Every app was built and shot headless; each shot was opened and graded; content apps
were code-traced to answer "does the LIVE constructor seed fake data, or is the seed
gated to `--shots`?" and driven where a static shot proves nothing (opened real PDFs/
docx/odt, ran a real shell, hit real journald/lsblk/podman/git). Grades reflect the
LIVE path, not the hermetic `--shots` render.

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

<!-- SWEEP IN PROGRESS. 9 apps inspected by hand (above). S1 (fake demo data) is
confirmed pervasive — nearly every content/data app is seeded with hardcoded sample
content and must be reworked to start empty on a real backend. Remaining ~34 apps are
being swept with a driving pass (click Send / open a real file / hit a real endpoint)
to separate "real backend behind a demo seed" from "pure mockup", plus per-app UI
bugs and functionality gaps vs docs/product.md. Findings appended as they land. -->

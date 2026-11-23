# Handoff — Photos → tl Gallery (port + deletion)

Worktree: `/srv/rcargo/t3-builder-home/work/tilo/.claude/worktrees/photos-port`
Branch: `photos-port` → pushed as **`photos-port-wip`**. Head before commit: `af8a152fb02` (WIP commit contains everything below; the working tree was fully staged).
`origin/main` moved to `f6768c336fb` while this was built (crates-into-groups lane). **Rebase onto main first** and re-check every Cargo path dep, `crates/dev/workspace-unify/Cargo.toml`, `Cargo.lock`, and the generated files; crate dirs moved.
Net: ~80 files, **+2,325 / −31,872** (net −29.5k). Native manifests: 61 (the `app_manifests` gate of 40 is untouched).

## State of each piece
- **ui-core**: `MediaGrid` virtualized (bands of cards on the shared `RowSource`/`VirtualList`; 10k test); `ImageViewer.zoom` prop (+ test); `Map` Web Mercator project/unproject, centre, `at` press, `MapPin` lat/long (+ round-trip/centre tests); new `Slideshow` component (frame-clock timed advance, index kept by the view). All compiled once (the `component-catalogue` example built). **Tests have not run** (named check was cancelled at wrap-up).
- **`crates/dev/ui-graph/components.json`**: regenerated from that build (147 components, `Slideshow` added, new props/events). The `tilo-app-window` staleness test is the proof it is byte-exact — not yet run.
- **tl app** (`apps/tl/photos`): written in full (below), **never parsed or checked**. No `app.ids`, no `locales/en-US.ftl`, no `de-DE.ftl`. First command for the next agent: `tolang check apps/tl/photos --write-ids --search apps/tl`, then generate `de-DE.ftl` with `.claude/scratch/gen_de.py` (strict; it lists any English value it lacks a German for), then `tolang fmt`.
- **Deletion**: staged, **held** for full parity (vision + platform capabilities). Inventory below.
- **Walk**: `scripts/headless-examples/app-walks/photos.json` written (rust+tl, PNG fixtures as base64 written at walk time; no binary files committed). **Not run.** After the two sides exist: `t test app-walk/photos`, shots light+dark.

## Open checks (all cancelled/never run at wrap-up)
- `t test test/tilo-ui test/tilo-app-window` (validates the ui-core tests + catalogue staleness) — was queued ~1h+ on a saturated fleet, cancelled.
- `tolang check apps/tl/photos --write-ids` — not run.
- `t cargo test --locked -p tilo-app-package --test app_manifests` — not run (should stay green: 61 manifests).
- Generated fleet/SDK: `t fix app-surfaces` then `t fix sdk-types` (adds `sdk/python/.../photos.py` from the tl surface); `t fix workspace-unify-x86_64`, `t fix lock-update` after rebase.
- Focused clippy on the touched crates; dependents `tilo-server`, `tilo-agent-session`.

## Feature inventory — Rust Photos → tl Gallery, gaps marked
| Rust feature | tl Gallery | Gap |
|---|---|---|
| Library grid | `library`/`library_grid` (virtualized `MediaGrid`) | — |
| Open a photo (chooser), open another | `import_photo` with `opens:` image kinds | — |
| Camera / content handoff | same `import_photo` (takes `File`, optional captured_at/location) | — |
| Import folder / reindex folder | — | **tl-image-caps folder choice** (not yet on main) |
| Refresh imported metadata (EXIF) | — | **tl-image-caps shared image codec** (`read_exif`) |
| View one large | `viewer` (`ui.ImageViewer`) | — |
| Next / previous | viewer Previous/Next buttons | keyboard arrows (no per-photo key action yet) |
| Zoom in/out/actual size | `ImageViewer.zoom` + buttons; scroll/drag in the viewer | — |
| Present / slide show | `present` view + `ui.Slideshow` (frame-clock advance) + `PageSlide` | fullscreen is the shell's; crossfade motion is a cut for now |
| Favorite / unfavorite | `toggle_favorite`, `favorite_selected` | — |
| Share / copy link | `share` (returns the address, shown) | **clipboard capability** (copy) |
| Copy photograph | — | **clipboard capability** |
| Remove / delete (with confirm) | `remove`, `remove_selected`, `split_photo` | — |
| Save photograph (original bytes) | `export_photo` + `file.save` | — |
| Save edited photo | — | **image transform/encode** (tl-image-caps) |
| Rotate left/right, crop to view, reset edits | — | **image transform/encode** (tl-image-caps) |
| Details: name, capture time, folder, place, description | `details_fields` bound inputs | — |
| Details: tags, people | chips + `tag`/`untag`/`add_person`/`remove_person` | — |
| Details: albums a photo is in | toggles in `details` | — |
| Place from map | `ui.Map` geo picker → `set_place` stores lat/long | — |
| Places view | `places` place (filter by place label) | world map of all places not built |
| Tags view / Folders view | `tags` / `folders` places | — |
| Timeline (chronological) | `timeline` place | ordering is library order, not capture-date sort |
| People: rename, merge, split, remove | `Person` resource + `rename_person`/`merge_people`/`split_photo`/`remove_person_resource`; `people_list`, `person` | — |
| People: create from faces (suggestions) | — | **vision service** (`faces`→`cluster(faces)` on `vision.image_model`, published `vision:vision.image-model`) + platform-root read; `Person` type is ready |
| People: approve/reject/hide/restore/merge suggestions, delete local data | — | **vision service** |
| Analyze image (vision description/objects/pets), OCR text | — | **vision service** |
| Library analysis start/pause/resume/retry/progress | — | **vision service** |
| Objects / pets views | — | **vision service** (fields `objects`/`pets` exist) |
| Search (system) | type `search { filter favorite/tags/people }`; each photo publishes its `picture` address for the system index | — |
| Selection: select/all/clear, batch favorite/remove | select mode in `library` + `favorite_selected`/`remove_selected` | range/modifier selection (mode-based today) |
| Pagination (library/albums/people/places) | replaced by the virtualized grid | — (no capability loss) |
| Accessibility acceptance | shared toolkit | Rust manifest scenario removed with the app |
| History / undo | system-provided | — |
| Hosted page (web) | — | **tl page route** (lands after the lead's tl-pages push) |
| Make thumbnails (offload) | — | **`File.thumbnail`** from tl-image-caps for the grid |
| Camera capture destination | the Rust camera launches `so.tilo.photos` (same id) — still resolves | — |

## Deletion inventory (staged, held)
- `apps/photos/` deleted (47 files, 29,861 `.rs`; locales, `surface.toml`, `tilo-app.toml`, tests, scenarios).
- `Cargo.toml` member; `Cargo.lock` photos stanza + dependent lines; `crates/dev/workspace-unify/Cargo.toml` two photo lines.
- `crates/agent/agent-session`: `photos` dep + `web_apps.rs` `photos::definition()`; `crates/server/tilo-server`: `photos` dep + `tests/offloadable_action.rs` fixture moved into the test (self-contained `TestEffect` + `thumbnail_batch`).
- `apps/file-explorer/src/tree.rs` id `so.tilo.lang.photos` → `so.tilo.photos`.
- `dist/dev/nested-session-build.sh`; `tools/screenshots/photos.toml`; `tools/policy/check_atspi_endpoint_contract.py` + test; `tools/binary-budget.toml`; `tools/ci/recorded_build_bytes.json`/`recorded_test_time.json`; `tools/windows/install-apps.ps1`; `dist/apps/registry-metadata.toml`; `dist/apps/app-collections.toml` adds `tl-photos` to `default`.
- `t.toml` app-walk matrix adds `photos`. `app_manifests.rs` untouched (61 manifests; same `apps/*/tilo-app.toml` glob as `os/scripts/pack-apps.sh` and `os/image/compile.sh` — re-check after the move).

## Owner rulings to keep
- Full parity: every Rust feature, every platform; a missing SDK piece is a blocker to report, never a drop. **The next push from this lane is the Photos port**, made by a fresh agent.
- Vision is a system service (rides this push); the app only calls it and shows its resources.
- `tl-image-caps` adds `File.thumbnail` + multi-file/folder choice and moves `read_exif`/`pixels.rs` onto the shared codec; Gallery uses them, and this deletion push carries the thumbnail and folder-choice parts.
- UI library knows nothing of Tilo; one app SDK. No second transport. UX only gets better.

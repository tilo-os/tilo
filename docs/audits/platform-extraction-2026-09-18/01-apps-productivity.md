# A01 — Productivity applications: platform extraction audit

## Scope and method

Scope: `apps/{api-tester,calculator,calendar,clocks,code-editor,contacts,db-client,diffmerge,ebook,hex-editor,notes,pdf-viewer,scratchpad,sheets,slides,snippets,totp,typst-editor,word-processor}`. The audit covers source, manifests, tests, scripts, platform guards, process and storage integration, native libraries, hardware, and host paths. It is static source analysis; dependency implementations and the application framework are outside its scope.

Design alignment: read `docs/architecture/platform-services.md`. All suggested boundaries below should be focused `ui::platform` facade services backed by narrow `crates/system` or `crates/runtime` implementations, reusing current services; never a giant platform crate or a framework dependency on app code. The design is proposed, so API names here remain conceptual.

## Runtime extraction findings

### A01-01 — Per-app host directory discovery (extract; medium)

**Evidence / platform:** Linux XDG conventions are embedded in production persistence: `apps/calendar/src/store.rs:20` (XDG_DATA_HOME/HOME/.local/share), `apps/contacts/src/storage.rs:45` (same), `apps/clocks/src/lib.rs:274` (XDG_STATE_HOME/HOME/.local/state), `apps/totp/src/storage.rs:79` (XDG_DATA_HOME/HOME), `apps/word-processor/src/lib.rs:396` (XDG_STATE_HOME, HOME, cwd recovery fallback), `apps/sheets/src/app.rs:5601` (XDG override then platform-aware dirs::data_dir). Additional already-portable host discovery exists in `apps/notes/src/storage.rs:78` (dirs::home_dir then HOME, appends notes), `apps/scratchpad/src/main.rs:31` (dirs::data_local_dir), `apps/typst-editor/src/app.rs:582` (dirs::document_dir/data_dir).

**Impact:** Data location policy differs across apps; Linux fallback layouts also apply on macOS and other targets. Missing HOME can disable persistence or put recovery into cwd. The dirs-based sites are consolidation opportunities, not intrinsically broken APIs. Notes' visible notes folder is a product/workspace choice and must remain distinct from private application state.

**Boundary:** Platform user locations/app-private storage roots, separated into data/state/cache and user-selected workspace/document locations. Existing `settings-store`, `atomic-file`, `accounts` and `app-state` consumers demonstrate shared services to coordinate with; do not replace serialization with a generic filesystem abstraction.

**Migration:** Supply resolved locations to each store; retain explicit test/launch overrides; preserve old path fallback/migration before changing defaults; keep CRDT schemas and names app-owned. Eliminate cwd fallback for private recovery only after defining explicit unavailable-state behavior.

**Acceptance:** Fake-location tests with no HOME, empty values, Unicode paths, Windows/macOS/Linux location providers; round-trip existing stored documents and recovery copies; confirm storage error behavior survives and no test writes real user data.

### A01-02 — Calendar hardcodes the Downloads folder (extract; medium)

**Evidence:** `apps/calendar/src/store.rs:60` joins HOME with literal `Downloads`, falls back to temp_dir, and `:75` selects the newest .ics there. This is production transfer logic, not just a test fixture. `TILO_CALENDAR_IMPORT_DIR` remains an explicit override.

**Impact:** Relocated/localized user directories and non-HOME environments are not represented; temp storage becomes an implicit export destination.

**Boundary / migration:** Platform known-folder resolution or the existing semantic file-chooser route; keep ICS selection/format handling in calendar. Prefer user-granted import/export handles/paths and retain explicit overrides for automation.

**Acceptance:** Relocated Downloads, no known folder, cancellation, permission denial and successful import/export use the same app actions; no guessed fallback grants.

### A01-03 — Clocks implements authenticated-session transport and private writes (extract backend; high)

**Evidence:** `apps/clocks/src/lib.rs:149` transport module uses XDG_RUNTIME_DIR/tilo/clocks.json (`:159`), owner-mode directory/file creation (`:170`, `:175`), rename-based publishing; `:191` consumes clocks-command.json. Private state repeats Unix permissions and atomic replacement at `:343`, `:353`, `:358`. Callers publish PID-attributed status in `apps/clocks/src/app.rs:1223` and `:1238`.

**Condition / impact:** Unix permissions are cfg guarded, but the XDG transport remains assumed on every platform. Missing runtime dir yields publish failure; non-Unix has no equivalent ACL enforcement in this module. Protocol and shell reader must migrate together; PID/session attribution cannot be dropped.

**Boundary:** Session-scoped activity publisher/command receiver with authenticated producer identity; private durable storage using platform ownership/ACL semantics. Timer/stopwatch state, command semantics and due-notification identity stay in Clocks.

**Migration:** Move the existing file transport intact into the Linux platform implementation first, reuse `atomic-file` where its contract fits, inject activity transport, then add other backends. Coordinate shell path/protocol consumers outside this scope.

**Acceptance:** Production publisher/receiver test proves unauthorized producers rejected, missing service explicit, command version/id matching preserved, exactly-once due notification identity retained, private persistence and restart tested on supported targets.

### A01-04 — Contacts directly speaks a custom notification D-Bus extension (extract backend; high)

**Evidence:** `apps/contacts/src/priority_contacts.rs:14` enables only when TILO_PACKAGE_ID is dev.tilo.contacts; `:23` opens session D-Bus; `:26` binds org.freedesktop.Notifications; `:35` calls XTiloReplacePriorityContacts. `apps/contacts/Cargo.toml:37` declares zbus.

**Impact:** Favorites' notification priority projection relies on the Linux/Tilo notification service wire protocol and launcher environment. Standalone/shots/tests deliberately disable it; do not remove that guard without an equivalent capability.

**Boundary / migration:** Platform `replace_priority_contacts` capability, preserving acknowledgement and retry semantics and authenticated caller attribution. App retains projection of favorite IDs/names/endpoints. Move zbus wire/client code and launcher identity interpretation into platform backend; coordinate shell ancestry verification.

**Acceptance:** Fake acknowledged/failed publisher tests prove retries and no stale acknowledgement; Linux integration proves sender verification and replacement; unsupported platforms explicitly report capability absence without exposing contact data.

### A01-05 — TOTP chooses key custody using secret-tool availability (extract backend; high)

**Evidence:** `apps/totp/src/storage.rs:232` probes secret-tool --version with bounded-command and chooses SecretService or LocalFile. `:248` migrates a local key only after Secret Service store/readback succeeds; `:298` and `:323` implement secret-tool lookup/store. `apps/totp/Cargo.toml:23` declares bounded-command. Unix private-permission assertions at `storage.rs:567` are test-only.

**Impact:** Native credential stores on non-Secret-Service hosts are not selected; absent helper takes local-key custody. Existing code correctly refuses fallback on a detected service's lookup failure, and that distinction must survive extraction.

**Boundary / migration:** Platform secret custody contract distinguishing unavailable, locked/denied, missing, corrupt and available; Linux Secret Service backend can initially retain bounded-command. Keep encryption format, TOTP algorithm and vault migration in app/domain code, but place secure key persistence/access rules behind custody adapter. Coordinate existing accounts vault rather than invent a second shared credential policy.

**Acceptance:** Verify missing helper versus denied service, local-key migration with readback, readback mismatch retains original key, no secret in logs/process arguments, native-store restart and explicit unsupported-host behavior. Never silently downgrade on access denial.

### A01-06 — Code editor owns Unix subprocess-group lifecycle (extract backend; high)

**Evidence:** `apps/code-editor/src/lsp.rs:28` imports Unix CommandExt; `:556` spawns server with piped stdio/current project directory; `:569` creates process group; `:895` Drop sends negative-PID SIGKILL at `:904` then kills/waits direct child. `apps/code-editor/Cargo.toml:57` gates libc dependency on Unix.

**Impact:** Deliberate Unix implementation has non-Unix direct-child fallback, not equivalent descendant cleanup. App also owns executable resolution by PATH. No compilation/runtime failure is asserted.

**Boundary / migration:** Managed child-tree capability with cwd, argv, stdin/stdout, bounded termination/reaping and descendant ownership; Unix process group and other OS job/container implementation belong in platform. Keep LSP wire protocol/server mapping and editor errors in app. Adjacent terminal dependency (`apps/code-editor/Cargo.toml:31`) should share lifecycle conventions with its independently audited backend.

**Acceptance:** Real helper spawning a descendant proves close/restart cleans both on every supported platform; stdio and non-ASCII/space project paths work; launch failure and missing executables remain actionable; no broader process signalling.

### A01-07 — Ebook equates sharing with a Linux mount path and POSIX path-list syntax (extract; high)

**Evidence:** `apps/ebook/src/content.rs:41` defaults workspace to /var/workspace, `:55` resolves override, `:69` classifies paths by lexical prefix, and `:404` parses EBOOK_LIBRARY_DIR with split(':'). The sample shared path in `apps/ebook/src/app.rs:1552` is separate demo data.

**Impact:** Collaboration location and shared/local semantics depend on a mount layout. Colon splitting cannot represent Windows drive paths; prefix classification is a filesystem-layout heuristic, not platform capability proof. Existing lexical normalization prevents ../ escape but is not symlink-aware.

**Boundary / migration:** Workspace content-location/capability service supplies shared root and authoritative classification; retain content IDs/book layout in reader. For environment compatibility use std::env::split_paths or an explicit structured list. Do not grant sharing based only on an app-supplied arbitrary root.

**Acceptance:** Shared/nonshared, lexical escape, symlink, relocated mount, Windows drives and multi-root cases; explicit unavailable workspace; migration preserves content-addressed EPUB lookup and local grants.

### A01-08 — PDFium dynamic library discovery is Linux-centric (extract loader backend; medium)

**Evidence:** `apps/pdf-viewer/src/source.rs:2514` first honors TILO_PDFIUM_LIB, then searches /usr/lib64, /usr/lib, Debian x86_64 path, /app/lib and HOME/.local/lib all using libpdfium.so (`:2522`), finally calls bind_to_system_library (`:2539`). `apps/pdf-viewer/Cargo.toml:37` declares pdfium-render.

**Impact:** Platform-specific search policy is in the document engine. The system-library fallback may work elsewhere, so this is not proof PDFium cannot load on macOS/Windows. Existing pure-Rust extraction fallback remains useful.

**Boundary / migration:** Platform native-library locator/loader policy returns binding or reason unavailable; preserve explicit override, version/ABI requirements and fallback. PDF editing/rasterization stays in PDF source worker, not platform framework.

**Acceptance:** Explicit override, packaged backend, native loader search and missing/mismatched library tests on supported architectures; prove real rasterization/edit behavior when backend required rather than silently accepting text-only fallback.

### A01-09 — PDF save requires directory fsync with ordinary File::open (extract durable-file operation; high)

**Evidence:** `apps/pdf-viewer/src/source.rs:128` creates private sibling temp directory; `:145` flush_private_save opens/syncs temporary file, then unconditionally opens/syncs directory at `:150`; `:164` renames. Only destination-parent directory sync at `:166` is best effort despite its cross-target comment.

**Impact:** Opening directories and replacing destinations have host-specific semantics. Private staging directory sync is a required success path even on targets without that operation. Static portability concern; not a measured Windows failure.

**Boundary / migration:** Platform private staged-file publication with exclusive/private directory, durable flush policy and replace semantics. Coordinate existing atomic-file crate; its usual bytes-based write may need a staged-file API because PDFium writes by pathname. Do not lose symlink/collision defenses when consolidating.

**Acceptance:** Replacement of existing PDF, permissions/ACL, staging collisions, sync errors, cleanup and no partial destination on failure; directory-sync unsupported policy explicitly tested; PDFium real pathname writer exercised.

### A01-10 — Word processor guesses toolkit drag payload ownership (extract ownership capability; high)

**Evidence:** `apps/word-processor/src/widget.rs:2406` removes dropped file after successful insert when is_dnd_spool_path says true. `:12864` recognizes name prefix winit-dnd- plus location under XDG_RUNTIME_DIR/XDG_CACHE_HOME/HOME or temp_dir (`:12877`).

**Impact:** App knows backend spool naming and Linux fallback directories. Naming plus a broad HOME prefix does not itself prove that the toolkit owns a file; a user-selected file can share the pattern. This is a source-level ownership ambiguity, not a reproduced deletion incident.

**Boundary / migration:** Drag payload exposes owned temporary lease or explicit release operation; backend owns cleanup, app consumes bytes/handle. Coordinate excluded framework/vendor owner without editing them as part of this audit. Preserve user files regardless of name.

**Acceptance:** Real user file named winit-dnd-* under HOME/temp remains untouched; genuine spool is cleaned once after success/cancel; malformed drops and repeat consumption cannot delete arbitrary files.

### A01-11 — Word processor discovers locale from POSIX environment itself (extract discovery; medium)

**Evidence:** `apps/word-processor/src/i18n.rs:83` selects LC_ALL/LANGUAGE/LANG; `:96` parses POSIX syntax; `:134` initializes ACTIVE. Totp and Slides already consume framework I18nManager (`apps/totp/src/i18n.rs:31`, `apps/slides/src/strings.rs:42`).

**Impact:** Native OS/app language preferences not represented unless environment is set; an app-local language source can disagree with system locale.

**Boundary / migration:** Platform preferred-language list feeds existing shared i18n service. Keep catalogs/Fluent formatting in app and reuse its service-facing registration pattern from Slides/TOTP. POSIX parsing belongs in Linux environment backend if still needed.

**Acceptance:** Native language preference without LANG, explicit app override, POSIX C/encoding/modifier/list handling and fallback; system locale changes propagated consistently.

## Tests and target-specific packaging

### A01-12 — Platform-dependent test fixtures need capability-specific verification (test-only; medium)

**Evidence:** `apps/code-editor/src/lsp.rs:1172` launches unguarded true; URI tests at `:1141` use Unix absolute paths. `apps/code-editor/tests/language_server.rs:89` resolves PATH entries by literal name, omitting executable suffix rules. `apps/code-editor/src/editor_view.rs:3172` clipboard test returns success when clipboard unavailable (`:3177`), explicitly prints skipping. `apps/pdf-viewer/src/app_events.rs:3050` derives paste assertions from actual clipboard state and accepts absent backend; it does not prove native clipboard works. `apps/pdf-viewer/src/source.rs:2841` and `apps/word-processor/src/lib.rs:466` symlink fixtures are correctly cfg(unix)-gated.

**Additional file coverage:** XDG environment fixtures: `apps/contacts/src/app.rs:13156`, `apps/contacts/src/app/tests.rs:2817`, `apps/contacts/src/main.rs:280`, `apps/sheets/src/app.rs:7873`, `apps/slides/src/framework_history_tests.rs:455`, `apps/typst-editor/src/framework_history_tests.rs:151`, `apps/notes/tests/framework_history.rs:481`, `apps/word-processor/src/widget/tests.rs:3774`. Absolute /tmp diagnostic images occur in `apps/pdf-viewer/src/app.rs:5482`, `:5628`, `:5812`; complete path matches appear in companion list.

**Migration / acceptance:** Replace true with a controlled test helper; use real executable resolver; retain native tests that fail clearly without declared prerequisites, plus injected adapters for unit tests. Supply application storage roots directly rather than relying on Linux environment redirection on every platform. Make native clipboard proof require and assert backend availability, not silent pass; keep absent-backend behavior separately named/tested. Temporary output should use temp_dir/TempDir and be scoped. String-only /tmp paths in action serialization fixtures do not need wholesale refactoring.

### A01-13 — Linux packaging and macOS bundle tooling are intentional target backends (retain target-specific)

Every scoped `tilo-app.toml` declares Linux namespaces/seccomp/Landlock; most also x86_64 v3 ISA. These are deployment policy, not portable runtime logic. Do not weaken them to permit a new target. Relevant sandbox anchors by file:

| apps/ directory | tilo-app.toml sandbox anchor |
|---|---:|
| api-tester | 29 |
| calculator | 30 |
| calendar | 33 |
| clocks | 17 |
| code-editor | 40 |
| contacts | 25 |
| db-client | 31 |
| diffmerge | 32 |
| ebook | 40 |
| hex-editor | 33 |
| notes | 24 |
| pdf-viewer | 35 |
| scratchpad | 23 |
| sheets | 35 |
| slides | 32 |
| snippets | 31 |
| totp | 33 |
| typst-editor | 31 |
| word-processor | 34 |

`apps/scratchpad/qos-app.toml` is additional legacy package metadata. `apps/scratchpad/build-macos.sh:4` intentionally rejects non-Darwin, builds Contents/MacOS and Info.plist (`:13`, `:15`), and ad-hoc codesigns (`:28`). `apps/scratchpad/README.md:11` documents this. Keep the script behind macOS packaging, optionally consolidate into a shared bundle builder with app-specific metadata. All 19 Cargo manifests contain optional runtime-dylib thin linkage; the comment naming .so in `apps/word-processor/Cargo.toml:21` is descriptive, not a hardcoded loader.

**Acceptance:** Linux packages retain exact effective isolation/capabilities; other targets have explicit packaging/ISA/capability translation and reject unsupported required protection rather than disabling it. Bundles launch with correct identifier/resources and signing policy. Manifest comments that predate current networking/editing behavior (e.g. DB client display-only claim, hex no-save claim) are documentation drift; effective launcher permissions require adjacent packaging audit before claiming runtime mismatch.

## Reviewed non-findings and reuse boundaries

- `apps/api-tester/src/http.rs` and Calendar CalDAV use reqwest/Rustls; DB engines use rusqlite bundled/Postgres/MySQL. Protocol implementations are app/domain behavior, not OS adapters merely because they use sockets/native-linked bundled SQLite. No direct OS trust-store policy found here.
- All apps use winit/vello/framework types; that alone is not leaked native backend implementation. Existing ui::portal, clipboard, text-selection, fullscreen and input abstractions should be reused, not reimplemented per app. PDF pinch comments in `apps/pdf-viewer/src/app_events.rs:506` describe backend events; Slides fullscreen comments (`apps/slides/src/app.rs:3910`) do not directly invoke Wayland.
- `apps/calendar/src/timezone.rs` and Clocks chrono-tz rules are portable domain data. Current time/chrono::Local observations can be injected for determinism, but do not warrant extracting the IANA calendar algorithms. `apps/typst-editor/src/compile.rs:131` uses embedded typst-assets fonts; `/main.typ` is a virtual document ID, not a host root file.
- `apps/ebook/src/local_library.rs` and `apps/sheets/src/settings.rs` already delegate shared preferences to settings-store; Calendar accounts delegates to accounts. Audit actual backend internals in those services separately, not here.
- `apps/code-editor/src/sample_project.rs:150`, `apps/snippets/src/seed.rs:81` and `:213` contain shell/Linux example text, not executed platform adapters. Demo `/var/workspace` and test-only `/etc/passwd`/`/tmp` serialization literals are not evidence of production filesystem access.
- Ordinary std::fs reads/writes, Rust threads, PathBuf usage, tempfile and cryptographic getrandom do not by themselves merit extraction. Code editor source-file saving, calendar/contact/notes persistence already reuse atomic-file; audit its native implementation in its assigned shared-service scope.

## Handoff summary

Thirteen grouped findings cover eleven runtime platform boundaries, test verification, and intentional packaging. Highest-priority contracts are credential custody, authenticated activity/contact transport, child-tree ownership, staged durable file publication, and drag-payload ownership. Coordinate adjacent shell readers, shared storage services, the terminal process backend, packaging, and PDFium deployment.

# Complete browser UX — implementation status

This is an in-progress status report. It does not certify completion of the full UX scope.

## Technology and current code

The browser uses a Rust shell (Vello/wgpu, winit and Taffy) with a separate C++ CEF helper. The pinned runtime is CEF 152.0.6+g708dc14 / Chromium 152.0.7977.83, using the prebuilt SDK. Built-in adblock-rust filtering covers native requests and cosmetic/scriptlet rules. Content is transferred to the Rust renderer as CPU image frames. Up to 32 normal tabs share a helper/profile with separate CEF browser instances.

## Requirement status

| Requirement | Current evidence and remaining work |
| --- | --- |
| Symmetric navigation history menus | Implemented and reviewed in `3b1f1243f`; live Back/Forward menu selection verified. Final integration still includes address-field synchronization. |
| Redesigned tabs | Implemented in `ac81de14e`, `35f4cf2f2`, `09c998a2d`; six review findings resolved. Live favicon, media/mute, accessible new/switch controls verified. Final integration must retain drag and download-close behavior. |
| Session recovery | Implemented and reviewed in `7ce424b9e` and `1b8ade978`; visible pointer and keyboard recovery choices passed. Actual clean restart, forced crash, Restore/Start fresh, lazy loading, lock contention and draft exclusion passed on isolated profiles. |
| Per-site blocker panel | Implemented in `3af55beab` with narrow-render followup `478634a8e`; offline timestamp-restoration fix `24f716f3a` passed scoped review. Native exact-origin/all-layer/count tests and live exception/persistence controls passed. |
| Site information | Implemented; camera/microphone permission-policy fix `24f716f3a` passed scoped review. Live certificate panel and pointer-scoped clear verified; fake-camera/microphone grant, revoke and origin-isolation regression passed. |
| Private windows | Private isolation and cleanup fixes are implemented and reviewed. Shared-engine frame matching correction `7c0191dc5` passes 20 WebView tests and scoped review. Final two-launch acceptance on optimized build `e3a9f95e` failed: one launch rendered correctly, but another retained the initial 1280×800 frame and displayed blank content despite Retina resize requests. Startup command/paint diagnosis continues. Both windows closed with app/helper/root removed and all 371 normal-profile files unchanged. |
| Download lifetime and exit choices | In progress. Basic native lifetime regression passed: an accepted transfer completed with exact bytes after its tab detached, and another tab could cancel a transfer. Rust tests passed for download metadata/status and the actual Keep/Cancel button action path. Native retry/POST/failure coverage, final review and live browser acceptance remain pending. One prior native run timed out before its first download prompt and during shutdown; that intermittent failure remains unresolved. |
| Fullscreen | Pending native-window integration, hidden chrome and safe exit handling. |
| Keychain credentials and basic autofill | Pending implementation. Synthetic Keychain storage feasibility checked; no user passwords read. |
| Startup and window performance | Pending fix and complete measurements. A real cookie-load stall was sampled inside the macOS Keychain read path; its service-side cause remains unresolved. Clock correlation feasibility was measured, but content-response-to-drawable tracking remains absent. |

## Verification and build time

Session checkpoint: 160 browser library tests passed (1 ignored), 2 binary tests passed, 24 focused Chromium tests passed. The latest incremental debug build took 3.84 seconds; earlier session builds included shared Cargo lock waits. This is not a clean-build measurement. Existing optimized-build and SDK setup observations are in [the baseline overview](2026-09-16-browser-pages/browser-state.md).

The session tests use `--app-root` to isolate app data, native storage and the recovery snapshot. They verify that only the selected restored page loads, background tabs load on first selection, switching tabs retains document state, closing a tab persists, and an unsent address draft cannot replace its committed URL. Process death preserves the recovery prompt; ordinary window close marks the session clean.

## Existing three-page measurements

These are the earlier engine-level benchmark results, not measurements of the final UX build:

| Page | Fresh load | Warm loads | Median observed scroll response |
| --- | ---: | ---: | ---: |
| Wikipedia Main Page | 0.80 s | 0.34 / 0.25 s | 14.1 ms |
| Hacker News | 0.32 s | 0.16 / 0.15 s | 20.3 ms |
| YouTube nature search | 9.75 s | 1.46 / 1.44 s | 8.9 ms |

Scroll response ends at the observed document scroll event. CEF delivered approximately 59–60 CPU frames/s in those runs; this excludes Rust-window presentation. YouTube video playback is not the benchmark workload. Raw runs and failures remain in [the benchmark audit](2026-09-16-browser-pages/README.md).

A separate preliminary 10.121-second observation of two paused local tabs showed no CPU-time increase at `ps` precision. Window visibility was uncontrolled, so it is only a background-idle observation. It does not replace the required final controlled idle and three-page measurements.

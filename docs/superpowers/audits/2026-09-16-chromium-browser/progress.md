# Chromium browser implementation evidence

Status: in progress. Reaching Podesta is verified; the complete browser capability
integration and GPU transport gates remain open.

## Chosen technology

- Existing Rust browser UI, winit, Vello/wgpu, and Taffy.
- Sandboxed native CEF helper: CEF 152.0.6+g708dc14,
  Chromium 152.0.7977.83. SDK archives are pinned in scripts/browser/cef-lock.json.
- Bounded framed IPC, latest-frame BGRA→RGBA CPU transport. This is not GPU texture
  sharing; no zero-copy or hardware-frame-transport performance claim is made.
- Native adblock-rust 0.12.1 matching for network requests, cosmetics, redirects,
  and scriptlets. Pinned resource bundle includes 214 entries with upstream source
  provenance/licenses. Custom document-start scripts use acknowledged CDP
  registration before navigation.

## Build and launch

From the repository root:

```sh
python3 scripts/browser/package.py
```

macOS output: target/browser-package/Browser.app. Linux output:
target/browser-package/qos-browser/browser. `--skip-native-build` reuses an already
built helper; `--profile dev` selects an unoptimized Rust build. Runtime discovery
uses the packaged helper; QOS_CHROMIUM_HOST and QOS_CHROMIUM_PROFILE are explicit
local development/test overrides. Missing runtime produces a visible error,
never simulated browsing.

## Real application evidence

On macOS ARM64, launched the optimized packaged Rust application with an isolated
Chromium profile. Through its native UI:

1. Navigated to https://podesta.ai/.
2. Observed title “Podesta AI — The operating system for the AI age” and rendered
   page content “We’re tinkering with the operating system for the AI age.”
3. Zoomed the Retina window: content resized correctly.
4. Navigated to https://example.org/, observed rendered “Example Domain”.
5. Used Back and observed Podesta URL/title again.

Independent native smoke checks additionally verified HTTPS HTTP 200, nonempty
BGRA frames, a closed event and clean process exit for Podesta, both standalone
and nested inside the Rust app package. macOS strict signature verification and
framework symlink checks pass across repeated builds. Developer ad-hoc signing is
not release notarization.

Linux ARM64: CUA inspection of the existing Fedora 44 VM now visibly shows the
actual Rust browser address `https://podesta.ai/` and the same Podesta headline.
This run selected Mesa lavapipe software Vulkan explicitly for the Rust UI; CEF
sandboxing and HTTPS validation remained enabled. It verifies real Linux page
rendering, but does not demonstrate hardware GPU acceleration.

## Measurements so far

- First optimized Rust thin-app profile build with dependencies missing from that
  profile’s cache: 190.75 seconds wall time. This is not a clean-checkout build or
  CEF source compilation; CEF is a downloaded binary SDK.
- Subsequent unchanged Rust build: 0.50–0.86 seconds, measured during packaging.
- At a zoomed Podesta page after visiting DuckDuckGo, Podesta, Example and Back:
  Rust UI process RSS about 199 MiB; aggregate browser/helper process RSS about
  1,459 MiB. Aggregate RSS counts shared pages more than once and includes retained
  renderer processes; it is not unique physical memory. One observation only,
  not a steady-state benchmark.
- Linux ARM64 Rust optimized browser build: 6 minutes 7 seconds, two build jobs
  in the existing Fedora 44 VM. Native CEF host compiles after architecture fix.
  Linux native runtime qualification passes; actual Rust browser window visibly renders Podesta with software Vulkan in the VM.

## Verification completed

- Browser library regression suite: 101 passed.
- Rust Chromium transport: 15 passed; independent scoped review passed.
- Rust blocker: 9 passed, including bounded FFI, poison failure, redirects/scriptlets.
- Native blocker/document-start fixtures: 11 passed on Linux; subsequent late-script CSP fix passed six targeted macOS checks and independent review.
- Native keyboard/Retina/script-boundary fixtures: 4 passed.
- Native protocol CTest: passed.
- Pinned blocker resource regeneration: exact reproducibility check passed.

## Open gates

- Native-blocker review findings on subdocument source attribution and strict-CSP
  cosmetics are fixed and independently approved.
- Procedural cosmetic actions and earliest initial filter-scriptlet execution
  remain limitations. Do not claim universal uBlock compatibility.
- Linux packaged distribution and hardware rendering qualification.
- Platform GPU texture transport and synchronization tests; current path is CPU.
- Downloads, explicit permissions, MV3 extension capability fixtures, actual
  clear-on-exit, and remaining everyday browser integration.
- Broader final review and updated performance measurements after integration.

# Shared top-bar editor qualification

The production baseline at 27ab22da339c903e7946b0cc122163646319f9a5 overlaps
the editor title, hint and Remove controls at 200% text. Its actual Widget
Escape journey leaves drag capture active, and keyboard removal leaves focus on
a vanished action. Both behavioral regressions were run and failed on the baseline.

The editor now paints shared Small ButtonFace and ListRowFace components.
Measured typography reserves independent title, wrapped hint and row space;
the trailing action reserves its shared intrinsic width. Add controls wrap within
the output. The last Remove is visibly disabled and never starts a drag.

Pointer activation waits for release inside the same control. Escape, outside
dismissal, focus loss and geometry changes cancel pending input. A retained submit
key consumes repeats and release even after Escape; focus follows the same widget
between Add and Remove. Secondary click, Control-click, the context-menu key / Shift+F10 and
the public topbar:editor:open semantic action share one opening path. The existing
typed ChromeIntent persists the normalized widget list. The catalogue exports topbar.editor.open as an existing View action. The shared
modal-capture/focus-trap declarations route popup input through the native host.
No policy, authority or
security setting changes.

Theme and ordered-widget changes invalidate the retained scene; unchanged themes retain the cache.
Both retained-scene regressions compare actual rendered pixels. Hidden layout
closes the popup and releases capture; the catalogue withholds opening while
client fullscreen hides the bar. Both lifecycle regressions failed on the
first candidate before these fixes. A rendered pixel comparison observes the focus ring at the actual shared button.

## Original captures

Twelve baseline and twelve candidate originals cover dark, light, high contrast,
200% text, 150% UI / 130% text and a 320px output, with all widgets and Add states.
These paint the complete production TopBarChrome; the empty surroundings are
fixture space. Checksums and source blobs are recorded in artifacts.json.
Representative unmodified captures are attached to the ready PR and anonymously
downloaded to verify content, dimensions and checksums.

The 320px primary bar still collides above the corrected editor. That remains an
explicit next chrome increment; these captures do not claim an app-filled desktop
or VM qualification. Integrated QA owns the separate installed isolated VM.

## Verification

Focused actual Widget journeys exercise keyboard and pointer activation,
repeat/release ownership, semantic opening, stable focus, disabled controls,
single reorder commits, Escape/outside/focus/resize interruptions and theme cache.
The shell integration journey verifies native host opening, repeated Escape and
resize and visible open/closed pixels. The isolated real compositor example
(scripts/headless-examples/topbar-editor.py) positively checks catalogue/invocation,
pointer cancellation/removal, keyboard re-add, held submit/Esc/release and cancelled
reorder, then verifies the process, socket and scratch profile are removed.
Cargo and software Vulkan rendering run only on the SSH runner.

## Catalogue follow-up

CI run78158 caught an isolated shell catalogue regression: desktop translation
keys were not registered in ActionsProvider's unit-test host. The action now
owns dedicated English/German search keys. The existing all-action copy assertion
is retained and runs both locales, including exact translated editor title and
subtitle. The real compositor journey checks that same title/subtitle before
invocation. Fresh v2 captures and their source blobs are indexed separately in
artifacts.json; the initial rendered candidate6571dd51 and its originals remain
unchanged.

The locale files overlap Dictation #2974 at file level, with independent hunks:
these two entries sit beside overview actions, while Dictation adds dictation
messages near the end. Messages #2881 and Files #3009 have no locale overlap.
The example now rejects an overlong Unix socket path before spawning a process.


## Original editor and responsive-bar reconciliation (2026-10-10)

The composed delivery retains exact authored PR3386 head
83c0fa1074e06e70b7e5df2308e866d2a8c5eb7c and PR3446 head
c6433af3dc39344f74a1d3d5289d65e331dc9457 as ancestors, against main
c83f787da3ad3836547eae8521e29b24b8af8c54. The historical 320px primary
collision described above is addressed by the responsive Toolbar in this combined
source. Original capture manifests remain historical evidence, unchanged.

Current main's shared placement helper is retained alongside editor resize and
hidden-layout cancellation. Both editor and Toolbar contribute popup capture,
focus trapping and retained-scene invalidation. The editor owns input while open;
opening either original semantic editor target closes the Toolbar menu first.
A new actual narrow production regression reproduced a rejected editor target
before this dispatch repair, and covers focus-loss drag cancellation and Escape.

All 23 original regression names and assertions remain. Twenty-two complete test
bodies are unchanged. The held-key test alone adapts synthetic event construction
to main's current KeyEvent API while retaining its actual Framed host path,
native certification, repeat mutation and every original assertion.

Fresh production offscreen editor and responsive-row/overflow matrices cover
dark, light, high contrast, 200% text, combined UI/text scaling and 320px width.
These use the remote software Vulkan adapter and fail if they cannot render.
They qualify fixture production chrome, not an installed VM or hardware display.
The combined delivery's PR carries inspected original PNGs and an exact-source
qualification receipt. Protected CI and integrator exact-main checks remain
separate gates; neither original request is closed based only on these captures.


The combined 320px scaling captures exposed another original-editor limitation:
labelled Remove buttons could leave only an ellipsis for widget names. Measured
constrained rows now reuse named shared icon-only Remove faces and shared
two-line titles, prioritizing complete names over the decorative drag grip.
The existing row drag gesture and semantic Add/Remove targets remain unchanged.
The strengthened combined capture regression fails on elided widget names and
passes with complete names at 320x600 under 200% text and combined 150% UI/130%
text. Before and after originals distinguish this repair from earlier wide
captures; narrower heights are not claimed by these combined-scale captures.

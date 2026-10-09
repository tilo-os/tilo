# People panel qualification

This increment keeps the existing People panel usable on short outputs and with
enlarged text. It preserves the status-lifecycle source from #3736 as an ancestor
of the current-main delivery branch; it does not rewrite that authored commit.

## Acceptance checklist

- Shared ScrollRegion bounds the viewport and exposes wheel, scrollbar,
  keyboard and semantic scrolling. Home/End reveal the selected control.
- Paging retains the pager selection, including a usable Previous fallback on
  the final page. Follow continues to resolve the painted stable identity.
- FieldFace and TextInput render the host's actual status selection and caret.
  Pointer re-entry keeps the draft; pointer drag and physical-scale Unicode
  editing use the shared input. Keyboard/IME edits still use the host editor.
- Shared control measurements, SegmentedFace partitioning and ListRowFace
  grow targets with text/UI scale. Sharing choices stack when needed.
- Disclosure and unavailable text wrap in measured bands. Empty/error content
  no longer overlaps the status editor; inferred status retains its source cue.
- Completed-click, repeat/release, Esc, hidden-editor, focus-loss and resize
  behavior retain production assertions. Resize ends an old scrollbar drag.
- Visible scroll-fade demand participates in host frame/deadline scheduling.
  Hidden People does not contribute a deadline. The library caret is static.

## Verification

Focused commands using Cargo on the remote builder:

~~~sh
cargo test --locked -p shell --lib people_
cargo test --locked -p shell --lib roster_follow_
cargo test --locked -p tilo-desktop --lib collaborators::
cargo clippy --locked -p tilo-desktop -p shell --all-targets -- -D warnings
cargo check --locked -p shell --tests
~~~

The original End-reachability and pager-focus regressions fail on the inspected
main-plus-preserved-source base. A reopening regression also reproduced the new
scroll path's dependency on fresh pointer movement; its assertion is retained.

Fixture-only screenshots call the production component with 13 participants and
exercise 1366×768, 1024×600, 640×480 and 640×360 viewports, light/dark palettes,
125% and 200% text, 125% UI scale, top/End focus, selected status text, inferred
status, empty roster and unavailable states. Representative original PNGs and
SHA-256 qualification are attached to the delivery PR. The before captures retain
their original revision and Library identity; no baseline PNG is refreshed.

The library's existing static-caret behavior is preserved. Timing qualification
observes the shared scrollbar's actual idle-fade contract rather than adding a
new blink timer. Captures use remote CPU offscreen rendering; native tests drive
real winit events through ShellOverlayHost. Installed VM/DRM acceptance remains a
separate gate and is not claimed here.

## Boundaries and remaining work

No credentials, grants, signing trust, collaboration/security policy, microphone
or account sends change. Files #3009 transfer/broker/undo paths remain untouched.
The shared overlay_host file overlaps other shell owners, but this increment is
confined to People routing/model and visible-child scheduling.

Normal protected CI/landing and exact merged-main checks remain integration
gates. Broader primary-bar collisions/overflow, remaining launcher loading/app
launch, mixed-window overview/chrome, additional dialogs and installed-device
states remain in the surface inventory.

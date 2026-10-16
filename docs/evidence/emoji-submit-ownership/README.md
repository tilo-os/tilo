# Emoji chooser submission ownership

Base: `5e6e5702e0237ba582a1f388c41e9d00e547fbd6`.

Holding Shift+Enter opened the tone chooser, then its repeated Enter submitted
the default form and closed the picker. A repeat received after reopening also
queued a pick without a new press. Both cases reproduce through the public
production `handle_event` API on unchanged source.

The picker now ignores repeated Enter in its result and tone modes. Repeated
arrow navigation remains available; a fresh Enter submits exactly one chosen
form. Nested Escape cancels the tone row before dismissing the picker. Existing
emoji assertions remain unchanged. No application receives fixture commits,
and no real recents store is connected.

`examples/emoji_picker_submit.rs` captures the actual chooser before and after
a repeated opening key in dark, light, high contrast, 200% text and combined
150% UI/130% text themes. Its before expectation positively observes the
unwanted queued commit and closed state. Its after expectation requires an open
chooser and no queued commit. The opened frames must remain byte-identical
across the repair. No visual baseline is replaced.

Commands, run only on the authorized remote CPU runner:

```sh
cargo test --locked -p tilo-desktop --lib emoji_picker::
cargo run --locked -p tilo-desktop --example emoji_picker_submit -- OUT before
cargo run --locked -p tilo-desktop --example emoji_picker_submit -- OUT after
cargo clippy --locked -p tilo-desktop --all-targets -- -D warnings
cargo check --locked -p shell --tests
```

CPU offscreen captures use Vulkan and the Mesa CPU ICD exclusively; absence of
a renderer fails. Native host source forwards the event stream to this component;
installed application insertion and physical input are separate acceptance gates.

Known retained #3534/#3891/#3314/#4118 file scopes contain no emoji path. Current
first-two-page UI owner-title inspection found no emoji owner. #3710 owns Quick
Settings details and has no edited-path overlap; Devices/Clipboard ownership
was left alone. This bounded inspection is not an exhaustive ownership claim.

The original desktop inventory stays open: tiny-output picker layout,
native launcher loading/sandboxed launch, further chrome/menu/dialog states,
narrow status summaries, dense workspace headers, previews and installed-device
acceptance. #4269 and #4276 remain separate ready integrator handoffs. Existing
security/runtime denials stay stopped; no draft/WIP/hold workaround or protected
merge bypass is used.

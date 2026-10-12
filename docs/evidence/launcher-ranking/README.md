# Stable launcher ranking

A provider revision can insert or reorder results while search stays open. The shell now preserves the selected provider/resource identity through the shared CommandPalette, replaces callbacks with the current provider results, and resets selection normally when the query changes or the chosen resource disappears.

Baseline production 3ae68f6a5792af796f903b688fd3f59640bee8cf fails all four regression cases: Gamma replaces the keyboard-selected Beta. Corrected production 2a3b4b862198a90faec7f6c15882958a2c5afe6e passes all four, including keyboard activation of the refreshed callback, query correction, disappearance, cancellation and pointer activation after resize. Five recovery cases and existing palette routing pass; shell tests compile and all-target shell/UI Clippy passes.

Six inspected original PNGs cover before/after dark, light and true 200% text. After a ranking update, Beta remains selected and visible, including at 800×700. artifacts.json records source revisions, dimensions and SHA-256. PNGs are published as PR attachments rather than committed binaries.

These are controlled provider fixtures exercising the production registry, keyboard/pointer routing, layout and paint. They do not prove a real sandboxed application maps successfully. The prior exact-merge private live search/Escape journey passed. VM/device verification remains blocked by unavailable KVM and the configured VM refusing SSH; no permissions or configuration changed. New native Library saves remain unavailable; the stopped upload returned no receipts. Original older Library identities are preserved.

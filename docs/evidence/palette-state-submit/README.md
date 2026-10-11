# Shared palette submission across changing states

Base: `1251dfd7371855a4b2755cc49de79a2e131f3d46`.

The reusable CommandPalette accepted repeated Enter events in both its result and
argument modes. Holding Enter over an empty result list could activate a result
that arrived later. Holding the press that opened an argument could submit its
prefilled value immediately. Native shell search already guards its outer Enter
handler; this repair belongs to the reusable component and its other hosts.

The component now consumes repeated Enter before either submission path.
Arrow-key repetition remains available. No layout, painting, theme, action
address, permission or host-observation changes are involved.

Two new production-widget dispatch journeys fail on unchanged production source
at the exact base. They cover empty-to-loaded results, repeated submission,
repeat navigation, reopening, prompted arguments, fresh confirmation and Escape
cancellation. Existing palette tests remain unchanged. Qualification runs on the
owned remote CPU runner, never the user's desktop.

Commands:

```sh
cargo test --locked -p tilo-ui --lib widget::command_palette::
cargo test --locked -p shell --lib launcher_recovery_tests::failed_palette_keyboard_invocation_keeps_context_for_retry_and_escape
cargo clippy --locked -p tilo-ui --all-targets -- -D warnings
cargo check --locked -p shell --tests
```

PR #3891 owns draw inspection and physical latency observation in the same
component. Its file overlap is recorded in
[the scope comment](https://code.tilo.so/tilo/tilo/pulls/3891#issuecomment-50445);
those hunks remain untouched. This is a scope notice, not an owner agreement.

The original desktop request remains open. The broader inventory in
`docs/desktop-surface-adoption.md` and PR #4269 still tracks launcher loading and
real sandboxed launch, chrome/menu/dialog states, narrow status summaries,
dense workspace headers, preview transitions and installed-device acceptance.
This slice repairs one shared input-state gap; it does not establish those
remaining outcomes. Previously delivered #3760, #4169 and #4180 remain closed
work. #4269's resize work is a separate ready handoff.

No new screenshot is claimed for an event-only change. Representative visual
evidence belongs to the preceding visual increments; these dispatch receipts
prove the submission lifecycle directly. Installed-device, DRM/VM and physical
acceptance remain separate owner gates. Existing permission denials remain
stopped; no draft/WIP/hold workaround, direct merge or protection bypass is used.

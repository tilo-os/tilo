# Native UI Input Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep the shared headless runner's keyboard conventions and discovered devices consistent with live input on macOS and Linux, and compile native UI window setup without Darwin-only warnings.

**Architecture:** Reuse the existing `InteractionEnvironment::with_pointer_modality` and `with_touch_modality` transitions. Platform expectations are explicit test inputs; OS-specific window attributes remain target-gated. This is one prerequisite of the full shell port, not its completion.

**Tech Stack:** Rust, winit 0.30, shared `ui`/`qos-app`, nextest; Apple Silicon macOS and Fedora ARM64.

**Spec:** `docs/superpowers/specs/2026-09-12-macos-shell-port-design.md`

## Global Constraints

- The shell runs on macOS, apps run inside, Fedora headless execution remains working, and all required tests pass before the full goal is complete.
- Keep shared app logic; host conventions belong in the framework.
- Do not hide a shared test failure by gating it away or add fake-success providers.
- Preserve live-equivalent headless event delivery, frame accounting, touch emulation and accessibility behavior.
- Do not change the existing profiling assertion that a warmed-up scroll scenario records one frame per notch.
- Work in `/Users/xaver/dev/qos-macos-shell`, branch `codex/macos-shell`.
- Mac target: `/Users/xaver/dev/qos/target/macos-shell`. Fedora clone: `/home/parallels/qos-macos-shell`; target: `/home/parallels/qos-target`. The controller performs git sync and Fedora verification.

## Evidence and scope

At `0063daf84`, the native UI non-golden run passed 3020/3032 tests. This task
addresses the profiling failure (41 frames instead of 40) and the shortcut
recorder expecting a Linux label from `PlatformConventions::default()` on Mac.
`Driver::send` currently replaces the full interaction environment with the
`POINTER`/`TOUCH` constants, which hardcode `KeyboardFamily::Pc`. Live UI already
uses transitions that retain discovered devices and keyboard family.

Font fixtures, Metal pixel-oracle semantics, search pagination and app styling
have separate recorded failures; they remain required work after this task.

## Task 1: Preserve host input conventions through headless events

**Files:**
- Modify/test: `ui/src/headless.rs` (`Driver::send` and its existing test module).
- Modify/test: `ui/src/widget/shortcut_recorder.rs` (named-key test only).
- Modify: `ui/src/app.rs` (three target-gated window attribute builders).
- Existing verification: `ui/tests/profiling.rs`, `ui/src/interaction.rs` tests.

**Interfaces:**
- Consumes `InteractionEnvironment::{with_pointer_modality, with_touch_modality, with_keyboard_modality}` and `Driver::{set_interaction_environment, interaction_environment, scroll}`.
- Produces unchanged public APIs; mouse/touch input changes modality without dropping keyboard family or discovered devices.

- [ ] Add a behavioral regression to the existing headless test module, using its real `Driver` and `send_touch` helper:

```rust
#[test]
fn pointer_and_touch_preserve_keyboard_family_and_discovered_devices() {
    let mut driver = Driver::new(
        State::new(),
        Box::new(Button::new("save", "Save", None)),
        (400, 200),
    );
    let mut initial = crate::InteractionEnvironment::default();
    initial.keyboard_family = crate::interaction::KeyboardFamily::Apple;
    initial.available.keyboard = true;
    initial.available.pointer = true;
    initial.available.touch = true;
    initial.available.controller = true;
    driver.set_interaction_environment(initial);
    driver.scroll(0.0, -1.0);
    let pointer = driver.interaction_environment();
    assert_eq!(pointer.keyboard_family, initial.keyboard_family);
    assert_eq!(pointer.available, initial.available);
    assert_eq!(pointer.pointer_precision, crate::PointerPrecision::Fine);
    send_touch(&mut driver, winit::event::TouchPhase::Started, 10.0, 10.0);
    let touch = driver.interaction_environment();
    assert_eq!(touch.keyboard_family, initial.keyboard_family);
    assert_eq!(touch.available, initial.available);
    assert_eq!(touch.pointer_precision, crate::PointerPrecision::Coarse);
}
```

- [ ] Run that test on Mac before changing `Driver::send`; record failure from lost keyboard/device information. Use `cargo nextest run -p ui --locked --profile ci -E 'test(pointer_and_touch_preserve_keyboard_family_and_discovered_devices)'` with the designated target directory.
- [ ] Change only the two modality choices in `Driver::send`:

```rust
WindowEvent::Touch(_) => Some(self.interaction.with_touch_modality()),
WindowEvent::CursorMoved { .. }
| WindowEvent::MouseInput { .. }
| WindowEvent::MouseWheel { .. } => Some(self.interaction.with_pointer_modality()),
```

  Retain the synthetic-touch reentrancy guard and keyboard transition. Do not suppress real relayouts when modality actually changes.

- [ ] Make the shortcut-recorder named-key test exercise both explicit platforms on every host. Keep the current real binding from `Super+Down` and assert literal expected labels:

```rust
assert_eq!(
    recorder.binding().keycap_labels(PlatformConventions::new(crate::platform::Platform::Linux)),
    ["Super", "Down"]
);
assert_eq!(
    recorder.binding().keycap_labels(PlatformConventions::new(crate::platform::Platform::MacOs)),
    ["⌘", "Down"]
);
```

- [ ] Remove the three Darwin-only `unused_mut` warnings in `AppHandler` window creation without lint suppression. Build immutable attributes, then shadow them in the Linux-only expression. Keep all existing names, ID resolution and settings:

```rust
#[cfg(target_os = "linux")]
let attributes = {
    use winit::platform::wayland::WindowAttributesExtWayland;
    let package_id = std::env::var("QOS_PACKAGE_ID").ok();
    let app_id = resolve_app_id(
        self.app.window_app_id.as_deref().or(package_id.as_deref()),
        std::env::current_exe()
            .ok()
            .and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
            .as_deref(),
    );
    attributes.with_name(format!("{app_id}.audience"), "")
};
```

  For floating windows, use the following Linux-only shadow:

```rust
#[cfg(target_os = "linux")]
let attributes = {
    use winit::platform::wayland::WindowAttributesExtWayland;
    attributes.with_name(
        format!("{}{}", crate::floating_surface::FLOATING_SURFACE_APP_ID_PREFIX, token),
        "",
    )
};
```

  For primary windows, preserve the `window_attributes` name:

```rust
#[cfg(target_os = "linux")]
let window_attributes = {
    use winit::platform::wayland::WindowAttributesExtWayland;
    let package_id = std::env::var("QOS_PACKAGE_ID").ok();
    let app_id = resolve_app_id(
        self.app.window_app_id.as_deref().or(package_id.as_deref()),
        std::env::current_exe()
            .ok()
            .and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
            .as_deref(),
    );
    window_attributes.with_name(app_id, "")
};
```

  At all three sites, remove `mut` from the initial declaration and retain its full builder chain unchanged.

- [ ] Format the three affected files and run focused nextest coverage for all headless, interaction, shortcut-recorder and profiling tests. The existing `scroll_scenario_produces_per_pass_stats` must pass unchanged. Command: `cargo nextest run -p ui --locked --profile ci -E 'test(headless::tests::) or test(interaction::tests::) or test(widget::shortcut_recorder::tests::) or test(profiling::)'`.
- [ ] Run `cargo clippy -p ui --all-targets --locked -- -D warnings`. Report unrelated failures separately; do not weaken them.
- [ ] Commit only the three task files as `fix(ui): preserve host input conventions in headless execution`.
- [ ] Controller fast-forwards Fedora and runs the same focused suite and UI all-target clippy. Record commands, exact commit and results. Existing full-suite failures remain in the goal ledger.

## Review and later work

Task review covers spec compliance, input behavior and platform compilation.
The broader port still needs canonical app factories/lifecycle, instance wake
routing, native shell/Metal hosting, shared services, real-app qualification and
the complete cross-host regression gates from the spec.

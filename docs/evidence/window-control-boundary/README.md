# Shared maximized window controls

Restore and Close activate on a completed click through the SDK PaintedTargets driver. A held press remains inert; Escape, pointer leave, focus loss, hidden chrome, changed geometry or changed input scale cancel it and consume its release. The existing deferred close commit and tiled restore layout remain covered. ButtonFace supplies hover/pressed colors through the shared compact pill painter; materials, geometry and quiet close styling are preserved.

WindowGroup.presentation_bounds and the shell host forwarding API report the existing group/client/control rectangles without a paint, layout, reveal or action drain. Shared PointerAction produces winit events, scales logical coordinates once and dispatches through production Widget input; the shots driver retains its existing FrameBuilder route.

The before completed-click regression fails on the original production path. After repair, three SDK maximized-control cases, the existing restore and durable-close cases, five shared activation cases and all seven desktop integration journeys pass. Shell --tests compilation and strict all-target UI/shell Clippy pass.

Four offscreen idle originals (light/dark, default/200% text) are byte-identical before/after. Held Close/Restore and cancelled/reflowed variants were inspected. These are production WindowGroup component fixtures containing the Stub Settings tile, not screenshots of a live app or full desktop. Original PNGs and SHA-256 qualification are attached to the ready source PR; no PNG baseline was refreshed.

Live VM replacement qualification is separate: a fresh supported daemon and shell activation succeeded, with unchanged trust policy. A complete replacement auditor requires a same-source authenticated apps/runtime layer and an actual failed-startup recovery fixture; that gate is not claimed by component captures. Remaining surfaces stay in the adoption inventory.

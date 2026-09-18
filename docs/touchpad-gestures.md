# Touchpad pinch-to-zoom in Tilo — what's needed

Today apps zoom on **Ctrl + two-finger scroll** because that's the only touchpad-zoom
signal the toolkit (winit) delivers on Wayland. Bare **pinch-to-zoom** needs work at two
layers. The gesture chain is: touchpad → libinput → compositor → Wayland protocol → winit
→ app.

## Where each layer stands

1. **libinput** — already detects 2-finger pinch (`GESTURE_PINCH_BEGIN/UPDATE/END`, with a
   scale factor + delta). **No work.**

2. **Compositor (smithay)** — needs to advertise the pinch protocol to clients:
   - Handle `InputEvent::GesturePinchBegin/Update/End` from smithay's libinput backend
     (they're already surfaced by the input backend).
   - Implement the **`zwp_pointer_gestures_v1`** global — smithay provides
     `PointerGesturesState` + the `delegate_pointer_gestures!` handler; wire it into the
     compositor's `State` and forward pinch events to the focused surface's pointer.
   - This is the Tilo-compositor work item. **Note:** on the current GNOME dev box, Mutter
     *already* advertises `zwp_pointer_gestures_v1`, so this layer isn't what's blocking
     dev testing — layer 3 is.

3. **Toolkit (winit) — the gap.** winit's Wayland backend does **not** bind
   `zwp_pointer_gestures_v1`; `WindowEvent::PinchGesture` is emitted **only** on macOS/iOS
   (verified against winit 0.30.13 source). So even when the compositor advertises the
   protocol (Mutter does today), winit-based Tilo apps never receive a pinch event.
   **Fix: vendor/patch winit to bind `zwp_pointer_gestures_v1` on Wayland and emit
   `WindowEvent::PinchGesture(delta)`** (map the protocol's `scale` to the same delta the
   macOS path produces), mirroring the existing macOS behavior. Mechanically: in winit's
   `platform_impl/linux/wayland`, register the `zwp_pointer_gestures_v1` global in the
   registry handler, create a `zwp_pointer_gesture_pinch_v1` for the seat pointer, and
   translate its `begin/update/end` into the existing `PinchGesture` event. Ship it via a
   `[patch.crates-io] winit = { git = "<our fork>" }` (Tilo already carries a couple of
   vendored deps).

4. **App** — already handles `WindowEvent::PinchGesture` (the PDF fix kept the macOS arm).
   **No further work** once winit delivers the event; every ui app gets pinch through
   the shared toolkit.

## Recommendation
Do **layer 3 (the winit patch) first** — it is **testable today on the
GNOME dev box** (Mutter already delivers the protocol), and it unblocks pinch across every
Tilo app at once since they all go through `ui` → winit. Then add **layer 2** (smithay
`wp_pointer_gestures`) so the Tilo compositor advertises the protocol when you're not on
GNOME. Effort: the winit patch is a few hundred lines in one backend module; the smithay
handler is a small delegate + input-event forwarding.

## Pragmatic alternative (no toolkit fork)
The Tilo compositor translates libinput pinch → **synthetic Ctrl+scroll wheel events** to the
focused client. Apps already handle Ctrl+scroll (PDF does), so nothing app-side changes.
Downsides: lower fidelity (scroll deltas, not a true scale factor), and it only works on the
Tilo compositor (not the GNOME dev box, which doesn't run our input path). Use this only if
the winit patch proves too costly to carry.

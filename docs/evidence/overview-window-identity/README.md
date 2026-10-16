# Overview window identity evidence

The native overview already uses the toolkit selection face and focus ring.
This increment binds that shared focus to a stable tab identity, including its
accessibility semantic key, rather than the preview's current row index.

On the inspected main (78a7f73), closing another window makes the selected Files
preview become Terminal. Closing the selected preview then pressing Enter
dismisses overview instead of consuming the interrupted activation. Both
regressions fail on main through production window close and input paths.

After the fix, a surviving selected window retains its focus and activates the
same tab. A disappeared selection restores the current workspace's shared focus;
one pending Enter/Space is consumed. Subsequent navigation, activation, Esc and
reopen continue normally. The native keyboard test also resizes to 800×480 at
200% text and asserts the production accessibility tree.

Eight before and eight after PNGs exercise the production ShellOverlayHost painter
in dark, light, high contrast and 200% text. The capture fixture uses demo state
only in tests, never in a user's desktop. Six inspected representative images
are saved to ChatGPT Library; artifacts.json records their identities, dimensions
and checksums. The ready PR embeds shareable browser attachments for both pairs.

Rendering and Cargo verification run on the remote builder. Rendering explicitly
selects Vulkan lavapipe with WGPU_BACKEND=vulkan and the lvp ICD. These are
offscreen production captures and native in-memory input journeys, not live VM
or physical-device verification. The previously denied DRM access was not retried.

Validation: all 26 overview-filtered tests and seven overview_window_actions
integration cases (pointer, touch, keyboard, close and policy-gated drag),
shell all-target Clippy with warnings
denied, cargo check --locked -p shell --tests, staged hooks and affected-package
scope. Normal CI, protected landing and a merged-main postcheck remain required.

# Workspace name shared-field qualification

A long workspace name previously painted beyond its field and left the caret offscreen.
The idle name also escaped its border. The baseline production caret-bounds regression
fails at 6d750be0472d653b15c61616d551d6581adfa531.

The desktop now composes FieldFace and a lazy, local-mode TextInput for painting.
The shell still owns the draft, directional selection, keyboard/IME routing and
the existing rename commit. The shared input shapes, clips and horizontally scrolls
the name; idle FieldFace elides it. The Create-invite control also uses the shared primary ButtonFace, replacing
a fill/label combination whose label was invisible in both light and dark.
A real pixel regression fails on the old painter and observes the new label.
The footer reserves measured Caption height and themed insets outside the
scrolling body; combined scaling no longer clips its instructions.

General body geometry reserves both UI and text
scales so the description does not collide with the name field at 150% UI/130% text.
Embedded TextInput receives the composed font scale because it shapes a stock role
rather than reading the UI-scaled typography token.

## Original captures

artifacts.json records unmodified PNG checksums, dimensions, baseline revision
and the exact production source blob. PNGs stay outside Git and representative
originals are attached to the owning PR. The six before/after variants cover dark,
light, high contrast, 200% text, 150% UI/130% text and a narrow idle field.

These card fixtures call draw_ws_settings_name_input with the same General body
clip as the production parent. The black surroundings are fixture space, not a
claim of an app-filled desktop. The separate native matrix paints the full shell
overlay and covers end-of-name and selected-Home states. Its six intermediate
originals capture the shared name field with the still-broken legacy invite and
footer painters; six final originals show the corrected controls. This phase
is labelled native-before-controls rather than an unchanged-main desktop.
The native pixel assertions observe the invite label and footer inside their
actual painted bounds. All 24 originals were inspected; representative originals
are attached to the PR.

## Verification

- cargo test --locked -p tilo-desktop --lib workspace_settings::overlay::tests::
- cargo test --locked -p shell --lib workspace_name_
- Native existing directional-selection/accessibility/IME and modal chord/Esc tests.
- cargo check --locked -p shell --tests
- Desktop/shell all-target Clippy and staged pre-commit checks.

The new native journey uses host.handle_event for pointer press/release, Home/End,
text entry, resize, cancellation, reopen, select-all, Unicode rename, commit,
held Enter repeat/release and dismissal; it asserts that input never reaches app content.
The held-submit regression also fails against the original 6d750be shell host.
The fix retains submit ownership until release, focus loss or a fresh press;
normal app input remains available beside the contextual lens. The owned-room
native fixture uses isolated hosted inputs and the production owner predicate,
without a policy override.

Painting does not mutate the host's selection or permission state.

Hardware VM/device validation belongs to the integrated QA owner on its authorized
isolated VM. No denied KVM/DRM route is retried here. Other workspace/device transitions,
header layout, status and chrome states remain in the desktop adoption inventory.

Qualification: nine focused desktop cases, six native cases (three new plus
directional accessibility/IME, modal chords and contextual-lens behavior),
shell integration compilation and desktop/shell all-target Clippy passed.
All Cargo and production renders ran on the SSH runner with only the CPU Vulkan ICD.

Overlap check: complete Messages #2881 (182 files) and Dictation #2974 (106 files)
share overlay_host/mod.rs. The new host-field/rename-submit/early Enter edge hunks
are separate from their update-pump/layer-geometry hunks. Files #3009 broker,
transfer and undo work, contextual composer registration, credentials and policy
are unchanged. Hardware QA remains with the integrated VM owner.

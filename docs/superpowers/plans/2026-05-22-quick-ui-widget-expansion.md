# Quick-UI widget expansion plan

> **Status:** ready-to-dispatch. Each unit below is a self-contained subagent prompt. Units are **file-disjoint** — multiple workers can run in parallel without conflict.

## Context

`quick-ui` ships **27 widgets** today (`quick-ui/src/widget/`): `app_widget`, `button`, `checkbox`, `column`, `command_palette`, `container`, `context_menu`, `data_table`, `file_picker`, `group_row`, `image`, `permission_prompt`, `row`, `scrollable`, `slider`, `spacer`, `stack`, `strip_view`, `tabs`, `text`, `text_input`, `tile_host`, `toast`, `toggle`, `tooltip`, `window_group`, `workspace_view`. The toolkit is solid (204 lib tests + the recently added `test_id` resolver + recording-mode video output). What's missing is breadth — there are no date/time pickers, no popover/modal primitives, no media widgets, no breadcrumbs, no progress indicators.

Note: there's an existing `quick-ui/src/overlay/popover.rs` with a `Popover` **struct** — that's a 10-line positioning/scrim hint, **not a Widget**. The "Popover widget" Unit 6 below is the user-facing Widget version that wraps arbitrary content; name it `PopoverWidget` (or move `overlay::Popover` aside) to avoid the collision.

Every unit below adds **one widget** plus its tests. No cross-widget refactoring. No trait-wide changes. New widgets implement the `Widget` trait (see `quick-ui/src/widget/mod.rs`) and follow the five-pass frame (sync → build_layout → apply_layout → draw → handle_event).

Read first:
- `CLAUDE.md` — distrobox build env, system deps, submodule workflow, "never `--force` remove untracked", verification covenant
- `quick-ui/src/widget/mod.rs` — `Widget` trait + the `test_id` opt-in
- `quick-ui/src/widget/button.rs` or `quick-ui/src/widget/slider.rs` — clean reference widgets to copy structure from
- `quick-ui/src/theme.rs` — colour + spacing tokens (NEVER hard-code colours; read from `theme`)

Test conventions:
- Tests live at the bottom of the same file (`#[cfg(test)] mod tests { ... }`)
- Tests use `crate::theme::Theme::dark()` for visual assertions; no GPU surface needed for layout/state assertions
- Use `vello::Scene::new()` directly to assert on drawn output (walk `scene.encoding().draw_data` similar to `concept_parity_spec` patterns)
- **No `#[ignore]`, no weakening assertions** (covenant)

## Units

Each unit below is ready to paste into a subagent prompt. Standard preamble for every unit:

> Repo: `/var/home/quinten/Development/quickdesktop`, branch `nirios-shell`. `quick-ui/` is **in-tree** (not a submodule). Builds use `distrobox enter my-distrobox -- bash -c 'cd /home/quinten/Development/quickdesktop && <cmd>'`. Set up a worktree off `nirios-shell` named `quick-ui-<widget>`. Single in-tree commit when done. Don't push, don't open a PR.
> Existing ledger: **204 quick-ui --lib tests, 335 quick-shell --lib tests** — both must stay green. Your unit should ADD tests; nothing should regress.

---

### Unit 1: ProgressBar widget

**Files:** `quick-ui/src/widget/progress_bar.rs` (new), `quick-ui/src/widget/mod.rs` (add `pub mod progress_bar; pub use progress_bar::ProgressBar;`)

**API:**
```rust
pub struct ProgressBar {
    progress: f32,       // 0.0 to 1.0; >1.0 enters indeterminate mode
    indeterminate: bool, // true: animated bar moves across the track regardless of `progress`
    rect: Rect,
    // ... animation state
}
impl ProgressBar {
    pub fn new(progress: f32) -> Self;
    pub fn indeterminate() -> Self;
    pub fn set_progress(&mut self, p: f32);
}
```

**Visuals:** rounded track at `theme.surface.elevated`, fill at `theme.accent.primary`, 8 px height by default. Indeterminate mode animates a 30% wide segment across the track at 2 s period.

**Tests:**
- `progress_bar_at_zero_paints_no_fill`
- `progress_bar_at_full_paints_track_width_fill`
- `progress_bar_at_half_paints_half_fill_within_one_pixel`
- `progress_bar_indeterminate_needs_animation_frame_returns_true`

---

### Unit 2: Spinner widget

**Files:** `quick-ui/src/widget/spinner.rs` (new), `mod.rs` (add)

**API:**
```rust
pub struct Spinner { size: f32, start: Instant, rect: Rect }
impl Spinner {
    pub fn new() -> Self;
    pub fn with_size(size: f32) -> Self; // diameter in px
}
```

**Visuals:** circular indicator with 4 dots orbiting at constant angular velocity (1 rotation per 1.5 s). Dots fade in/out so the leading one is brightest. Read colours from `theme.accent.primary` with alpha gradient.

**Tests:**
- `spinner_default_size_is_24_px`
- `spinner_needs_animation_frame_always`
- `spinner_paints_four_dots`

---

### Unit 3: DatePicker widget

**Files:** `quick-ui/src/widget/date_picker.rs` (new), `mod.rs` (add). Add `chrono = "0.4"` to `quick-ui/Cargo.toml` — it is NOT currently a direct dep of `quick-ui` (`quick-shell` pulls it transitively but `quick-ui` doesn't yet). One-line Cargo addition.

**API:**
```rust
pub struct DatePicker {
    selected: chrono::NaiveDate,
    month_view: (i32, u32), // (year, month)
    on_change: Option<Box<dyn FnMut(NaiveDate)>>,
    rect: Rect,
}
impl DatePicker {
    pub fn new(initial: NaiveDate) -> Self;
    pub fn selected(&self) -> NaiveDate;
    pub fn on_change<F: FnMut(NaiveDate) + 'static>(self, f: F) -> Self;
}
```

**Visuals:** 7-column calendar grid with previous/next month buttons. Active day is highlighted with `theme.accent.primary`. Other-month days are dimmed.

**Tests:**
- `date_picker_initial_state_shows_selected_month`
- `date_picker_next_month_advances`
- `date_picker_clicking_day_updates_selection`
- `date_picker_first_day_of_week_is_monday` (locale assumption documented in code)

---

### Unit 4: TimePicker widget

**Files:** `quick-ui/src/widget/time_picker.rs` (new), `mod.rs` (add)

**API:**
```rust
pub struct TimePicker {
    hours: u8,    // 0-23
    minutes: u8,  // 0-59
    seconds: Option<u8>, // None unless explicit
    on_change: Option<Box<dyn FnMut(u8, u8, Option<u8>)>>,
    rect: Rect,
}
impl TimePicker {
    pub fn new(hours: u8, minutes: u8) -> Self;
    pub fn with_seconds(self, seconds: u8) -> Self;
}
```

**Visuals:** three vertical spinners (HH : MM : SS) with up/down arrows. Click-and-drag spins; scroll wheel also spins.

**Tests:**
- `time_picker_clamps_hours_to_23`
- `time_picker_clamps_minutes_to_59`
- `time_picker_scroll_up_increments_minutes`

---

### Unit 5: ColorPicker widget

**Files:** `quick-ui/src/widget/color_picker.rs` (new), `mod.rs` (add)

**API:**
```rust
pub struct ColorPicker {
    color: peniko::Color,
    on_change: Option<Box<dyn FnMut(Color)>>,
    rect: Rect,
}
impl ColorPicker {
    pub fn new(initial: Color) -> Self;
    pub fn color(&self) -> Color;
}
```

**Visuals:** HSV square (saturation × value) on the left, hue strip on the right, 3 RGB sliders below for fine control. Big preview swatch in the top-right.

**Tests:**
- `color_picker_dragging_hue_strip_changes_hue_only`
- `color_picker_clicking_sv_square_changes_saturation_and_value`
- `color_picker_rgb_slider_to_red_yields_pure_red`

---

### Unit 6: PopoverWidget

**Files:** `quick-ui/src/widget/popover_widget.rs` (new), `mod.rs` (add). **Do NOT name the struct `Popover`** — the name is taken by `quick-ui/src/overlay/popover.rs::Popover` (a 10-line positioning hint, not a Widget). Use `PopoverWidget`. See the Context section above.

**API:**
```rust
pub struct PopoverWidget<C: Widget> {
    anchor_rect: Rect,
    content: C,
    side: PopoverSide,  // Above / Below / Left / Right (auto-flip if no room)
    open: bool,
    on_dismiss: Option<Box<dyn FnMut()>>,
    rect: Rect,
}
pub enum PopoverSide { Above, Below, Left, Right }
```

**Visuals:** rounded rect with `theme.surface.overlay`, drop shadow, 4 px small arrow pointing at the anchor's edge. Position is auto-computed: if `Below` would clip off-screen, flip to `Above`.

**Tests:**
- `popover_anchored_below_positions_under_anchor`
- `popover_flips_when_no_room_below`
- `popover_dismiss_on_outside_click`
- `popover_closed_paints_nothing`

---

### Unit 7: ModalDialog widget

**Files:** `quick-ui/src/widget/modal_dialog.rs` (new), `mod.rs` (add)

**API:**
```rust
pub struct ModalDialog<C: Widget> {
    title: String,
    content: C,
    primary_action: Option<(String, Box<dyn FnMut()>)>,
    secondary_action: Option<(String, Box<dyn FnMut()>)>,
    on_dismiss: Box<dyn FnMut()>,
    open: bool,
    rect: Rect,
}
```

**Visuals:** centered rounded rect over a 70% black backdrop. `theme.surface.dialog`. Title bar + content + footer with the two action buttons (primary on right).

**Tests:**
- `modal_dialog_dismiss_on_esc`
- `modal_dialog_backdrop_click_dismisses`
- `modal_dialog_primary_action_invokes_callback`
- `modal_dialog_paints_backdrop_when_open` (assert non-zero alpha on a fullscreen rect)

---

### Unit 8: Toolbar widget

**Files:** `quick-ui/src/widget/toolbar.rs` (new), `mod.rs` (add)

**API:**
```rust
pub struct Toolbar {
    items: Vec<ToolbarItem>,
    orientation: Orientation,
    rect: Rect,
}
pub enum ToolbarItem {
    Button { icon: Icon, label: Option<String>, on_click: Box<dyn FnMut()> },
    Separator,
    Spacer,
}
pub enum Orientation { Horizontal, Vertical }
```

**Visuals:** strip of buttons separated by `theme.divider`. When the strip would overflow its rect, items past the limit collapse into a `…` overflow popover.

**Tests:**
- `toolbar_with_three_buttons_paints_three_button_rects`
- `toolbar_overflow_creates_overflow_button`
- `toolbar_separator_paints_a_divider_line`

---

### Unit 9: Breadcrumb widget

**Files:** `quick-ui/src/widget/breadcrumb.rs` (new), `mod.rs` (add)

**API:**
```rust
pub struct Breadcrumb {
    segments: Vec<String>,
    on_navigate: Option<Box<dyn FnMut(usize)>>, // index of clicked segment
    rect: Rect,
}
impl Breadcrumb {
    pub fn new(segments: Vec<String>) -> Self;
}
```

**Visuals:** `home > docs > project > readme.md` with chevron separators. Active segment (the rightmost) is bolded; earlier segments are clickable.

**Tests:**
- `breadcrumb_with_one_segment_paints_no_separator`
- `breadcrumb_clicking_middle_segment_calls_on_navigate_with_that_index`
- `breadcrumb_with_long_path_truncates_middle_segments`

---

### Unit 10: Avatar widget

**Files:** `quick-ui/src/widget/avatar.rs` (new), `mod.rs` (add)

**API:**
```rust
pub struct Avatar {
    image: Option<peniko::Image>,
    initials: String,    // 1-2 chars; used if image is None
    size: f32,
    rect: Rect,
}
impl Avatar {
    pub fn new(image: peniko::Image, size: f32) -> Self;
    pub fn from_initials(initials: &str, size: f32) -> Self;
}
```

**Visuals:** circular crop of the image, or a coloured circle (background colour deterministically derived from the initials hash) with the initials centred in white.

**Tests:**
- `avatar_with_initials_paints_initials_text`
- `avatar_with_initials_background_is_deterministic_for_same_initials`
- `avatar_with_image_paints_circle_clipped_image`

---

### Unit 11: Badge widget

**Files:** `quick-ui/src/widget/badge.rs` (new), `mod.rs` (add)

**API:**
```rust
pub struct Badge {
    content: BadgeContent,
    rect: Rect,
}
pub enum BadgeContent {
    Dot,                  // small filled circle
    Count(u32),           // displayed number; ≥100 shown as "99+"
    Text(String),         // arbitrary short text
}
```

**Visuals:** `theme.accent.danger` background, white text, rounded pill. Designed to be overlaid in the top-right corner of another widget (caller positions).

**Tests:**
- `badge_count_zero_paints_nothing` (semantic: zero badges shouldn't show)
- `badge_count_one_hundred_paints_99plus`
- `badge_dot_paints_filled_circle`

---

### Unit 12: ResizableSplit widget

**Files:** `quick-ui/src/widget/resizable_split.rs` (new), `mod.rs` (add)

**API:**
```rust
pub struct ResizableSplit<L: Widget, R: Widget> {
    left: L,
    right: R,
    orientation: SplitOrientation,
    split_position: f32, // 0.0 to 1.0, fraction of width/height for `left`
    rect: Rect,
}
pub enum SplitOrientation { Horizontal, Vertical }
```

**Visuals:** the divider is a 4 px strip (`theme.divider`), hover state shows a cursor change and ~2 px wider hover indicator. Dragging the divider updates `split_position`.

**Tests:**
- `resizable_split_initial_position_split_evenly`
- `resizable_split_dragging_divider_changes_split_position`
- `resizable_split_clamps_position_to_min_max_bounds` (10% min for each side)

---

### Unit 13: VideoPlayer widget

**Files:** `quick-ui/src/widget/video_player.rs` (new), `mod.rs` (add). Depends on `quick-shell::network::decoder::H264Decoder` — actually since `quick-ui` can't depend on `quick-shell`, **vendor** a tiny `H264Decoder` wrapper into `quick-ui` directly OR add `openh264` as a `quick-ui` dep too. Pick the latter (~10 line API surface).

**API:**
```rust
pub struct VideoPlayer {
    decoder: H264Decoder,
    latest_frame: Option<peniko::Image>,
    state: PlaybackState,
    on_seek: Option<Box<dyn FnMut(Duration)>>,
    rect: Rect,
}
pub enum PlaybackState { Playing, Paused, Stopped }
impl VideoPlayer {
    pub fn new() -> Self;
    pub fn feed_frame(&mut self, nalus: &[u8]); // for live streams
}
```

**Visuals:** video rect at the top, controls overlay at the bottom (auto-hide after 3 s of no mouse movement). Play/pause button, scrub bar (only meaningful if seeking is supported — emit `on_seek`).

**Tests:**
- `video_player_starts_paused`
- `video_player_feed_frame_updates_latest_image`
- `video_player_paint_with_image_uses_image_draw_command`

---

### Unit 14: ImageViewer widget

**Files:** `quick-ui/src/widget/image_viewer.rs` (new), `mod.rs` (add)

**API:**
```rust
pub struct ImageViewer {
    image: peniko::Image,
    zoom: f32,         // 1.0 = fit-to-viewport
    pan: kurbo::Vec2,  // image-space offset
    rect: Rect,
}
impl ImageViewer {
    pub fn new(image: peniko::Image) -> Self;
    pub fn fit_to_viewport(&mut self);
}
```

**Visuals:** image drawn centered + scaled by `zoom`; panning via click-and-drag, scroll wheel zooms (centered on cursor).

**Tests:**
- `image_viewer_default_zoom_is_one`
- `image_viewer_scroll_up_zooms_in`
- `image_viewer_drag_pans`
- `image_viewer_fit_resets_zoom_and_pan`

---

### Unit 15: TreeView widget

**Files:** `quick-ui/src/widget/tree_view.rs` (new), `mod.rs` (add)

**API:**
```rust
pub struct TreeView<T> {
    root: TreeNode<T>,
    expanded: HashSet<NodeId>,
    selected: Option<NodeId>,
    on_select: Option<Box<dyn FnMut(NodeId, &T)>>,
    rect: Rect,
}
pub struct TreeNode<T> {
    id: NodeId,
    label: String,
    data: T,
    children: Vec<TreeNode<T>>,
}
```

**Visuals:** indented rows. Expanded nodes show a `▼`; collapsed `▶`. Selected row is highlighted.

**Tests:**
- `tree_view_expand_node_shows_children`
- `tree_view_select_node_calls_on_select`
- `tree_view_arrow_right_expands_focused_node`
- `tree_view_arrow_down_moves_focus_to_next_visible_row`

---

### Unit 16: Segmented control (ToggleGroup)

**Files:** `quick-ui/src/widget/toggle_group.rs` (new), `mod.rs` (add)

**API:**
```rust
pub struct ToggleGroup {
    segments: Vec<Segment>,
    selected: usize,
    on_change: Option<Box<dyn FnMut(usize)>>,
    rect: Rect,
}
pub struct Segment {
    label: String,
    icon: Option<Icon>,
}
```

**Visuals:** horizontal strip of buttons; selected segment has `theme.accent.primary` background. Clicking changes selection (single-select; for multi-select use `Toolbar`).

**Tests:**
- `toggle_group_initial_selection_zero`
- `toggle_group_click_segment_two_selects_index_two`
- `toggle_group_paints_separator_lines_between_segments`

---

## Tier 2 — maintenance passes (also parallel-safe)

### Unit M-1: test_id audit

**File scope:** every widget impl in `quick-ui/src/widget/*.rs` that currently doesn't override `test_id()`. Add an override for ~10 high-value widgets:
- `button.rs::Button` — use the label as the id (`Some(self.label.as_str())`)
- `slider.rs::Slider` — `Some("slider")`  with a `with_id(self, id)` builder for custom ids
- `toggle.rs::Toggle` — same pattern
- `checkbox.rs::Checkbox` — same
- `text_input.rs::TextInput` — same
- `tabs.rs::Tabs` — `Some("tabs")` (per-tab discovery via the registry walk)
- `data_table.rs::DataTable` — `Some("data_table")`

**Tests:** for each, one test asserts `test_id()` returns the expected `Some(_)`. Add to each widget's existing test module.

### Unit M-2: theme audit

**File scope:** read-only walk of `quick-ui/src/widget/*.rs` looking for hard-coded `peniko::Color::rgb*` literals in `draw()` methods. Output a list of violations to `docs/superpowers/audits/2026-05-22-quick-ui-theme-audit.md`. **Don't fix in this unit** — just document. Fixes per widget come as follow-up units.

### Unit M-3: deprecated winit API

**File scope:** `quick-ui/src/app.rs`. Migrate from the deprecated `EventLoop::create_window` + `EventLoop::run` to `ApplicationHandler` + `EventLoop::run_app`. This is the path the compositor's `WinitBackend` already uses (see `quick-shell/src/compositor/backend/winit.rs` as a reference).

**Tests:** the existing example apps under `quick-ui/examples/` must still launch and paint. Verify with `cargo run -p quick-ui --example gallery` inside distrobox.

---

## Dispatch checklist

Before dispatching a worker for any unit:

1. [ ] Worker's prompt includes the standard preamble (worktree setup, distrobox build env, no-PR contract)
2. [ ] Worker's prompt cites the exact file(s) to create + the exact test names
3. [ ] Worker's prompt warns: never `git worktree remove --force` while untracked files are present
4. [ ] Worker's prompt commits all work before exiting (per CLAUDE.md "Worker-driven cherry-pick" recipe; `quick-ui` units are in-tree so it's a single commit on `nirios-shell`'s worktree branch — no submodule pointer bump)
5. [ ] Worker's report includes branch name + commit SHA + new test names + before/after lib counts

## Coordinator checklist after a worker finishes

1. [ ] `git status -C .claude/worktrees/<name>` — confirm zero untracked files
2. [ ] Cherry-pick the worker's commit onto `nirios-shell` (in-tree quick-ui units cherry-pick directly; no submodule fetch dance)
3. [ ] Run `cargo test -p quick-ui --lib` inside distrobox — confirm new tests pass + count grew
4. [ ] `git worktree remove .claude/worktrees/<name>` (no `--force` unless `git status` is clean) + `git branch -D <name>`
5. [ ] Mark task completed

---

## What's deferred from this plan

- **Rich text editor** — replaces `editor_app` stub. Needs design discussion first (Automerge `Text` collaboration, format marks). Should be its own plan after we have a brainstorming pass.
- **Drag-and-drop generic protocol** — needs cross-widget design (drop targets, drag sources, payload types). Plan separately.
- **Accessibility (AT-SPI tree)** — depends on the `test_id` widget-walk extension, but the AT-SPI surface itself is a whole tree to design. Plan separately when there's a screen-reader user-test path.

//! `PathBar` — the single, calm path affordance at the top of the file
//! manager. It has two modes:
//!
//! * **Breadcrumb** (default): the clickable path trail (`Home › Projects ›
//!   tilo`). Clicking an ancestor navigates to it.
//! * **Editable** (Finder/Nautilus location bar): clicking the current
//!   location — or the empty trail area, or pressing Ctrl+L — turns the trail
//!   into a real editable text field. As you type, **path completions appear in
//!   a dropdown below the field**; Enter navigates, Esc reverts to the trail.
//!
//! The path is shown exactly **once** — this bar replaces both the old separate
//! breadcrumb band *and* the footer's redundant path label.
//!
//! The dropdown is the shared [`ui::widget::Autocomplete`] with a filesystem
//! suggestion source, so the "text field + completion popover" behaviour (arrow
//! navigation, Tab-complete, click-to-accept, theming, selection/clipboard)
//! is the toolkit's, not hand-rolled here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use ui::widget::{AppModel, ModelPath};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::icons::icon_for_path;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Autocomplete, AutocompleteOutcome, Suggestion, Widget};

use crate::breadcrumb_bar::BreadcrumbBar;
use crate::breadcrumbs::breadcrumb_components;
use crate::command::{CommandSink, FileCommand, push};
use crate::i18n::t;
use crate::model::format_path;

/// Height of the bar (a single line of text plus breathing room for the
/// editable field's chrome).
pub const PATH_BAR_HEIGHT: f32 = 34.0;

pub struct PathBar {
    /// The clickable trail (breadcrumb mode).
    crumbs: BreadcrumbBar,
    /// The editable field + completion popover (edit mode).
    field: Autocomplete,
    editing: bool,
    current: PathBuf,
    home: Option<PathBuf>,
    // Completion and submission share the location captured when editing began.
    // Navigation may update the breadcrumb context without replacing typed text.
    edit_current: PathBuf,
    edit_home: Option<PathBuf>,
    /// The workspace mounts, so the trail can name one instead of
    /// spelling out where it happens to be mounted.
    workspace_roots: Vec<PathBuf>,
    sink: CommandSink,
    rect: Rect,
}

impl PathBar {
    pub fn new(sink: CommandSink) -> Self {
        Self {
            crumbs: BreadcrumbBar::new(Vec::new(), sink.clone()),
            field: Autocomplete::new()
                .with_id("files.path_input")
                .with_placeholder(&t("files-path-placeholder")),
            editing: false,
            current: PathBuf::new(),
            home: None,
            edit_current: PathBuf::new(),
            edit_home: None,
            workspace_roots: Vec::new(),
            sink,
            rect: Rect::ZERO,
        }
    }

    /// Update the trail to reflect the current directory. A no-op on the trail
    /// while editing, so navigation the user hasn't committed doesn't stomp
    /// their in-progress typing.
    pub fn set_path(
        &mut self,
        current: PathBuf,
        home: Option<PathBuf>,
        workspace_roots: Vec<PathBuf>,
    ) {
        self.current = current;
        self.home = home;
        self.crumbs.set_workspace_roots(workspace_roots.clone());
        self.workspace_roots = workspace_roots;
        if !self.editing {
            self.crumbs.set_segments(breadcrumb_components(
                &self.current,
                self.home.as_deref(),
                &self.workspace_roots,
            ));
        }
    }

    /// Enter edit mode: seed the field with the current path, wire the
    /// filesystem completion source, focus + select-all, and open the dropdown.
    pub fn begin_edit(&mut self) {
        self.editing = true;
        self.edit_current = self.current.clone();
        self.edit_home = self.home.clone();
        let home = self.edit_home.clone();
        let current = self.edit_current.clone();
        self.field
            .set_suggester(move |q| path_completions_at(q, home.as_deref(), &current));
        self.field
            .set_text(&format_path(&self.current, self.home.as_deref()));
        self.field.begin_edit();
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    fn end_edit(&mut self) {
        self.editing = false;
        self.field.close();
        self.field.set_focus(false);
        self.crumbs.set_segments(breadcrumb_components(
            &self.current,
            self.home.as_deref(),
            &self.workspace_roots,
        ));
    }
}

impl Widget for PathBar {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` only fails on tree-internal bookkeeping errors, never on
        // caller input; there is none to report here.
        #[allow(
            clippy::unwrap_used,
            reason = "new_leaf cannot fail for a freshly built style"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(PATH_BAR_HEIGHT),
                },
                flex_grow: 1.0,
                flex_shrink: 1.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // `node` was created by `build_layout` in this same tree, so a
        // lookup miss would mean the frame cycle called these out of order.
        #[allow(
            clippy::unwrap_used,
            reason = "node was built in this tree by build_layout; a miss means the frame cycle called these out of order"
        )]
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        self.crumbs.apply_layout_external(self.rect);
        self.field.apply_layout_external(self.rect);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.editing {
            self.field.draw(scene, transform, tr, theme, presence);
        } else {
            self.crumbs.draw(scene, transform, tr, theme, presence);
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.editing {
            self.field
                .draw_overlay(scene, transform, tr, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.editing {
            self.field.handle_event(event, ctx);
            match self.field.take_outcome() {
                AutocompleteOutcome::Accepted(value) => {
                    let resolved =
                        resolve_location(&value, self.edit_home.as_deref(), &self.edit_current);
                    push(&self.sink, FileCommand::OpenPath(resolved));
                    self.end_edit();
                }
                AutocompleteOutcome::Cancelled => self.end_edit(),
                AutocompleteOutcome::None => {}
            }
        } else {
            self.crumbs.handle_event(event, ctx);
            if self.crumbs.take_edit_requested() {
                self.begin_edit();
            }
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.editing {
            self.field.cursor_shape(point)
        } else {
            self.crumbs.cursor_shape(point)
        }
    }

    fn name(&self) -> &'static str {
        "PathBar"
    }

    fn test_id(&self) -> Option<&str> {
        Some("path_bar")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        if self.editing {
            self.field.set_focus(focused);
        } else {
            self.crumbs.set_focus(focused);
        }
    }

    fn is_focused(&self) -> bool {
        if self.editing {
            self.field.is_focused()
        } else {
            self.crumbs.is_focused()
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.editing && self.field.wants_pointer_capture()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        if self.editing {
            self.field.accessibility()
        } else {
            self.crumbs.accessibility()
        }
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        if self.editing {
            return self.field.perform_action(action, state, path);
        }
        let handled = self.crumbs.perform_action(action, state, path);
        if self.crumbs.take_edit_requested() {
            self.begin_edit();
        }
        handled
    }
}

/// Expand a leading `~` against `home` into an absolute path.
fn expand_tilde(input: &str, home: Option<&Path>) -> PathBuf {
    if let Some(home) = home {
        if input == "~" {
            return home.to_path_buf();
        }
        if let Some(rest) = input
            .strip_prefix('~')
            .and_then(|rest| rest.strip_prefix(std::path::is_separator))
        {
            return home.join(rest);
        }
    }
    PathBuf::from(input)
}

/// Resolve a location against the folder being browsed, without removing root
/// separators (`/` and `C:/` are not equivalent to empty text and `C:`).
fn resolve_location(input: &str, home: Option<&Path>, current: &Path) -> PathBuf {
    current.join(expand_tilde(input, home))
}

fn path_completions_at(input: &str, home: Option<&Path>, current: &Path) -> Vec<Suggestion> {
    let resolved = resolve_location(input, home, current);
    let mut query = resolved.to_string_lossy().into_owned();
    if (input.is_empty() || input == "~") && !query.ends_with(std::path::is_separator) {
        query.push(std::path::MAIN_SEPARATOR);
    }
    path_completions(&query, home)
}

/// The display form placed into the field for a completion: `~`-relative when
/// under home, absolute otherwise, with a trailing `/` on directories so the
/// next keystroke drills into it.
fn to_display(path: &Path, home: Option<&Path>, is_dir: bool) -> String {
    let mut s = format_path(path, home);
    if is_dir && !s.ends_with(std::path::is_separator) {
        s.push(std::path::MAIN_SEPARATOR);
    }
    s
}

/// Split the typed text into `(directory to list, name prefix to match)`.
///
/// The prefix comes from the raw typed string (the tail after the last `/`),
/// **not** from `PathBuf::file_name` — which normalises a trailing `.` away, so
/// typing `…/.` to reveal dotfiles would otherwise be lost.
fn split_for_completion(input: &str, home: Option<&Path>) -> (PathBuf, String) {
    if input.is_empty() || input == "~" {
        return (expand_tilde(input, home), String::new());
    }
    match input.rfind(std::path::is_separator) {
        Some(slash) => {
            // Retain the separator: `C:\` names the drive root, while `C:`
            // names that drive's current working directory.
            let dir_part = input.get(..=slash).unwrap_or_default();
            let prefix = input.get(slash + 1..).unwrap_or_default().to_string();
            let dir = if dir_part.is_empty() {
                PathBuf::from("/")
            } else {
                expand_tilde(dir_part, home)
            };
            (dir, prefix)
        }
        // No separator yet: nothing to anchor a directory listing on.
        None => (expand_tilde(input, home), String::new()),
    }
}

/// Real filesystem path completions for `input`: the entries of the directory
/// it names whose names start with the typed prefix (directories first). This
/// reads the directory directly — the same `read_dir` the listing uses — so the
/// suggestions are the live filesystem, never a mock.
pub fn path_completions(input: &str, home: Option<&Path>) -> Vec<Suggestion> {
    let (dir, prefix) = split_for_completion(input, home);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let show_hidden = prefix.starts_with('.');
    let lower_prefix = prefix.to_lowercase();
    let mut entries: Vec<(bool, String, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && !show_hidden {
                return None;
            }
            if !lower_prefix.is_empty() && !name.to_lowercase().starts_with(&lower_prefix) {
                return None;
            }
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            Some((is_dir, name, e.path()))
        })
        .collect();
    // Directories first, then case-insensitive by name.
    entries.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
    });
    entries
        .into_iter()
        .take(60)
        .map(|(is_dir, name, path)| {
            Suggestion::labelled(to_display(&path, home, is_dir), name)
                .with_icon(icon_for_path(&path, is_dir))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::new_sink;
    use automerge::AutoCommit;

    /// Editing keeps completion and submission on one location, accepted locations preserve roots, and breadcrumb mode enters named text input.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn path_editing_keeps_location_and_input_consistent() {
        // --- phase: navigation_during_edit_keeps_completion_and_submission_on_the_same_location ---
        {
            use ui::a11y::Action;
            use winit::event::{DeviceId, ElementState, KeyEvent, RawKeyEvent};
            use winit::keyboard::{Key, KeyLocation, NamedKey, NativeKeyCode, PhysicalKey};

            let fixture = tempfile::tempdir().unwrap();
            let before = fixture.path().join("before");
            let after = fixture.path().join("after");
            for directory in [&before, &after] {
                std::fs::create_dir_all(directory.join("same-name")).unwrap();
            }
            for input in ["same-name", "~/same-name"] {
                let sink = new_sink();
                let mut bar = PathBar::new(sink.clone());
                bar.set_path(before.clone(), Some(before.clone()), Vec::new());
                bar.begin_edit();
                // An asynchronous navigation updates the displayed folder while the
                // location editor deliberately preserves the user's in-progress edit.
                bar.set_path(after.clone(), Some(after.clone()), Vec::new());
                let mut doc = AutoCommit::new();
                assert!(bar.perform_action(
                    &Action::SetValue(input.into()),
                    &mut doc,
                    &automerge::ROOT
                ));
                assert_eq!(bar.field.suggestions().len(), 1);
                assert_eq!(
                    resolve_location(&bar.field.suggestions()[0].value, Some(&before), &before),
                    before.join("same-name")
                );

                let key = Key::Named(NamedKey::Enter);
                let event = WindowEvent::KeyboardInput {
                    device_id: DeviceId::dummy(),
                    event: KeyEvent::synthetic(
                        RawKeyEvent {
                            physical_key: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                            state: ElementState::Pressed,
                        },
                        key.clone(),
                        None,
                        KeyLocation::Standard,
                        false,
                        None,
                        key,
                    ),
                    is_synthetic: false,
                };
                let mut taffy = TaffyTree::new();
                let node = taffy.new_leaf(Style::default()).unwrap();
                let mut handled = false;
                let mut focus = None;
                bar.handle_event(
                    &event,
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &winit::event::Modifiers::default(),
                        state: &mut doc,
                        path: &automerge::ROOT,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &taffy,
                    },
                );
                assert!(handled);
                assert!(!bar.is_editing());
                assert_eq!(
                    &*sink.borrow(),
                    &[FileCommand::OpenPath(before.join("same-name"))]
                );
            }
        }

        // --- phase: accepted_locations_preserve_roots_and_use_the_browsed_folder ---
        {
            let root = tempfile::tempdir().unwrap();
            let current = root.path().join("current");
            std::fs::create_dir(&current).unwrap();
            let child = current.join("child");
            std::fs::create_dir(&child).unwrap();
            assert_eq!(resolve_location("child", None, &current), child);
            assert_eq!(
                resolve_location("~", Some(root.path()), &current),
                root.path()
            );
            let disk_root = crate::breadcrumbs::filesystem_root(&current);
            assert_eq!(
                resolve_location(&disk_root.display().to_string(), None, &current),
                disk_root
            );
            #[cfg(windows)]
            assert_eq!(
                resolve_location("C:/", None, &current),
                PathBuf::from("C:/")
            );
        }

        // --- phase: breadcrumb_mode_is_focusable_and_activation_enters_named_text_input ---
        {
            use ui::a11y::{Action, Role};

            let mut bar = PathBar::new(new_sink());
            bar.set_path(
                PathBuf::from("/home/u/Documents"),
                Some(PathBuf::from("/home/u")),
                Vec::new(),
            );
            bar.set_focus(true);
            let trail = bar.accessibility().expect("trail semantics");
            assert_eq!(trail.role, Role::Group);
            assert_eq!(trail.name.as_deref(), Some("Breadcrumbs"));
            assert!(trail.state.focused && trail.state.focusable);

            let mut doc = AutoCommit::new();
            assert!(bar.perform_action(&Action::Activate, &mut doc, &automerge::ROOT));
            assert!(bar.is_editing());
            let input = bar.accessibility().expect("editable path semantics");
            assert_eq!(input.role, Role::TextInput);
            assert!(input.state.focusable);
        }
    }

    /// Completion lists matching entries, whole directories on a trailing slash, expands tilde, reads the browsed folder and shows hidden entries only for dot prefixes.
    #[test]
    fn path_completion_lists_matching_entries() {
        // --- phase: completions_list_directory_entries_matching_the_prefix ---
        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("Documents")).unwrap();
            std::fs::create_dir(dir.path().join("Downloads")).unwrap();
            std::fs::create_dir(dir.path().join("Music")).unwrap();
            std::fs::write(dir.path().join("Do-not-list.txt"), b"x").unwrap();
            std::fs::write(dir.path().join(".hidden"), b"x").unwrap();

            // "<dir>/Do" matches the two "Do…" directories + the "Do…" file.
            let input = dir.path().join("Do").display().to_string();
            let sugg = path_completions(&input, None);
            let labels: Vec<&str> = sugg.iter().map(|s| s.label.as_str()).collect();
            assert!(labels.contains(&"Documents"), "labels: {labels:?}");
            assert!(labels.contains(&"Downloads"), "labels: {labels:?}");
            assert!(labels.contains(&"Do-not-list.txt"), "files complete too");
            assert!(!labels.contains(&"Music"), "non-matching entry excluded");
            // Directories are offered before files.
            let doc_pos = labels.iter().position(|l| *l == "Documents").unwrap();
            let file_pos = labels.iter().position(|l| *l == "Do-not-list.txt").unwrap();
            assert!(
                doc_pos < file_pos,
                "directories sort before files: {labels:?}"
            );
            // A dir suggestion's inserted value carries a trailing slash so the next
            // keystroke drills into it.
            let docs = sugg.iter().find(|s| s.label == "Documents").unwrap();
            assert_eq!(
                docs.value,
                format!(
                    "{}{}",
                    dir.path().join("Documents").display(),
                    std::path::MAIN_SEPARATOR
                )
            );
        }

        // --- phase: trailing_slash_lists_the_whole_directory ---
        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("alpha")).unwrap();
            std::fs::create_dir(dir.path().join("beta")).unwrap();
            let input = format!("{}/", dir.path().display());
            let sugg = path_completions(&input, None);
            let labels: Vec<&str> = sugg.iter().map(|s| s.label.as_str()).collect();
            assert!(
                labels.contains(&"alpha") && labels.contains(&"beta"),
                "{labels:?}"
            );
        }

        // --- phase: tilde_expands_to_home ---
        {
            let home = tempfile::tempdir().unwrap();
            std::fs::create_dir(home.path().join("Projects")).unwrap();
            let sugg = path_completions("~/Pro", Some(home.path()));
            assert_eq!(sugg.len(), 1);
            assert_eq!(sugg[0].label, "Projects");
            // The inserted value stays in ~-form for readability.
            assert!(
                sugg[0].value.starts_with("~/Projects"),
                "value: {}",
                sugg[0].value
            );
            assert_eq!(
                expand_tilde("~/Projects", Some(home.path())),
                home.path().join("Projects"),
            );
        }

        // --- phase: hidden_entries_appear_only_when_the_prefix_is_a_dot ---
        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join(".config")).unwrap();
            std::fs::create_dir(dir.path().join("visible")).unwrap();
            let all = path_completions(&format!("{}/", dir.path().display()), None);
            assert!(
                all.iter().all(|s| s.label != ".config"),
                "dotfiles hidden by default"
            );
            let dotted = path_completions(&format!("{}/.", dir.path().display()), None);
            assert!(
                dotted.iter().any(|s| s.label == ".config"),
                "a dot prefix reveals them"
            );
        }

        // --- phase: relative_completions_read_the_browsed_folder ---
        {
            let root = tempfile::tempdir().unwrap();
            let current = root.path().join("current");
            std::fs::create_dir(&current).unwrap();
            let child = current.join("unique-child");
            std::fs::create_dir(&child).unwrap();
            std::fs::write(child.join("nested.txt"), b"test").unwrap();
            assert_eq!(path_completions_at("", None, &current).len(), 1);
            assert_eq!(
                path_completions_at("~", Some(&current), root.path()).len(),
                1
            );
            let suggestions = path_completions_at("unique-ch", None, &current);
            assert_eq!(suggestions.len(), 1);
            assert_eq!(suggestions[0].label, "unique-child");
            assert_eq!(PathBuf::from(&suggestions[0].value), child);
            let nested = path_completions_at("unique-child/", None, &current);
            assert_eq!(nested.len(), 1);
            assert_eq!(nested[0].label, "nested.txt");
        }
    }

    #[test]
    #[cfg(windows)]
    fn drive_root_and_unc_completion_preserve_the_absolute_prefix() {
        assert_eq!(
            expand_tilde(r"~\Documents", Some(Path::new(r"C:\Users\reader"))),
            PathBuf::from(r"C:\Users\reader\Documents")
        );
        assert_eq!(
            split_for_completion(r"C:\Do", None),
            (PathBuf::from(r"C:\"), "Do".into())
        );
        assert_eq!(
            split_for_completion(r"\\server\share\Do", None),
            (PathBuf::from(r"\\server\share\"), "Do".into())
        );
    }
}

//! `PathBar` — the single, calm path affordance at the top of the file
//! manager. It has two modes:
//!
//! * **Breadcrumb** (default): the clickable path trail (`Home › Projects ›
//!   qos`). Clicking an ancestor navigates to it.
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

use automerge::{AutoCommit, ObjId};
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
                .with_placeholder("Type a path, e.g. ~/Documents"),
            editing: false,
            current: PathBuf::new(),
            home: None,
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
        let home = self.home.clone();
        self.field
            .set_suggester(move |q| path_completions(q, home.as_deref()));
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
    fn sync(&mut self, _state: &mut AutoCommit, _root: &ObjId) {}

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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if self.editing {
            self.field.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            );
            match self.field.take_outcome() {
                AutocompleteOutcome::Accepted(value) => {
                    let resolved = expand_tilde(value.trim_end_matches('/'), self.home.as_deref());
                    push(&self.sink, FileCommand::OpenPath(resolved));
                    self.end_edit();
                }
                AutocompleteOutcome::Cancelled => self.end_edit(),
                AutocompleteOutcome::None => {}
            }
        } else {
            self.crumbs.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            );
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
        self.editing
    }

    fn set_focus(&mut self, focused: bool) {
        if self.editing {
            self.field.set_focus(focused);
        }
    }

    fn is_focused(&self) -> bool {
        self.editing && self.field.is_focused()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.editing && self.field.wants_pointer_capture()
    }
}

/// Expand a leading `~` against `home` into an absolute path.
fn expand_tilde(input: &str, home: Option<&Path>) -> PathBuf {
    if let Some(home) = home {
        if input == "~" {
            return home.to_path_buf();
        }
        if let Some(rest) = input.strip_prefix("~/") {
            return home.join(rest);
        }
    }
    PathBuf::from(input)
}

/// The display form placed into the field for a completion: `~`-relative when
/// under home, absolute otherwise, with a trailing `/` on directories so the
/// next keystroke drills into it.
fn to_display(path: &Path, home: Option<&Path>, is_dir: bool) -> String {
    let mut s = format_path(path, home);
    if is_dir && !s.ends_with('/') {
        s.push('/');
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
    match input.rfind('/') {
        Some(slash) => {
            let dir_part = input.get(..slash).unwrap_or_default();
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

    #[test]
    fn completions_list_directory_entries_matching_the_prefix() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Documents")).unwrap();
        std::fs::create_dir(dir.path().join("Downloads")).unwrap();
        std::fs::create_dir(dir.path().join("Music")).unwrap();
        std::fs::write(dir.path().join("Do-not-list.txt"), b"x").unwrap();
        std::fs::write(dir.path().join(".hidden"), b"x").unwrap();

        // "<dir>/Do" matches the two "Do…" directories + the "Do…" file.
        let input = format!("{}/Do", dir.path().display());
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
        assert!(docs.value.ends_with("/Documents/"), "value: {}", docs.value);
    }

    #[test]
    fn trailing_slash_lists_the_whole_directory() {
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

    #[test]
    fn tilde_expands_to_home() {
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

    #[test]
    fn hidden_entries_appear_only_when_the_prefix_is_a_dot() {
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
}

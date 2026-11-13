//! The path bar: the trail of the folder shown, and the editable location a
//! click on its empty space turns it into.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use ui::widget::ModelPath;

use taffy::{Dimension, Display, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::icons::icon_for_path;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Autocomplete, AutocompleteOutcome, Breadcrumb, Suggestion, Widget};

use crate::breadcrumbs::{breadcrumb_components, breadcrumb_root_icon};
use crate::command::{CommandSink, FileCommand, push};
use crate::i18n::t;
use crate::model::format_path;
use ui::widget::DrawChild;

pub struct PathBar {
    crumbs: Breadcrumb,
    targets: Vec<PathBuf>,
    crumbs_node: Option<NodeId>,
    edit_requested: Rc<Cell<bool>>,
    field: Autocomplete,
    editing: bool,
    current: PathBuf,
    home: Option<PathBuf>,
    edit_current: PathBuf,
    edit_home: Option<PathBuf>,
    workspace_roots: Vec<PathBuf>,
    sink: CommandSink,
    rect: Rect,
    own: ui::own_paint::OwnPaint,

    dirty: bool,
}

impl PathBar {
    pub fn new(sink: CommandSink) -> Self {
        Self {
            dirty: true,
            crumbs: Breadcrumb::new(Vec::new()),
            targets: Vec::new(),
            crumbs_node: None,
            edit_requested: Rc::new(Cell::new(false)),
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
            own: ui::own_paint::OwnPaint::default(),
        }
    }

    pub fn set_path(
        &mut self,
        current: PathBuf,
        home: Option<PathBuf>,
        workspace_roots: Vec<PathBuf>,
    ) {
        self.current = current;
        self.home = home;
        self.workspace_roots = workspace_roots;
        if !self.editing {
            self.refresh_crumbs();
        }
    }

    fn refresh_crumbs(&mut self) {
        let components =
            breadcrumb_components(&self.current, self.home.as_deref(), &self.workspace_roots);
        let labels: Vec<String> = components.iter().map(|(label, _)| label.clone()).collect();
        let targets: Vec<PathBuf> = components.into_iter().map(|(_, target)| target).collect();
        if labels == self.crumbs.segments() && targets == self.targets {
            return;
        }
        let sink = self.sink.clone();
        let goto = targets.clone();
        let edit = Rc::clone(&self.edit_requested);
        self.crumbs = Breadcrumb::new(labels.clone())
            .root_icon(breadcrumb_root_icon(
                &self.current,
                self.home.as_deref(),
                &self.workspace_roots,
            ))
            .on_view("path_bar", move |index| {
                if let Some(target) = goto.get(index) {
                    push(&sink, FileCommand::Navigate(target.clone()));
                }
            })
            .on_edit_requested(move || edit.set(true))
            .on_overflow({
                let sink = self.sink.clone();
                let hidden_labels = labels;
                let hidden_targets = targets.clone();
                move |hidden, rect| {
                    let folders = hidden
                        .into_iter()
                        .filter_map(|index| {
                            Some((
                                hidden_labels.get(index)?.clone(),
                                hidden_targets.get(index)?.clone(),
                            ))
                        })
                        .collect();
                    push(
                        &sink,
                        FileCommand::ShowPathMenu {
                            folders,
                            at: Point::new(rect.x0, rect.y1),
                        },
                    );
                }
            });
        self.targets = targets;
    }

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

    pub fn crumb_targets(&self) -> Vec<(vello::kurbo::Rect, PathBuf)> {
        if self.is_editing() {
            return Vec::new();
        }
        self.crumbs
            .segment_rects()
            .into_iter()
            .filter_map(|(index, rect)| Some((rect, self.targets.get(index)?.clone())))
            .collect()
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    fn end_edit(&mut self) {
        self.editing = false;
        self.field.close();
        self.field.set_focus(false);
        self.refresh_crumbs();
    }
}

impl Widget for PathBar {
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        visitor(&self.crumbs);
        true
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.crumbs]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.crumbs]
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let crumbs = self.crumbs.build_layout(taffy, theme, node_map);
        let height = Dimension::length(theme.tokens.metrics.min_pointer_target);
        #[allow(
            clippy::unwrap_used,
            reason = "the trail's node was just created in this same tree"
        )]
        let node = {
            taffy
                .new_with_children(
                    Style {
                        display: Display::Flex,
                        size: Size {
                            width: Dimension::length(f32::from(u16::MAX)),
                            height,
                        },
                        min_size: Size {
                            width: Dimension::length(0.0),
                            height,
                        },
                        flex_grow: 1.0,
                        flex_shrink: 1.0,
                        ..Default::default()
                    },
                    &[crumbs],
                )
                .unwrap()
        };
        self.crumbs_node = Some(crumbs);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        if let Some(crumbs) = self.crumbs_node {
            self.crumbs
                .apply_layout(taffy, crumbs, Point::new(self.rect.x0, self.rect.y0));
        }
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
        let (editing, rect) = (self.editing, self.rect);
        let (field, crumbs) = (&mut self.field, &mut self.crumbs);
        self.own.record(scene, transform, |scene| {
            if editing {
                field.draw_child(scene, transform, tr, theme, presence);
            } else {
                crumbs.draw_child(scene, transform, tr, theme, presence);
            }
            let reach = f64::from(theme.tokens.spacing.s2);
            rect.inflate(reach, reach)
        });
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.own.damage(regions);
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.own.present();
        self.field.mark_drawn();
        self.crumbs.mark_drawn();
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
        self.dirty = true;
        if self.editing {
            self.field.handle_event(event, ctx);
            match self.field.take_outcome() {
                AutocompleteOutcome::Accepted(value) => {
                    let resolved =
                        resolve_location(&value, self.edit_home.as_deref(), &self.edit_current);
                    push(&self.sink, FileCommand::OpenPath(resolved));
                    self.end_edit();
                    *ctx.handled = true;
                }
                AutocompleteOutcome::Cancelled => self.end_edit(),
                AutocompleteOutcome::None => {}
            }
        } else {
            let node = self.crumbs_node.unwrap_or(ctx.node_id);
            self.crumbs
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if self.edit_requested.replace(false) {
                *ctx.handled = true;
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

    fn needs_redraw(&self) -> bool {
        ui::widget::redraw_or_children(self.dirty, self)
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
        _key_path: &[u64],
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if self.editing {
            return self.field.perform_action(action, &[], state, path);
        }
        if matches!(action, ui::a11y::Action::Activate) {
            self.begin_edit();
            return true;
        }
        self.crumbs.perform_action(action, &[], state, path)
    }
}

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

pub(crate) fn resolve_location(input: &str, home: Option<&Path>, current: &Path) -> PathBuf {
    current.join(expand_tilde(input, home))
}

pub(crate) fn path_completions_at(
    input: &str,
    home: Option<&Path>,
    current: &Path,
) -> Vec<Suggestion> {
    let resolved = resolve_location(input, home, current);
    let mut query = resolved.to_string_lossy().into_owned();
    if (input.is_empty() || input == "~") && !query.ends_with(std::path::is_separator) {
        query.push(std::path::MAIN_SEPARATOR);
    }
    path_completions(&query, home)
}

fn to_display(path: &Path, home: Option<&Path>, is_dir: bool) -> String {
    let mut s = format_path(path, home);
    if is_dir && !s.ends_with(std::path::is_separator) {
        s.push(std::path::MAIN_SEPARATOR);
    }
    s
}

fn split_for_completion(input: &str, home: Option<&Path>) -> (PathBuf, String) {
    if input.is_empty() || input == "~" {
        return (expand_tilde(input, home), String::new());
    }
    match input.rfind(std::path::is_separator) {
        Some(slash) => {
            // Keep the separator: `C:\` names the drive root, while `C:` names that drive's current folder.
            let dir_part = input.get(..=slash).unwrap_or_default();
            let prefix = input.get(slash + 1..).unwrap_or_default().to_string();
            let dir = if dir_part.is_empty() {
                PathBuf::from("/")
            } else {
                expand_tilde(dir_part, home)
            };
            (dir, prefix)
        }
        None => (expand_tilde(input, home), String::new()),
    }
}

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
            let path = e.path();
            let is_dir = e
                .file_type()
                .is_ok_and(|t| t.is_dir() || (t.is_symlink() && path.is_dir()));
            Some((is_dir, name, path))
        })
        .collect();
    entries.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| crate::model::name_cmp(&a.1, &b.1))
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

    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn path_editing_keeps_location_and_input_consistent() {
        {
            use ui::a11y::Action;
            use winit::event::{ElementState, KeyEvent, RawKeyEvent};
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
                bar.set_path(after.clone(), Some(after.clone()), Vec::new());
                let mut doc = AutoCommit::new();
                assert!(bar.perform_action(
                    &Action::SetValue(input.into()),
                    &[],
                    &mut ui::widget::Edits::lend(&mut doc),
                    &automerge::ROOT
                ));
                assert_eq!(bar.field.suggestions().len(), 1);
                assert_eq!(
                    resolve_location(&bar.field.suggestions()[0].value, Some(&before), &before),
                    before.join("same-name")
                );

                let key = Key::Named(NamedKey::Enter);
                let event = ui::synthetic_input::keyboard_input(KeyEvent::synthetic(
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
                ));
                let handled = ui::testing::Dispatch::new()
                    .doc(&mut doc)
                    .send(&mut bar, &event)
                    .handled;
                assert!(handled);
                assert!(!bar.is_editing());
                assert_eq!(
                    &*sink.borrow(),
                    &[FileCommand::OpenPath(before.join("same-name"))]
                );
            }
        }

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

        {
            use ui::a11y::{Action, Role};

            let mut bar = PathBar::new(new_sink());
            bar.set_path(
                PathBuf::from("/home/u/Documents"),
                Some(PathBuf::from("/home/u")),
                Vec::new(),
            );
            let trail = bar.accessibility().expect("trail semantics");
            assert_eq!(trail.role, Role::List);
            assert_eq!(trail.value.as_deref(), Some("Documents"));

            let mut doc = AutoCommit::new();
            assert!(bar.perform_action(
                &Action::Activate,
                &[],
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            assert!(bar.is_editing());
            let input = bar.accessibility().expect("editable path semantics");
            assert_eq!(input.role, Role::TextInput);
            assert!(input.state.focusable);
        }
    }

    #[test]
    fn shared_breadcrumb_preserves_roots_targets_and_narrow_current_folder() {
        use ui::icons::Icon;
        let home = Path::new("/home/u");
        assert_eq!(breadcrumb_root_icon(home, Some(home), &[]), Icon::Home);
        assert_eq!(
            breadcrumb_root_icon(Path::new("/"), Some(home), &[]),
            Icon::HardDrive
        );
        for (path, expected) in [
            ("/home/u", vec!["Home"]),
            (
                "/home/u/Documents/Projects/shared/current/Reports",
                vec![
                    "Home",
                    "Documents",
                    "Projects",
                    "shared",
                    "current",
                    "Reports",
                ],
            ),
        ] {
            let mut bar = PathBar::new(new_sink());
            bar.set_path(PathBuf::from(path), Some(home.to_path_buf()), Vec::new());
            assert_eq!(bar.crumbs.segments(), expected);
            assert_eq!(bar.targets.first().map(PathBuf::as_path), Some(home));
            assert_eq!(
                bar.targets.last().map(PathBuf::as_path),
                Some(Path::new(path))
            );
            assert_eq!(
                bar.accessibility().unwrap().value.as_deref(),
                expected.last().copied()
            );
            let mut text = ui::font_catalog::fixture_renderer();
            for theme in [Theme::dark(), Theme::light()] {
                assert!(
                    text.measure_style("Documents", None, &theme.text(ui::TextRole::Caption))
                        .0
                        > 0.0,
                    "the actual breadcrumb face shapes its labels"
                );
                let wide = bar.crumbs.visible_segments(900.0, &theme, &mut text);
                assert_eq!(wide, expected);
                let narrow = bar.crumbs.visible_segments(160.0, &theme, &mut text);
                assert_eq!(narrow.last().map(String::as_str), expected.last().copied());
                if expected.len() > 4 {
                    assert!(narrow.len() < expected.len(), "the middle folds away");
                    assert_eq!(
                        bar.crumbs.visible_segments(320.0, &theme, &mut text),
                        vec!["Home", "...", "current", "Reports"],
                        "the shared trail preserves its root and final two targets"
                    );
                } else {
                    assert_eq!(bar.crumbs.separator_count(), 0);
                }
            }
        }
    }

    #[test]
    fn path_completion_lists_matching_entries() {
        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("Documents")).unwrap();
            std::fs::create_dir(dir.path().join("Downloads")).unwrap();
            std::fs::create_dir(dir.path().join("Music")).unwrap();
            std::fs::write(dir.path().join("Do-not-list.txt"), b"x").unwrap();
            std::fs::write(dir.path().join(".hidden"), b"x").unwrap();

            let input = dir.path().join("Do").display().to_string();
            let sugg = path_completions(&input, None);
            let labels: Vec<&str> = sugg.iter().map(|s| s.label.as_str()).collect();
            assert!(labels.contains(&"Documents"), "labels: {labels:?}");
            assert!(labels.contains(&"Downloads"), "labels: {labels:?}");
            assert!(labels.contains(&"Do-not-list.txt"), "files complete too");
            assert!(!labels.contains(&"Music"), "non-matching entry excluded");
            let doc_pos = labels.iter().position(|l| *l == "Documents").unwrap();
            let file_pos = labels.iter().position(|l| *l == "Do-not-list.txt").unwrap();
            assert!(
                doc_pos < file_pos,
                "directories sort before files: {labels:?}"
            );
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

        {
            let home = tempfile::tempdir().unwrap();
            std::fs::create_dir(home.path().join("Projects")).unwrap();
            let sugg = path_completions("~/Pro", Some(home.path()));
            assert_eq!(sugg.len(), 1);
            assert_eq!(sugg[0].label, "Projects");
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

    #[cfg(unix)]
    #[test]
    fn a_linked_folder_completes_as_a_folder_in_listing_order() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a-file.txt"), b"x").unwrap();
        std::fs::write(dir.path().join("file-10.txt"), b"x").unwrap();
        std::fs::write(dir.path().join("file-9.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), dir.path().join("z-linked")).unwrap();
        std::os::unix::fs::symlink("missing-target", dir.path().join("dangling")).unwrap();
        let sugg = path_completions(&format!("{}/", dir.path().display()), None);
        let labels: Vec<&str> = sugg.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "z-linked",
                "a-file.txt",
                "dangling",
                "file-9.txt",
                "file-10.txt"
            ],
            "a linked folder sorts with the folders, numbers read by value"
        );
        assert_eq!(
            sugg[0].value,
            format!(
                "{}{}",
                dir.path().join("z-linked").display(),
                std::path::MAIN_SEPARATOR
            ),
            "the next keystroke drills into the linked folder"
        );
        let dangling = sugg.iter().find(|s| s.label == "dangling").unwrap();
        assert!(!dangling.value.ends_with(std::path::MAIN_SEPARATOR));
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

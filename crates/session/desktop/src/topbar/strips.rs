//! Where you are, at the bar's left end: the workspace, then the strip
//! shown ("Home › Research"). The strip's name is one target, and it opens
//! the strip switcher on the right edge, the one place strips are picked,
//! made, named and removed. The bar never lists the strips or counts who is
//! on them: who is where shows as faces on the switcher's strips.
//!
//! The workspace's new name, asked for in the workspace menu, is typed in
//! place of its name here.
use super::{
    Affine, BAR_CONTROL_PX, ChromeIntent, Rect, Scene, TextRenderer, TextRole, Theme, TopBarChrome,
    at, t,
};
use crate::text_entry::{self, Edit, Mods};
use tilo_desktop_model::{DesktopTree, StripId};
use ui::text_render::TextStyle;
use ui::text_selection::Selection;
use ui::widget::{FieldFace, TextInput, Widget};
use winit::event::WindowEvent;

/// Longest workspace name the bar accepts, in characters.
const NAME_MAX_CHARS: usize = 48;

/// Widest the naming field grows, before the room the bar has.
pub(super) const FIELD_MAX_PX: f64 = 200.0;

#[derive(Default)]
pub(super) struct BarStrips {
    /// The active workspace's strips, in order.
    strips: Vec<(StripId, String)>,
    active: Option<StripId>,
    field_rect: Rect,
    /// The workspace's new name, while it is typed.
    naming: Option<Naming>,
}

/// The workspace name being typed.
struct Naming {
    text: String,
    selection: Selection,
    input: Option<TextInput>,
}

/// The drawn chevron between the workspace and the strip's name.
const STRIP_CHEVRON_PX: f64 = 12.0;

/// Everything about the strips that changes what the bar paints.
#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct StripsKey {
    shown: Option<String>,
    naming: Option<(String, usize, usize)>,
}

impl TopBarChrome {
    /// Whether the workspace's new name is being typed, in place of its
    /// name in the bar.
    pub(super) fn renaming_workspace(&self) -> bool {
        self.strips.naming.is_some()
    }

    pub(super) fn sync_strips(&mut self, tree: &DesktopTree) {
        let workspace = tree.workspaces.iter().find(|w| w.id == tree.active);
        self.strips.strips = workspace
            .map(|w| {
                w.strips
                    .iter()
                    .map(|strip| (strip.id, strip.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        self.strips.active = tree
            .active_strip
            .get(&tree.active)
            .copied()
            .filter(|id| self.strips.strips.iter().any(|(strip, _)| strip == id))
            .or_else(|| self.strips.strips.first().map(|(id, _)| *id));
    }

    /// The shown strip's name, which the bar names after the workspace
    /// (Home, a chevron, Research): only for a strip with a name.
    pub(super) fn strip_name(&self) -> Option<&str> {
        let active = self.strips.active?;
        self.strips
            .strips
            .iter()
            .find(|(id, _)| *id == active)
            .map(|(_, name)| name.trim())
            .filter(|name| !name.is_empty())
    }

    /// The gap either side of the chevron before the strip's name.
    fn strip_name_gap(theme: &Theme) -> f64 {
        f64::from(theme.tokens.spacing.s1)
    }

    /// The width the strip's name takes after the workspace's pill at its
    /// natural length: the separator, its gaps and the name; zero when the
    /// bar names no strip. The workspace name leaves it this room.
    pub(super) fn strips_natural_width(
        &self,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        let Some(name) = self.strip_name() else {
            return 0.0;
        };
        let style = Self::strip_text_style(theme);
        let name = text_renderer.measure_style(name, None, &style).0;
        STRIP_CHEVRON_PX + f64::from(name) + 2.0 * Self::strip_name_gap(theme)
    }

    /// Paint the shown strip's name between `x0` and `x1`, after the
    /// workspace pill: the separator dim, the name elided to the room left.
    /// The separator and the name are one target, which opens the strip
    /// switcher ([`ChromeIntent::ShowStrips`]).
    pub(super) fn draw_strip_name(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        span: (f64, f64),
    ) {
        self.ws_strip_rect = Rect::ZERO;
        let Some(name) = self.strip_name().map(str::to_owned) else {
            return;
        };
        let (x0, x1) = span;
        let style = Self::strip_text_style(theme);
        let gap = Self::strip_name_gap(theme);
        let name_x = x0 + gap + STRIP_CHEVRON_PX + gap;
        if x1 - name_x < BAR_CONTROL_PX {
            return;
        }
        let name =
            text_renderer.elide_to_width_style(&name, ui::num::f32_of_f64(x1 - name_x), &style);
        let (name_w, name_h) = text_renderer.measure_style(&name, None, &style);
        let colors = &theme.tokens.colors;
        let chevron_x = x0 + gap;
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::new(
                chevron_x,
                center_y - STRIP_CHEVRON_PX / 2.0,
                chevron_x + STRIP_CHEVRON_PX,
                center_y + STRIP_CHEVRON_PX / 2.0,
            ),
            ui::icons::Icon::ChevronRight,
            colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        text_renderer.draw_style(
            scene,
            &name,
            colors.text_primary,
            at(transform, (name_x, center_y - f64::from(name_h) / 2.0)),
            None,
            &style,
        );
        self.ws_strip_rect = Rect::new(
            x0,
            center_y - BAR_CONTROL_PX / 2.0,
            name_x + f64::from(name_w) + gap,
            center_y + BAR_CONTROL_PX / 2.0,
        );
    }

    /// Where the strip's name was painted this frame, at the bar's height:
    /// its part's bounds.
    pub(super) fn strips_painted(&self) -> Vec<Rect> {
        std::iter::once(self.ws_strip_rect)
            .filter(|rect| !rect.is_zero_area())
            .map(|rect| {
                Rect::new(
                    rect.x0 - 4.0,
                    self.rect.y0 - 2.0,
                    rect.x1 + 4.0,
                    self.rect.y1 + 2.0,
                )
            })
            .collect()
    }

    pub(super) fn strips_key(&self) -> StripsKey {
        StripsKey {
            shown: self.strip_name().map(str::to_owned),
            naming: self
                .strips
                .naming
                .as_ref()
                .map(|n| (n.text.clone(), n.selection.anchor, n.selection.cursor)),
        }
    }

    fn strip_text_style(theme: &Theme) -> TextStyle {
        theme.tokens.typography.style(TextRole::Label)
    }

    /// The field the workspace's new name is typed in, where its name
    /// stands.
    pub(super) fn paint_workspace_name_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        field: Rect,
    ) {
        self.strips.field_rect = field;
        let Some(naming) = self.strips.naming.as_mut() else {
            return;
        };
        let placeholder = t("topbar-workspace-name-placeholder");
        let face = FieldFace::new(field, &naming.text)
            .placeholder(&placeholder)
            .text_role(TextRole::Caption)
            .focused(true);
        if naming.text.is_empty() {
            face.paint(scene, transform, text_renderer, theme);
        } else {
            face.paint_chrome(scene, transform, theme);
        }
        let input = naming.input.get_or_insert_with(|| {
            TextInput::new(Vec::new())
                .with_plain_chrome()
                .with_spellcheck(false)
                .with_autocorrect(false)
                .with_text_role(TextRole::Caption)
        });
        input.set_text(&naming.text);
        input.set_selection(naming.selection.anchor, naming.selection.cursor);
        input.set_focus(true);
        input.apply_layout_external(face.text_rect(theme));
        let input_theme = theme
            .clone()
            .with_text_scale(theme.text_scale * theme.ui_scale);
        input.draw(
            scene,
            transform,
            text_renderer,
            &input_theme,
            &ui::presence::PresenceCtx::default(),
        );
    }

    /// Let the painted name field take a pointer event, and copy its
    /// selection back into the name the bar owns.
    pub(super) fn field_pointer(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let Some(naming) = self.strips.naming.as_mut() else {
            return false;
        };
        let Some(input) = naming.input.as_mut() else {
            return false;
        };
        let took = input.handle_chrome_pointer(event, scale_factor, modifiers);
        if took && let Some((anchor, cursor)) = input.selection_anchor_focus_public() {
            naming.selection = ui::text_selection::Selection { anchor, cursor };
        }
        took
    }

    /// Start renaming the workspace in place, from its current name, as the
    /// workspace menu's Rename asks.
    pub fn begin_workspace_rename(&mut self) {
        let text = self.workspace_name.clone();
        let end = text.len();
        self.strips.naming = Some(Naming {
            text,
            selection: Selection {
                anchor: 0,
                cursor: end,
            },
            input: None,
        });
        self.cache_fp = super::CacheFingerprint::default();
    }

    pub(super) fn cancel_workspace_rename(&mut self) -> bool {
        let cancelled = self.strips.naming.take().is_some();
        if cancelled {
            self.strips.field_rect = Rect::ZERO;
        }
        cancelled
    }

    fn commit_workspace_rename(&mut self) {
        let Some(naming) = self.strips.naming.take() else {
            return;
        };
        self.strips.field_rect = Rect::ZERO;
        let name: String = naming.text.trim().chars().take(NAME_MAX_CHARS).collect();
        if name.is_empty() {
            return;
        }
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.intent(ChromeIntent::RenameWorkspace { name });
        }
    }

    /// Keys while the name is typed. Every key is the field's; returns
    /// whether the event was one.
    fn workspace_rename_key(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let Some(naming) = self.strips.naming.as_mut() else {
            return false;
        };
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        if key.state != winit::event::ElementState::Pressed {
            return true;
        }
        let mods = Mods::from_state(modifiers.state());
        match text_entry::apply(
            &key.logical_key,
            mods,
            &mut naming.text,
            &mut naming.selection,
        ) {
            Edit::Submit => self.commit_workspace_rename(),
            Edit::Cancel => {
                self.cancel_workspace_rename();
            }
            Edit::Changed => {
                if naming.text.chars().count() > NAME_MAX_CHARS {
                    naming.text = naming.text.chars().take(NAME_MAX_CHARS).collect();
                    let end = naming.text.len();
                    naming.selection = Selection::collapsed(end);
                }
            }
            Edit::Ignored | Edit::ConsumedQuiet => {}
        }
        true
    }

    pub(super) fn workspace_rename_test_rects(&self) -> Vec<(String, Rect)> {
        vec![("topbar_workspace_name_field".into(), self.strips.field_rect)]
    }

    /// The name field for assistive technology, while it is typed.
    pub(super) fn workspace_rename_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let naming = self.strips.naming.as_ref()?;
        Some(
            AccessNode::new(Role::TextInput)
                .with_hashed_key("topbar-workspace-name")
                .with_name(t("topbar-workspace-name-placeholder"))
                .with_value(naming.text.clone())
                .with_bounds(self.strips.field_rect)
                .with_state(AccessState {
                    focusable: true,
                    focused: true,
                    ..Default::default()
                }),
        )
    }

    /// While the workspace's name is typed, the field owns the keyboard and
    /// a press anywhere else ends it. Returns whether the event was
    /// consumed.
    pub(super) fn handle_workspace_rename_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if self.strips.naming.is_none() {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Some(ctx.logical(position));
                // The name field is painted by the toolkit's TextInput but
                // lives outside the retained tree; its pointer path is
                // forwarded here so caret placement, drag selection and the
                // context menu reach the same editor that paints it.
                self.field_pointer(event, ctx.scale_factor, *ctx.modifiers);
                false
            }
            WindowEvent::KeyboardInput { .. } => {
                let consumed = self.workspace_rename_key(event, *ctx.modifiers);
                *ctx.handled |= consumed;
                consumed
            }
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                ..
            } => {
                let inside = self
                    .last_cursor
                    .is_some_and(|point| self.strips.field_rect.contains(point));
                if inside {
                    self.field_pointer(event, ctx.scale_factor, *ctx.modifiers);
                    *ctx.handled = true;
                    return true;
                }
                self.field_pointer(event, ctx.scale_factor, *ctx.modifiers);
                self.cancel_workspace_rename();
                false
            }
            WindowEvent::MouseInput { .. } => {
                // A release ends a drag that may have left the field.
                self.field_pointer(event, ctx.scale_factor, *ctx.modifiers);
                false
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::{DesktopChromeSnapshot, DesktopChromeSource};
    use std::cell::RefCell;
    use std::rc::Rc;
    use tilo_desktop_model::{Collaborator, Strip, Tab, TileContent, WindowGroup, WindowWidth};
    use tilo_desktop_model::{Workspace, WorkspaceId};
    use ui::headless::Framed;

    struct Host {
        tree: RefCell<DesktopTree>,
        intents: RefCell<Vec<ChromeIntent>>,
    }

    impl DesktopChromeSource for Host {
        fn snapshot(&self) -> DesktopChromeSnapshot {
            DesktopChromeSnapshot::default()
        }
        fn tree(&self) -> DesktopTree {
            self.tree.borrow().clone()
        }
        fn intent(&self, intent: ChromeIntent) -> bool {
            self.intents.borrow_mut().push(intent);
            true
        }
    }

    fn group(id: u64) -> WindowGroup {
        WindowGroup {
            id,
            active_tab: id.saturating_add(1000),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
            tabs: vec![Tab {
                id: id.saturating_add(1000),
                title: "Notes".into(),
                app_title: None,
                tile: TileContent::AppPlaceholder {
                    kind: tilo_desktop_model::AppKind::Other("so.tilo.notes".into()),
                    label: "Notes".into(),
                },
            }],
        }
    }

    /// A workspace of three named strips, the first shown, with people on
    /// the other two.
    fn tree() -> DesktopTree {
        let mut workspace = Workspace::new(WorkspaceId(1), "Room");
        let mut games = Strip::new(StripId(11), "Games");
        games.window_groups = vec![group(1)];
        let mut quiet = Strip::new(StripId(12), "Quiet");
        quiet.window_groups = vec![group(2)];
        workspace.strips = vec![Strip::new(StripId(10), "Main"), games, quiet];
        let mut ada = Collaborator::new("ada", "Ada");
        ada.active_tab = Some(1001);
        let mut bo = Collaborator::new("bo", "Bo");
        bo.active_tab = Some(1002);
        workspace.collaborators = vec![ada, bo];
        let mut tree = DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        };
        tree.active_strip.insert(WorkspaceId(1), StripId(10));
        tree
    }

    fn bar(tree: &DesktopTree, width: u32) -> (Framed<TopBarChrome>, Rc<Host>) {
        let chrome = Rc::new(Host {
            tree: RefCell::new(tree.clone()),
            intents: RefCell::default(),
        });
        let bar =
            TopBarChrome::from_tree_with_local_user(tree, "me".into()).with_chrome(chrome.clone());
        let frame = Framed::new(ui::State::new(), bar, (width, 480), Theme::dark());
        (frame, chrome)
    }

    #[test]
    fn one_strip_picker_the_bar_names_only_the_strip_shown_and_opens_the_switcher() {
        let tree = tree();
        let (mut frame, chrome) = bar(&tree, 1600);
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        // Only the strip shown, named after the workspace.
        assert_eq!(frame.strip_name(), Some("Main"));
        let crumb = frame.ws_strip_rect;
        assert!(!crumb.is_zero_area(), "Workspace › Strip");
        assert!(crumb.x0 >= frame.ws_pill_rect.x1 - 0.5);
        // No other strip, and no count of who is on one, anywhere in the
        // bar: not as a target, not for assistive technology.
        let rects = frame.test_rects();
        for other in ["Games", "Quiet"] {
            assert!(
                rects
                    .iter()
                    .all(|(name, rect)| !name.contains(other) || rect.is_zero_area()),
                "the bar lists no other strip: {rects:?}"
            );
        }
        let spoken = format!("{:?}", frame.accessibility());
        assert!(
            !spoken.contains("Games") && !spoken.contains("Quiet"),
            "nothing about the other strips is spoken from the bar"
        );
        assert!(
            !spoken.contains("people there") && !spoken.contains("person there"),
            "no people counts in the bar"
        );
        // The crumb opens the switcher, the one place strips are picked.
        Framed::harness_mut(&mut frame).click_at(crumb.center().x, crumb.center().y);
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert_eq!(
            chrome.intents.borrow().as_slice(),
            [ChromeIntent::ShowStrips]
        );
        let audit = Framed::harness(&frame).native_app_audit();
        assert!(audit.is_certified(), "{audit:?}");
    }

    /// Renaming the workspace types over the workspace's name, not over the
    /// strip's name after it, which stays where the person sees it.
    #[test]
    fn the_workspace_is_renamed_where_its_name_stands() {
        let mut one = tree();
        one.workspaces[0].strips.truncate(1);
        let (mut frame, chrome) = bar(&one, 1440);
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        let pill = frame.ws_pill_rect;
        let strip_name = frame.ws_strip_rect;
        assert!(!pill.is_zero_area() && !strip_name.is_zero_area());

        frame.begin_workspace_rename();
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        let field = frame.strips.field_rect;
        assert!(!field.is_zero_area());
        assert!(
            (field.x0 - pill.x0).abs() < 0.5,
            "the field starts where the workspace's name did: {field:?} vs {pill:?}"
        );
        assert!(
            field.x1 <= frame.ws_strip_rect.x0 + 0.5,
            "and the strip's name still follows it: {field:?} then {:?}",
            frame.ws_strip_rect
        );
        assert!(
            !frame.ws_strip_rect.is_zero_area(),
            "the strip keeps its name in the bar"
        );

        Framed::harness_mut(&mut frame).key("Escape");
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert_eq!(frame.ws_pill_rect, pill, "the name is back as it was");
        assert!(frame.strips.field_rect.is_zero_area());

        // The current name is selected: typing replaces it.
        frame.begin_workspace_rename();
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        let field = frame.strips.field_rect.center();
        Framed::harness_mut(&mut frame).click_at(field.x, field.y);
        Framed::harness_mut(&mut frame).type_text("Lounge");
        Framed::harness_mut(&mut frame).key("Enter");
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert_eq!(
            chrome.intents.borrow().last(),
            Some(&ChromeIntent::RenameWorkspace {
                name: "Lounge".into()
            })
        );
    }

    #[test]
    fn a_narrow_bar_keeps_the_strip_named_and_passes_the_native_audit() {
        let (frame, _) = bar(&tree(), 360);
        let audit = Framed::harness(&frame).native_app_audit();
        assert!(audit.is_certified(), "{audit:?}");
    }
}

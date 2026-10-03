//! The content column: the open snippet's language, folder and tags, what it
//! is for, its main action, and its body in the code editor; and the two
//! actions the column's header holds (favorite, more).
//!
//! Every control is the toolkit's retained widget, so focus, keyboard
//! activation, pressed states and tooltips are the toolkit's. The column is
//! rebuilt (through `NavShell::set_content`) when what it shows changes;
//! the header actions follow the open snippet in place.

use std::collections::HashMap;
use std::rc::Rc;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};

use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::{
    AppModel, Button, ButtonRole, Chip, ChipLead, Column, ControlSize, Element, EmptyState,
    ModelPath, Row, Text, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::app::{Action, Core, Requests, Shared};
use crate::model::{Language, SnippetId};
use crate::tr;

/// What the content column shows. A change rebuilds the column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DetailKey {
    /// The open snippet's id, language, folder, tags, description and
    /// whether its body has variables.
    snippet: Option<(SnippetId, Language, String, Vec<String>, String, bool)>,
    library_empty: bool,
}

impl DetailKey {
    pub(crate) fn of(core: &Core) -> Self {
        Self {
            snippet: core.open.and_then(|id| core.library.get(id)).map(|s| {
                (
                    s.id,
                    s.language,
                    s.folder.clone(),
                    s.tags.clone(),
                    s.description.clone(),
                    s.has_placeholders(),
                )
            }),
            library_empty: core.library.snippets.is_empty(),
        }
    }
}

/// Build the content column for what `core` has open.
pub(crate) fn build(core: &Shared, requests: &Requests) -> Element {
    let key = DetailKey::of(&core.borrow());
    if key.library_empty {
        // A fresh library: the one thing to do is create a snippet.
        return Box::new(EmptyAction {
            state: EmptyState::new(tr("snippets-empty-library"))
                .with_icon(Icon::Code)
                .with_action(tr("snippets-new-snippet"))
                .with_action_id("snippets.library.new-snippet"),
            requests: Rc::clone(requests),
        });
    }
    let Some((id, language, folder, tags, description, templated)) = key.snippet else {
        return Box::new(EmptyState::new(tr("snippets-none-selected")).with_icon(Icon::Code));
    };

    // The facts: the language (a button that opens its picker), then the
    // folder and tags.
    let picker = {
        let requests = Rc::clone(requests);
        Button::new(
            &format!("snippets.snippet:{id}.language"),
            &language.name(),
            None,
        )
        .with_icon(Icon::Code)
        .control_size(ControlSize::Small)
        .tooltip(tr("snippets-language"))
        .on_effect(move || requests.borrow_mut().push(Action::PickLanguage(id)))
    };
    let mut facts = Row::new().push(Box::new(Anchored {
        button: picker,
        core: Rc::clone(core),
        part: Part::Language,
        shown: None,
    }));
    if !folder.is_empty() {
        facts = facts.push(Box::new(
            Chip::new(folder).with_lead(ChipLead::Glyph(Icon::Folder)),
        ));
    }
    for tag in tags {
        facts = facts.push(Box::new(Chip::new(tag)));
    }
    let mut about = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(facts));
    if !description.trim().is_empty() {
        about = about.push(Box::new(
            Text::new(&description).with_role(TextRole::Body).dimmed(),
        ));
    }
    let about = Gapped {
        column: about,
        gap: |rhythm| rhythm.sibling,
    };

    // The main action: copy, through the fill-in dialog when the body has
    // variables.
    let copy = {
        let requests = Rc::clone(requests);
        Button::new(
            &format!("snippets.snippet:{id}.copy"),
            &tr(if templated {
                "snippets-fill-and-copy"
            } else {
                "snippets-copy"
            }),
            None,
        )
        .role(ButtonRole::Primary)
        .on_effect(move || requests.borrow_mut().push(Action::Copy(id)))
    };

    Box::new(Gapped {
        column: Column::new()
            .with_id("snippets.detail")
            .push(Box::new(about))
            .push(Box::new(copy))
            .push(Box::new(BodyWell {
                core: Rc::clone(core),
                rect: Rect::ZERO,
            })),
        gap: |rhythm| rhythm.group,
    })
}

/// A column whose gap is a step of the theme's rhythm.
struct Gapped {
    column: Column,
    gap: fn(&Rhythm) -> f32,
}

impl ui::widget::Decorator for Gapped {
    type Inner = Column;

    fn inner(&self) -> &Column {
        &self.column
    }

    fn inner_mut(&mut self) -> &mut Column {
        &mut self.column
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.column
            .set_gap((self.gap)(&theme.tokens.spacing.rhythm));
        Widget::build_layout(&mut self.column, taffy, theme, node_map)
    }

    fn name(&self) -> &'static str {
        "SnippetColumn"
    }
}

/// A button that acts on the open snippet, from the header row.
fn open_snippet_button(
    core: &Shared,
    requests: &Requests,
    button: Button,
    action: fn(SnippetId) -> Action,
) -> Button {
    let core = Rc::clone(core);
    let requests = Rc::clone(requests);
    button.on_effect(move || {
        if let Some(id) = core.try_borrow().ok().and_then(|core| core.open) {
            requests.borrow_mut().push(action(id));
        }
    })
}

/// The header's favorite toggle for the open snippet: a star that reads as
/// on while the snippet is a favorite.
pub(crate) fn favorite_button(core: &Shared, requests: &Requests) -> Element {
    let button = Button::icon_only("snippets.favorite", Icon::Star, &tr("snippets-favorite"));
    Box::new(Anchored {
        button: open_snippet_button(core, requests, button, Action::ToggleFavorite),
        core: Rc::clone(core),
        part: Part::Favorite,
        shown: None,
    })
}

/// The header's overflow button: the open snippet's less frequent actions.
pub(crate) fn more_button(core: &Shared, requests: &Requests) -> Element {
    let button = Button::icon_only(
        "snippets.more",
        Icon::MoreHorizontal,
        &tr("snippets-more-actions"),
    );
    Box::new(Anchored {
        button: open_snippet_button(core, requests, button, Action::More),
        core: Rc::clone(core),
        part: Part::More,
        shown: None,
    })
}

/// Which of the app's buttons an [`Anchored`] is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Language,
    Favorite,
    More,
}

/// One of the app's buttons that the core follows: the header's buttons
/// are disabled while no snippet is open and the favorite toggle reads the
/// snippet's state; the buttons that open a menu tell the core where they
/// are, so the menu opens under them.
struct Anchored {
    button: Button,
    core: Shared,
    part: Part,
    /// The (enabled, on) state last applied.
    shown: Option<(bool, bool)>,
}

impl Anchored {
    fn refresh(&mut self) {
        if self.part == Part::Language {
            return;
        }
        let Ok(core) = self.core.try_borrow() else {
            return;
        };
        let open = core.open.and_then(|id| core.library.get(id));
        let state = (open.is_some(), open.is_some_and(|s| s.favorite));
        drop(core);
        if self.shown == Some(state) {
            return;
        }
        self.shown = Some(state);
        let (enabled, on) = state;
        self.button.set_enabled(enabled);
        if self.part == Part::Favorite {
            self.button.set_toggled(on);
            self.button.set_tooltip(Some(tr(if on {
                "snippets-remove-favorite"
            } else {
                "snippets-add-favorite"
            })));
        }
    }
}

impl ui::widget::Decorator for Anchored {
    type Inner = Button;

    fn inner(&self) -> &Button {
        &self.button
    }

    fn inner_mut(&mut self) -> &mut Button {
        &mut self.button
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.refresh();
        Widget::sync(&mut self.button, state, path);
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        self.refresh();
        Widget::update(&mut self.button, state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = Widget::build_layout(&mut self.button, taffy, theme, node_map);
        ui::widget::decorator::register_node(node_map, node, self);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        Widget::apply_layout(&mut self.button, taffy, node, origin);
        let rect = Widget::get_rect(&self.button);
        if let Ok(mut core) = self.core.try_borrow_mut() {
            match self.part {
                Part::Language => core.language_anchor = rect,
                Part::More => core.more_anchor = rect,
                Part::Favorite => {}
            }
        }
    }

    fn name(&self) -> &'static str {
        match self.part {
            Part::Language => "SnippetLanguage",
            Part::Favorite => "SnippetFavorite",
            Part::More => "SnippetMore",
        }
    }
}

/// The empty library's call to action, routed to the app's "New snippet".
struct EmptyAction {
    state: EmptyState,
    requests: Requests,
}

impl EmptyAction {
    fn drain(&mut self) {
        if self.state.take_action() {
            self.requests.borrow_mut().push(Action::NewSnippet);
        }
    }
}

impl ui::widget::Decorator for EmptyAction {
    type Inner = EmptyState;

    fn inner(&self) -> &EmptyState {
        &self.state
    }

    fn inner_mut(&mut self) -> &mut EmptyState {
        &mut self.state
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = Widget::build_layout(&mut self.state, taffy, theme, node_map);
        ui::widget::decorator::register_node(node_map, node, self);
        node
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        self.drain();
        Widget::update(&mut self.state, state, path)
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        Widget::handle_event(&mut self.state, event, ctx);
        self.drain();
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        root: &ModelPath,
    ) -> bool {
        let done = Widget::perform_action(&mut self.state, action, state, root);
        self.drain();
        done
    }

    fn name(&self) -> &'static str {
        "SnippetsEmpty"
    }
}

/// The body: the code editor in a raised well that takes the rest of the
/// column. The editor lives in the core, which routes its input.
struct BodyWell {
    core: Shared,
    rect: Rect,
}

impl Widget for BodyWell {
    #[allow(
        clippy::expect_used,
        reason = "TaffyTree::new_leaf only fails on allocation, which aborts anyway"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let lines = 4.0 * theme.text(TextRole::Code).line_height_px();
        let node = taffy
            .new_leaf(Style {
                flex_grow: 1.0,
                flex_basis: length(0.0_f32),
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: taffy::Dimension::auto(),
                },
                min_size: taffy::Size {
                    width: length(0.0_f32),
                    height: length(lines),
                },
                ..Default::default()
            })
            .expect("a leaf node");
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::draw::leaf_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.height() <= 0.0 || self.rect.width() <= 0.0 {
            return;
        }
        let ground = theme.tokens.colors.surfaces.raised;
        ui::surface::card(
            scene,
            transform,
            self.rect,
            f64::from(theme.tokens.radii.control),
            ground,
        );
        let mut core = self.core.borrow_mut();
        core.body_editor.set_rect(self.rect);
        core.body_editor
            .draw_embedded(scene, transform, text, &theme.on_ground(ground));
    }

    fn needs_redraw(&self) -> bool {
        // The editor's caret, selection and text change without a layout.
        true
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "SnippetBody"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

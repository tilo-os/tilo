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
use ui::widget::TaffyTreeExt;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};

use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::{
    AppModel, Button, ButtonRole, Chip, ChipLead, Column, ControlSize, Dropdown, Element,
    EmptyState, ModelPath, Row, Text, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::app::{Action, Core, Requests, Shared};
use crate::model::Snippets;
use crate::model::{Language, SnippetId};
use crate::resource::SnippetsView;
use crate::tr;
use ui::tilo_actions::ActionBinding;
use ui::tilo_resource::{Derived, Effect};

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
            snippet: core.open.as_ref().and_then(|id| {
                let s = core.library.get(id)?;
                Some((
                    id.clone(),
                    s.language,
                    s.folder.0.clone(),
                    s.tags.iter().cloned().collect(),
                    s.description.clone(),
                    s.has_placeholders(),
                ))
            }),
            library_empty: core.library.is_empty(),
        }
    }

    /// Whether a snippet is open.
    pub(crate) fn has_snippet(&self) -> bool {
        self.snippet.is_some()
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
                .with_action(tr("snippets-new"))
                .with_action_id("snippets.library.new-snippet"),
            requests: Rc::clone(requests),
        });
    }
    let Some((id, language, folder, tags, description, templated)) = key.snippet else {
        return Box::new(EmptyState::new(tr("snippets-none-selected")).with_icon(Icon::Code));
    };

    // The facts: the language (a picker over every language), then the
    // folder and tags.
    let picker = {
        let requests = Rc::clone(requests);
        let id = id.clone();
        Dropdown::from_state(
            Language::ALL.into_iter().map(Language::name).collect(),
            &ui::LocalState::new(language.name()),
        )
        .with_id(&format!("snippets.snippet:{id}.language"))
        .with_resolved_accessible_name(tr("snippets-language"))
        .leading_icon(Icon::Code)
        .control_size(ControlSize::Small)
        .on_change(move |name| {
            if let Some(language) = Language::ALL.into_iter().find(|l| l.name() == name) {
                requests
                    .borrow_mut()
                    .push(Action::SetLanguage(id.clone(), language));
            }
        })
    };
    let mut facts = Row::new().push(Box::new(LanguagePicker {
        dropdown: picker,
        core: Rc::clone(core),
    }));
    // The folder and each tag reveal their snippets, as the place each is
    // published as does.
    let reveal = |place: Derived<Snippets>, view: SnippetsView| {
        ActionBinding::unit_for(&place.publish(), view.id()).ok()
    };
    if !folder.is_empty() {
        let mut chip = Chip::new(folder.clone())
            .with_id(format!("snippets.snippet:{id}.folder"))
            .with_lead(ChipLead::Glyph(Icon::Folder))
            .with_accessible_name(format!("Folder: {folder}"));
        if let Some(reveal) = reveal(
            Snippets::folder_place(folder.clone()),
            SnippetsView::RevealFolder { folder },
        ) {
            chip = chip.on_action(reveal);
        }
        facts = facts.push(Box::new(chip));
    }
    for tag in tags {
        let mut chip = Chip::new(tag.clone())
            .with_id(format!("snippets.snippet:{id}.tag:{tag}"))
            .with_accessible_name(format!("Tag: {tag}"));
        if let Some(reveal) = reveal(
            Snippets::tag_place(tag.clone()),
            SnippetsView::Reveal { tag: tag.clone() },
        ) {
            chip = chip.on_action(reveal);
        }
        facts = facts.push(Box::new(chip));
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
    let about = about.with_rhythm(Rhythm::Sibling);

    // The main action: copy, through the fill-in dialog when the body has
    // variables.
    let copy = {
        let requests = Rc::clone(requests);
        let copied = id.clone();
        Button::new(
            &format!("snippets.snippet:{id}.copy"),
            &tr(if templated {
                "snippets-fill-and-copy"
            } else {
                "snippets-copy"
            }),
        )
        .role(ButtonRole::Primary)
        .on_effect(move || requests.borrow_mut().push(Action::Copy(copied.clone())))
    };

    Box::new(
        Column::new()
            .with_id("snippets.detail")
            .with_rhythm(Rhythm::Group)
            .push(Box::new(about))
            .push(Box::new(copy))
            .push(Box::new(BodyWell {
                core: Rc::clone(core),
                rect: Rect::ZERO,
            })),
    )
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
        if let Some(id) = core.try_borrow().ok().and_then(|core| core.open.clone()) {
            requests.borrow_mut().push(action(id));
        }
    })
}

/// The content header's actions: the open snippet's favorite toggle and
/// overflow menu, or none while no snippet is open.
pub(crate) fn header_actions(core: &Shared, requests: &Requests, open: bool) -> Vec<Element> {
    if open {
        vec![favorite_button(core, requests), more_button(core, requests)]
    } else {
        Vec::new()
    }
}

/// The header's favorite toggle for the open snippet: a star that reads as
/// on while the snippet is a favorite.
fn favorite_button(core: &Shared, requests: &Requests) -> Element {
    let button = Button::icon_only("snippets.favorite", Icon::Star, &tr("snippets-favorite"));
    Box::new(Anchored {
        button: open_snippet_button(core, requests, button, Action::ToggleFavorite),
        core: Rc::clone(core),
        part: Part::Favorite,
        shown: None,
    })
}

/// The header's overflow button: the open snippet's less frequent actions.
fn more_button(core: &Shared, requests: &Requests) -> Element {
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

/// Which of the header's buttons an [`Anchored`] is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Favorite,
    More,
}

/// One of the header's buttons that the core follows: the favorite toggle
/// reads the snippet's state, and the overflow button tells the core where
/// it is, so its menu opens under it.
struct Anchored {
    button: Button,
    core: Shared,
    part: Part,
    /// The (enabled, on) state last applied.
    shown: Option<(bool, bool)>,
}

impl Anchored {
    fn refresh(&mut self) {
        let Ok(core) = self.core.try_borrow() else {
            return;
        };
        let open = core.open.as_ref().and_then(|id| core.library.get(id));
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

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.refresh();
        Widget::sync(&mut self.button, state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
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
        if self.part == Part::More
            && let Ok(mut core) = self.core.try_borrow_mut()
        {
            core.more_anchor = rect;
        }
    }

    fn name(&self) -> &'static str {
        match self.part {
            Part::Favorite => "SnippetFavorite",
            Part::More => "SnippetMore",
        }
    }
}

/// The language picker; it opens when the core asks for it (the palette's
/// or a capture's "pick a language").
struct LanguagePicker {
    dropdown: Dropdown,
    core: Shared,
}

impl LanguagePicker {
    fn open_if_asked(&mut self) {
        let asked = self
            .core
            .try_borrow_mut()
            .is_ok_and(|mut core| std::mem::take(&mut core.pick_language));
        if asked && !self.dropdown.is_open() {
            self.dropdown.open_menu();
        }
    }
}

impl ui::widget::Decorator for LanguagePicker {
    type Inner = Dropdown;

    fn inner(&self) -> &Dropdown {
        &self.dropdown
    }

    fn inner_mut(&mut self) -> &mut Dropdown {
        &mut self.dropdown
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = Widget::build_layout(&mut self.dropdown, taffy, theme, node_map);
        ui::widget::decorator::register_node(node_map, node, self);
        node
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.open_if_asked();
        Widget::update(&mut self.dropdown, state, path)
    }

    fn name(&self) -> &'static str {
        "SnippetLanguage"
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

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
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
        _key_path: &[u64],
        state: &mut ui::widget::Edits<'_>,
        root: &ModelPath,
    ) -> bool {
        let done = Widget::perform_action(&mut self.state, action, &[], state, root);
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
        let style = Style {
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
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
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
        core.body.control_mut().set_rect(self.rect);
        core.body
            .control_mut()
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

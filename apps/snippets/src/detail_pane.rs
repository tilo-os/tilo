//! The content column: the open snippet's language, folder and tags, what it
//! is for, its main action, and its body in the code editor. Its favorite,
//! duplicate and delete are the window bar's (`app::window_bar`).
//!
//! Every control is the toolkit's retained widget, so focus, keyboard
//! activation, pressed states and tooltips are the toolkit's. The column is
//! rebuilt (through `NavShell::set_content`) when what it shows changes.

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
    EmptyState, ModelPath, Row, TextInput, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
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
}

/// Build the content column for what `core` has open.
pub(crate) fn build(core: &Shared, requests: &Requests) -> Element {
    let key = DetailKey::of(&core.borrow());
    if key.library_empty {
        return empty_library(requests);
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
            SnippetsView::RevealFolder {
                folder: folder.clone(),
            },
        ) {
            chip = chip.on_action(reveal);
        }
        facts = facts.push(Box::new(chip));
    }
    for tag in tags.iter() {
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
    let about = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(name_field(core, requests, &id)))
        .push(Box::new(facts))
        // What the snippet is for, and which folder it is filed under: both
        // are the library's organising model, so both are editable where the
        // snippet is read rather than only when it is created.
        .push(Box::new(field(
            core,
            requests,
            &id,
            "description",
            "snippets-description",
            "snippets-description-placeholder",
            &description,
            Action::SetDescription as fn(SnippetId, String) -> Action,
        )))
        .push(Box::new(field(
            core,
            requests,
            &id,
            "folder",
            "snippets-folder",
            "snippets-folder-placeholder",
            &folder,
            Action::SetFolder as fn(SnippetId, String) -> Action,
        )))
        .push(Box::new(tag_editor(core, requests, &id, &tags)));
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
        .on_own_device_effect(move || requests.borrow_mut().push(Action::Copy(copied.clone())))
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

/// A fresh library: what a snippet is for, and the one thing to do, create
/// one.
fn empty_library(requests: &Requests) -> Element {
    Box::new(EmptyAction {
        state: EmptyState::new(tr("snippets-empty-library"))
            .with_icon(Icon::Code)
            .with_message(tr("snippets-empty-library-message"))
            .with_action(tr("snippets-new"))
            .with_action_id("snippets.library.new-snippet"),
        requests: Rc::clone(requests),
    })
}

/// The open snippet's name, edited in place: a new snippet starts
/// untitled, and nothing else renames it.
fn name_field(core: &Shared, requests: &Requests, id: &SnippetId) -> TextInput {
    let title = core
        .borrow()
        .library
        .get(id)
        .map(|snippet| snippet.title.clone())
        .unwrap_or_default();
    let requests = Rc::clone(requests);
    let renamed = id.clone();
    TextInput::local()
        .with_id(&format!("snippets.snippet:{id}.title"))
        .with_accessible_name(tr("snippets-name"))
        .with_placeholder("snippets-untitled")
        .with_initial_value(&title)
        .on_change(move |title| {
            requests
                .borrow_mut()
                .push(Action::Rename(renamed.clone(), title.to_owned()));
        })
}

/// A single-line field of the snippet's own facts, committing on every
/// change: the title's editor, for the fields that are not the title.
fn field(
    core: &Shared,
    requests: &Requests,
    id: &SnippetId,
    name: &str,
    accessible: &str,
    placeholder: &str,
    value: &str,
    make: fn(SnippetId, String) -> Action,
) -> TextInput {
    let initial = value.to_owned();
    let requests = Rc::clone(requests);
    let changed = id.clone();
    let _ = core;
    TextInput::local()
        .with_id(&format!("snippets.snippet:{id}.{name}"))
        .with_accessible_name(&tr(accessible))
        .with_placeholder(placeholder)
        .with_initial_value(&initial)
        .on_change(move |text| {
            requests
                .borrow_mut()
                .push(make(changed.clone(), text.to_owned()));
        })
}

/// The snippet's tags, each removable, and a field to add one: the sidebar
/// groups the library by these, so they are the snippet's to change.
fn tag_editor(core: &Shared, requests: &Requests, id: &SnippetId, tags: &[String]) -> Column {
    let mut row = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Sibling);
    for tag in tags.iter() {
        let requests = Rc::clone(requests);
        let removed = id.clone();
        let name = tag.clone();
        let remove = Button::icon_only(
            &format!("snippets.snippet:{id}.tag:{tag}.remove"),
            Icon::X,
            &format!("Remove the tag {tag}"),
        )
        .control_size(ControlSize::Small)
        .on_own_device_effect(move || {
            requests
                .borrow_mut()
                .push(Action::RemoveTag(removed.clone(), name.clone()));
        });
        row = row.push(Box::new(
            Row::new()
                .push(Box::new(
                    Chip::new(tag.clone()).with_accessible_name(format!("Tag: {tag}")),
                ))
                .push(Box::new(remove)),
        ));
    }
    let requests_add = Rc::clone(requests);
    let added = id.clone();
    let add = TextInput::local()
        .with_id(&format!("snippets.snippet:{id}.new-tag"))
        .with_accessible_name(&tr("snippets-add-tag"))
        .with_placeholder(tr("snippets-add-tag-placeholder"))
        .on_change(move |text| {
            // Committed as the field is typed into and cleared by the app
            // once the tag is in, the way a tag entry field behaves.
            let tag = text.trim();
            if !tag.is_empty() {
                requests_add
                    .borrow_mut()
                    .push(Action::AddTag(added.clone(), tag.to_owned()));
            }
        });
    let _ = core;
    row.push(Box::new(add))
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
            min_size: taffy::Size::from_lengths(0.0_f32, lines),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    ui::rect_geometry!();

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
}

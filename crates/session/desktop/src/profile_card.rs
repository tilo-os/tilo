//! Who you are here: the card your own avatar in the top bar opens.
//!
//! It shows you as everyone else sees you (your avatar in your colour, your
//! name) and puts changing that name right there, in a field: type, Enter,
//! done. Below it, what keeps this identity yours (a passkey, a recovery
//! key) and your other devices (pairing a new one). Those are the host's
//! own commands, the same ones search lists, so the card shows what the
//! host offers ([`ProfileModel::actions`]) and hands a choice back by id;
//! it never carries them out itself.
//!
//! Where the room is connected to a service elsewhere (a Discord server it
//! bridges), the card lists the person's account there under "Linked
//! accounts" ([`ProfileModel::linked`]): link it, which shows the code to
//! send from the service with a Copy button, or unlink it. The host runs
//! those on the connector's room record and tells the card what came of it
//! ([`ProfileCard::set_linked`]); the card stays open meanwhile.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use ui::icons::Icon;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    Avatar, AvatarSize, Button, ButtonRole, ColorSwatch, ColorSwatchPicker, Column, Container,
    Divider, ListRow, Popover, PopoverSide, Row, Segment, Text, TextInput, Toggle, ToggleGroup,
    Widget,
};
use ui::{LocalState, TextRole};
use vello::kurbo::{Point, Rect};
use vello::peniko::Color;
use winit::event::WindowEvent;

use crate::i18n::{t, t_args};
pub use tilo_app::linked_accounts::{LinkState, LinkedAccount};

/// The words for a status preset.
/// The words for one way of sharing what you are doing.
fn sharing_key(mode: ui::StatusSharing) -> &'static str {
    match mode {
        ui::StatusSharing::Automatic => "profile-sharing-auto",
        ui::StatusSharing::ManualOnly => "profile-sharing-manual",
        ui::StatusSharing::Off => "profile-sharing-off",
    }
}

fn availability_key(availability: ui::Availability) -> &'static str {
    match availability {
        ui::Availability::Available | ui::Availability::Idle => "profile-available",
        ui::Availability::Focusing => "profile-focusing",
        ui::Availability::Away => "profile-away",
    }
}

/// The words for what people see.
fn visibility_key(visibility: ui::PresenceVisibility) -> &'static str {
    match visibility {
        ui::PresenceVisibility::WhereIAm => "profile-visibility-where",
        ui::PresenceVisibility::ThatImHere => "profile-visibility-here",
        ui::PresenceVisibility::Nothing => "profile-visibility-nothing",
    }
}

/// What a choice of what people see means, in a line.
fn visibility_detail_key(visibility: ui::PresenceVisibility) -> &'static str {
    match visibility {
        ui::PresenceVisibility::WhereIAm => "profile-visibility-where-detail",
        ui::PresenceVisibility::ThatImHere => "profile-visibility-here-detail",
        ui::PresenceVisibility::Nothing => "profile-visibility-nothing-detail",
    }
}

/// What a profile action is about, so the card can group them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileGroup {
    /// Being with the people here: saying something beside the pointer.
    Together,
    /// Keeping this identity: a passkey, a recovery key, signing in.
    Identity,
    /// The person's other devices.
    Devices,
}

/// One of the host's commands about the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileAction {
    pub id: String,
    pub title: String,
    pub group: ProfileGroup,
    /// What it is about at a glance: a passkey, a key, a device.
    pub icon: Icon,
}

/// Who the card is about, as the host knows it when it opens.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProfileModel {
    /// The name everyone here sees.
    pub name: String,
    /// The stable identity the avatar's colour and initials derive from
    /// when the person chose no colour.
    pub identity: String,
    pub colour: Option<Color>,
    pub can_rename: bool,
    /// The person has not named themselves yet (`name` is their login
    /// account's): the card asks, once, for the name people here should
    /// see, until they give one.
    pub ask_name: bool,
    pub actions: Vec<ProfileAction>,
    /// The services the room is connected to, with the person's account
    /// at each; none hides the section.
    pub linked: Vec<LinkedAccount>,
    /// How available the person says they are.
    pub availability: ui::Availability,
    /// The short status they wrote, if any.
    pub status: String,
    /// Whether what they are doing is shared on its own, only in their
    /// words, or not at all; `None` where the host keeps no such choice.
    pub sharing: Option<ui::StatusSharing>,
    /// The colour they chose, by its id; `None` while their identity's own
    /// colour is worn.
    pub colour_id: Option<String>,
    /// What the people in the workspace on screen see of them; `None`
    /// outside any shared workspace.
    pub sight: Option<WorkspaceSight>,
    /// The host keeps the person's status, colour and what people see:
    /// the card offers them.
    pub offers_presence: bool,
    /// Whether the people here see them at all: a spectator only watches,
    /// and the card says so in place of how they are seen.
    pub seen: Seen,
    /// Whether the desktop's control hints show; `None` where the host has
    /// none to offer.
    pub control_hints: Option<bool>,
}

/// Whether the people here see the person.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Seen {
    /// They are here: everyone sees them as the card shows.
    #[default]
    Here,
    /// They only watch, as the room's spectator: nobody here sees them.
    Watching,
}

/// What the people in one workspace see of the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSight {
    /// The workspace's name.
    pub workspace: String,
    pub visibility: ui::PresenceVisibility,
}

/// What the person did in the card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileChoice {
    /// Go by this name from now on (trimmed, never empty).
    Rename(String),
    /// The host's command, by id.
    Action(String),
    /// Link the person's account with this connector's service (its app
    /// id). The card stays open for the code.
    Link(String),
    /// Unlink the person's account with this connector's service.
    Unlink(String),
    Availability(ui::Availability),
    /// Say this short status (empty clears it).
    Status(String),
    /// Share what you are doing this way from now on.
    Sharing(ui::StatusSharing),
    /// Wear this colour, by its id.
    Colour(String),
    /// Let the people in the workspace on screen see this much.
    Visibility(ui::PresenceVisibility),
    /// Show the desktop's control hints, or not.
    ControlHints(bool),
}

impl ProfileChoice {
    /// Whether the card closes on it: a linked account's choice keeps it
    /// open to show what came of it.
    fn closes(&self) -> bool {
        matches!(self, Self::Rename(_) | Self::Action(_))
        // The rest are settings the card goes on showing.
    }
}

/// The card behind the bar's own avatar.
/// The card itself as a place keyboard focus can rest: opening the card is
/// not editing, so focus lands here, on the whole card, rather than in its
/// name field. A click on the field, or Tab, enters it. It lies under the
/// card's content, filling it, draws nothing and takes no input; assistive
/// technology reads it as the card.
struct CardFocus {
    rect: Rect,
    focused: bool,
}

impl CardFocus {
    fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            focused: false,
        }
    }
}

impl Widget for CardFocus {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use ui::widget::TaffyTreeExt;
        let style = taffy::Style {
            position: taffy::Position::Absolute,
            inset: taffy::Rect {
                left: taffy::LengthPercentageAuto::length(0.0),
                right: taffy::LengthPercentageAuto::length(0.0),
                top: taffy::LengthPercentageAuto::length(0.0),
                bottom: taffy::LengthPercentageAuto::length(0.0),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Group)
                .with_name(t("profile-card"))
                .with_semantic_key("profile-card-focus")
                .with_state(AccessState::new().focusable(true).focused(self.focused)),
        )
    }

    fn name(&self) -> &'static str {
        "ProfileCardFocus"
    }
}

pub struct ProfileCard {
    popover: Popover<Container>,
    /// The name being typed.
    draft: LocalState<String>,
    /// The status being typed.
    status_draft: LocalState<String>,
    /// The name the card opened with.
    shown: String,
    chosen: Rc<RefCell<Option<ProfileChoice>>>,
    focus_reset: bool,
    /// What the open card shows, to build it again when a linked account
    /// changes, and the anchor and viewport it opened at.
    model: Option<ProfileModel>,
    placed: (Rect, Rect),
    /// The card's widgets were built again (it opened, or a linked account
    /// changed while open): a layout that still maps the ones they replaced
    /// must be built again before it reaches them.
    replaced: bool,
}

impl Default for ProfileCard {
    fn default() -> Self {
        Self {
            popover: Popover::with_content(Container::new()),
            draft: LocalState::new(String::new()),
            status_draft: LocalState::new(String::new()),
            shown: String::new(),
            chosen: Rc::default(),
            focus_reset: false,
            model: None,
            placed: (Rect::ZERO, Rect::ZERO),
            replaced: false,
        }
    }
}

impl ProfileCard {
    pub fn new() -> Self {
        Self::default()
    }

    /// A field for the person's name: submitting a non-empty one renames them.
    fn name_field(&self, id: &str, placeholder: String, accessible: String) -> TextInput {
        let chosen = Rc::clone(&self.chosen);
        TextInput::from_state(&self.draft)
            .with_id(id)
            .with_placeholder(placeholder)
            .with_accessible_name(accessible)
            .on_submit(move |typed| {
                let typed = typed.trim();
                if !typed.is_empty() {
                    *chosen.borrow_mut() = Some(ProfileChoice::Rename(typed.to_owned()));
                }
            })
    }

    fn body(&self, model: &ProfileModel) -> Column {
        let mut avatar = Avatar::person(&model.name, &model.identity).size(AvatarSize::Large);
        if let Some(colour) = model.colour {
            avatar = avatar.with_presence(colour);
        }
        let name: Box<dyn Widget> = if model.can_rename {
            Box::new(self.name_field(
                "profile-name",
                t("profile-name-placeholder"),
                t("profile-name"),
            ))
        } else {
            // A name with no spaces (a key, before one is chosen) still
            // wraps inside the card.
            Box::new(
                Text::new(&model.name)
                    .with_role(TextRole::Subheading)
                    .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph),
            )
        };
        // Not named yet: in place of the hint, a field asking for the name
        // people here should see. Gone once they give one.
        let below: Box<dyn Widget> = if model.ask_name && !model.can_rename {
            Box::new(self.name_field(
                "profile-add-name",
                t("profile-add-your-name"),
                t("profile-add-your-name"),
            ))
        } else {
            let hint = if model.seen == Seen::Watching {
                "profile-watching-hint"
            } else {
                "profile-name-hint"
            };
            Box::new(Text::new(&t(hint)).with_role(TextRole::Caption).dimmed())
        };
        let who = Column::new()
            .with_no_padding()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .push(name)
            .push(below);
        let header = Row::new()
            .no_wrap()
            .with_align_items(taffy::AlignItems::Center)
            .push(Box::new(avatar))
            .push(Box::new(who));
        let mut body = Column::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .with_padding_rhythm(ui::tokens::Rhythm::Related, ui::tokens::Rhythm::Related)
            // First in the card's focus order: the card opens focused on
            // itself, not in its name field.
            .push(Box::new(CardFocus::new()))
            .push(Box::new(header));
        if model.offers_presence {
            body = self.presence_sections(body, model);
        } else if model.sharing.is_some() {
            body = self.page_sharing(body, model);
        }
        for (group, heading) in [
            (ProfileGroup::Together, None),
            (ProfileGroup::Identity, Some("profile-section-identity")),
            (ProfileGroup::Devices, Some("profile-section-devices")),
        ] {
            let actions: Vec<&ProfileAction> = model
                .actions
                .iter()
                .filter(|action| action.group == group)
                // One clear way to keep who you are: the host's first
                // (it lists them best first). The rest stay in search, so
                // three overlapping asks never compete here.
                .take(if group == ProfileGroup::Identity {
                    1
                } else {
                    usize::MAX
                })
                .collect();
            if actions.is_empty() {
                continue;
            }
            body = body.push(Box::new(Divider::horizontal()));
            if let Some(heading) = heading {
                body = body.push(Box::new(
                    Text::new(&t(heading)).with_role(TextRole::Caption).dimmed(),
                ));
            }
            for action in actions {
                let chosen = Rc::clone(&self.chosen);
                let id = action.id.clone();
                body = body.push(Box::new(
                    Button::new(&format!("profile-action-{}", action.id), &action.title)
                        .with_icon(action.icon)
                        .role(ButtonRole::Plain)
                        .on_click(move || {
                            *chosen.borrow_mut() = Some(ProfileChoice::Action(id.clone()));
                        }),
                ));
            }
        }
        if !model.linked.is_empty() {
            body = body.push(Box::new(Divider::horizontal())).push(Box::new(
                Text::new(&t("profile-section-linked"))
                    .with_role(TextRole::Caption)
                    .dimmed(),
            ));
            for account in &model.linked {
                body = body.push(self.linked_row(account));
            }
        }
        self.push_control_hints(body, model.control_hints)
    }

    /// The one setting the desktop's control hints have, below a divider,
    /// where the host offers them.
    fn push_control_hints(&self, body: Column, on: Option<bool>) -> Column {
        let Some(on) = on else {
            return body;
        };
        let body = body.push(Box::new(Divider::horizontal()));
        let state = LocalState::new(on);
        let chosen = Rc::clone(&self.chosen);
        let label = t("profile-control-hints");
        body.push(Box::new(
            ListRow::new("profile-control-hints", label.clone()).trailing(Box::new(
                Toggle::from_state(&state)
                    .with_id("profile-control-hints-toggle")
                    .with_accessible_name(&label)
                    .on_change(move |on| {
                        *chosen.borrow_mut() = Some(ProfileChoice::ControlHints(on));
                    }),
            )),
        ))
    }

    /// Where the host keeps only how what they do is shared (a page): what
    /// they are doing in their own words, and how it is shared.
    fn page_sharing(&self, body: Column, model: &ProfileModel) -> Column {
        let body = body
            .push(Box::new(Divider::horizontal()))
            .push(Box::new(
                Text::new(&t("profile-section-status"))
                    .with_role(TextRole::Caption)
                    .dimmed(),
            ))
            .push(Box::new(self.status_field()));
        self.push_sharing(body, model)
    }

    /// What they are doing in their own words: Enter says it, an empty
    /// field clears it.
    fn status_field(&self) -> TextInput {
        let chosen = Rc::clone(&self.chosen);
        TextInput::from_state(&self.status_draft)
            .with_id("profile-status")
            .with_placeholder(t("profile-status-placeholder"))
            .with_accessible_name(t("profile-status"))
            .on_submit(move |typed| {
                *chosen.borrow_mut() = Some(ProfileChoice::Status(typed.trim().to_owned()));
            })
    }

    /// Status, colour and what people see: how the person shows to others.
    /// How what they are doing is shared, when the host keeps that choice.
    fn push_sharing(&self, body: Column, model: &ProfileModel) -> Column {
        let Some(current) = model.sharing else {
            return body;
        };
        let sharings = [
            ui::StatusSharing::Automatic,
            ui::StatusSharing::ManualOnly,
            ui::StatusSharing::Off,
        ];
        let chosen = Rc::clone(&self.chosen);
        let toggle = Box::new(
            ToggleGroup::new(
                sharings
                    .iter()
                    .map(|mode| Segment::new(t(sharing_key(*mode))))
                    .collect(),
                sharings
                    .iter()
                    .position(|mode| *mode == current)
                    .unwrap_or(0),
            )
            .with_id("profile-sharing")
            .with_accessible_name(t("a11y-shell-sharing-change"))
            .on_change(move |index| {
                if let Some(mode) = sharings.get(index) {
                    *chosen.borrow_mut() = Some(ProfileChoice::Sharing(*mode));
                }
            }),
        );
        body.push(Box::new(
            Text::new(&t("profile-sharing"))
                .with_role(TextRole::Caption)
                .dimmed(),
        ))
        .push(toggle)
    }

    fn presence_sections(&self, mut body: Column, model: &ProfileModel) -> Column {
        let heading =
            |key: &str| Box::new(Text::new(&t(key)).with_role(TextRole::Caption).dimmed());
        // Status: a preset, then what they are doing in their own words.
        let presets = [
            ui::Availability::Available,
            ui::Availability::Focusing,
            ui::Availability::Away,
        ];
        let chosen = Rc::clone(&self.chosen);
        let selected = presets
            .iter()
            .position(|preset| *preset == model.availability)
            .unwrap_or(0);
        let availability = ToggleGroup::new(
            presets
                .iter()
                .map(|preset| Segment::new(t(availability_key(*preset))))
                .collect(),
            selected,
        )
        .with_id("profile-availability")
        .with_accessible_name(t("profile-section-status"))
        .on_change(move |index| {
            if let Some(preset) = presets.get(index) {
                *chosen.borrow_mut() = Some(ProfileChoice::Availability(*preset));
            }
        });
        body = body
            .push(Box::new(Divider::horizontal()))
            .push(heading("profile-section-status"))
            .push(Box::new(availability))
            .push(Box::new(self.status_field()));
        body = self.push_sharing(body, model);

        // Colour: the twelve people colours.
        let chosen = Rc::clone(&self.chosen);
        let hues = ui::tokens::PresenceHue::ALL;
        // The colours as people travel in them: the dark material's presence
        // palette, the one identity colours are defined at.
        let palette = Theme::dark().tokens.colors.presence;
        let mut colours = ColorSwatchPicker::new(
            hues.iter()
                .map(|hue| ColorSwatch::named(palette.hue(*hue), hue.label())),
            // Two rows of six: the card's width, never past it.
            hues.len() / 2,
        )
        .on_change(move |index| {
            if let Some(hue) = hues.get(index) {
                *chosen.borrow_mut() = Some(ProfileChoice::Colour(hue.id().to_owned()));
            }
        });
        if let Some(index) = model
            .colour_id
            .as_deref()
            .and_then(ui::tokens::PresenceHue::from_id)
            .map(ui::tokens::PresenceHue::slot)
        {
            colours = colours.with_selected(index);
        }
        body = body
            .push(Box::new(Divider::horizontal()))
            .push(heading("profile-section-colour"))
            .push(Box::new(colours));

        self.push_visibility(body, model)
    }

    /// What the people in this workspace see of the person, when the
    /// host keeps that choice.
    fn push_visibility(&self, mut body: Column, model: &ProfileModel) -> Column {
        let Some(sight) = &model.sight else {
            return body;
        };
        let choices = [
            ui::PresenceVisibility::WhereIAm,
            ui::PresenceVisibility::ThatImHere,
            ui::PresenceVisibility::Nothing,
        ];
        let chosen = Rc::clone(&self.chosen);
        let selected = choices
            .iter()
            .position(|choice| *choice == sight.visibility)
            .unwrap_or(0);
        let what = ToggleGroup::new(
            choices
                .iter()
                .map(|choice| Segment::new(t(visibility_key(*choice))))
                .collect(),
            selected,
        )
        .with_id("profile-visibility")
        .with_accessible_name(t_args(
            "profile-section-visibility",
            &[("workspace", sight.workspace.clone().into())],
        ))
        .on_change(move |index| {
            if let Some(choice) = choices.get(index) {
                *chosen.borrow_mut() = Some(ProfileChoice::Visibility(*choice));
            }
        });
        body = body
            .push(Box::new(Divider::horizontal()))
            .push(Box::new(
                Text::new(&t_args(
                    "profile-section-visibility",
                    &[("workspace", sight.workspace.clone().into())],
                ))
                .with_role(TextRole::Caption)
                .dimmed(),
            ))
            .push(Box::new(what))
            .push(Box::new(
                Text::new(&t(visibility_detail_key(sight.visibility)))
                    .with_role(TextRole::Caption)
                    .dimmed(),
            ));
        body
    }

    /// One service's row: a button to link, the code to send with Copy, or
    /// the linked account with Unlink.
    fn linked_row(&self, account: &LinkedAccount) -> Box<dyn Widget> {
        let service = || ui::i18n::ArgValue::Text(account.service.clone());
        let chosen = Rc::clone(&self.chosen);
        let app = account.app.clone();
        let (what, button): (Box<dyn Widget>, Button) = match &account.state {
            LinkState::Unlinked => {
                return Box::new(
                    Button::new(
                        &format!("profile-link-{}", account.app),
                        &t_args("profile-link", &[("service", service())]),
                    )
                    .with_icon(Icon::Link)
                    .role(ButtonRole::Plain)
                    .on_click(move || {
                        *chosen.borrow_mut() = Some(ProfileChoice::Link(app.clone()));
                    }),
                );
            }
            LinkState::Linking(code) => {
                let copy = code.code.clone();
                let chip = Container::new()
                    .with_background()
                    .hug()
                    .with_inset(ui::tokens::Rhythm::Tight)
                    .with_child(Box::new(
                        Text::new(&code.code)
                            .with_id(format!("profile-link-code-{}", account.app))
                            .with_role(TextRole::Code),
                    ));
                let row = Row::new()
                    .no_wrap()
                    .with_align_items(taffy::AlignItems::Center)
                    .with_justify(taffy::JustifyContent::SpaceBetween)
                    .push(Box::new(chip))
                    .push(Box::new(
                        Button::new(&format!("profile-copy-{}", account.app), &t("profile-copy"))
                            .with_icon(Icon::Copy)
                            .on_click(move || ui::text_selection::clipboard_copy(&copy)),
                    ));
                return Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_rhythm(ui::tokens::Rhythm::Tight)
                        .push(Box::new(
                            Text::new(&code.instruction).with_live(ui::a11y::Politeness::Polite),
                        ))
                        .push(Box::new(row)),
                );
            }
            LinkState::Linked(name) => (
                Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_rhythm(ui::tokens::Rhythm::Tight)
                        .push(Box::new(Text::new(&account.service)))
                        .push(Box::new(
                            Text::new(&name.label())
                                .with_id(format!("profile-linked-{}", account.app))
                                .with_role(TextRole::Caption)
                                .dimmed(),
                        )),
                ),
                Button::new(
                    &format!("profile-unlink-{}", account.app),
                    &t("profile-unlink"),
                )
                .with_icon(Icon::Unlink)
                .on_click(move || {
                    *chosen.borrow_mut() = Some(ProfileChoice::Unlink(app.clone()));
                }),
            ),
        };
        Box::new(
            Row::new()
                .no_wrap()
                .with_align_items(taffy::AlignItems::Center)
                .with_justify(taffy::JustifyContent::SpaceBetween)
                .push(what)
                .push(Box::new(button)),
        )
    }

    /// Build the card's popover for `model` where it opened, open.
    fn present(&mut self, model: &ProfileModel) {
        let (anchor, viewport) = self.placed;
        self.popover = Popover::with_content(
            Container::new()
                .with_no_padding()
                .with_child(Box::new(self.body(model))),
        )
        .side(PopoverSide::Below)
        .aligned(ui::widget::PanelAlign::Trailing)
        .confined_to_viewport()
        .with_label(t("profile-card"));
        self.popover.set_viewport(viewport);
        self.popover.set_anchor(anchor);
        self.popover.set_open(true);
        self.replaced = true;
    }

    /// Open the card for `model` under `anchor` (the own avatar) within
    /// `viewport`, or close it when it is open.
    pub fn toggle(&mut self, model: &ProfileModel, anchor: Rect, viewport: Rect) {
        if self.is_open() {
            self.close();
            return;
        }
        self.shown.clone_from(&model.name);
        // Asked for a name: the field starts empty, showing what to type.
        self.draft.set(if model.ask_name && !model.can_rename {
            String::new()
        } else {
            model.name.clone()
        });
        self.status_draft.set(model.status.clone());
        self.chosen.borrow_mut().take();
        self.placed = (anchor, viewport);
        self.present(model);
        self.model = Some(model.clone());
        self.focus_reset = true;
    }

    /// What came of a linked account's choice (a link made its code, an
    /// account was linked or unlinked): the open card shows `linked` now.
    pub fn set_linked(&mut self, linked: Vec<LinkedAccount>) {
        let Some(model) = self.model.as_mut() else {
            return;
        };
        if model.linked == linked {
            return;
        }
        model.linked = linked;
        let model = model.clone();
        if self.is_open() {
            // The same card, its contents built again: the popover keeps
            // where it is and the pointer it is tracking.
            let body = Box::new(self.body(&model));
            self.popover.content_mut().set_child(body);
            self.replaced = true;
            // The control chosen is gone; focus goes back into the card.
            self.focus_reset = true;
        }
    }

    pub fn linked(&self) -> &[LinkedAccount] {
        self.model.as_ref().map_or(&[], |model| &model.linked)
    }

    /// Close the card. A name typed but not yet entered is kept, as a field
    /// that loses focus keeps what was typed.
    pub fn close(&mut self) {
        if self.is_open() {
            self.popover.close();
            self.commit_draft();
        }
    }

    fn commit_draft(&self) {
        let typed = self.draft.get();
        let typed = typed.trim();
        if !typed.is_empty() && typed != self.shown.trim() && self.chosen.borrow().is_none() {
            *self.chosen.borrow_mut() = Some(ProfileChoice::Rename(typed.to_owned()));
        }
    }

    pub fn is_open(&self) -> bool {
        self.popover.is_open()
    }

    /// The name in the field now.
    pub fn draft(&self) -> String {
        self.draft.get()
    }

    /// Type `name` into the name field, as the person would.
    pub fn set_draft(&mut self, name: &str) {
        self.draft.set(name.to_owned());
    }

    /// What the person did since the last call. A choice closes the card,
    /// but for a linked account's, whose outcome the card goes on to show.
    pub fn take_choice(&mut self) -> Option<ProfileChoice> {
        let choice = self.chosen.borrow_mut().take()?;
        if let ProfileChoice::Rename(name) = &choice {
            self.shown.clone_from(name);
        }
        if choice.closes() && self.popover.is_open() {
            self.popover.close();
        }
        Some(choice)
    }

    /// The bounds of the open card.
    pub fn rect(&self) -> Rect {
        self.popover.rect()
    }

    /// Lay the card out where it will be painted; geometry is usable after.
    pub fn place(&mut self, text: &mut TextRenderer, theme: &Theme) {
        self.popover.place(text, theme);
    }
}

impl Widget for ProfileCard {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let width = theme.tokens.metrics.panel_width_utility * theme.text_scale.max(1.0);
        let capacity = self.popover.available_content_width(theme);
        self.popover.content_mut().set_width(width.min(capacity));
        self.popover.build_layout(taffy, theme, map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.popover.apply_layout(taffy, node, origin);
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        self.popover.update(state, path)
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.popover.sync(state, path);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let open = self.is_open();
        self.popover.handle_event(event, ctx);
        if open && !self.is_open() {
            // Escape or a click outside: what was typed is kept.
            self.commit_draft();
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.popover]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.popover]
    }

    fn take_focus_reset(&mut self) -> bool {
        std::mem::take(&mut self.focus_reset) | self.popover.take_focus_reset()
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.replaced) | self.popover.take_structure_dirty()
    }

    fn traps_focus(&self) -> bool {
        self.is_open()
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.popover.modal_capture()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.popover.wants_pointer_capture()
    }

    fn needs_redraw(&self) -> bool {
        self.popover.needs_redraw()
    }

    fn name(&self) -> &'static str {
        "ProfileCard"
    }

    fn get_rect(&self) -> Rect {
        self.popover.get_rect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> ProfileModel {
        ProfileModel {
            name: "Kim".into(),
            identity: "kim-root".into(),
            colour: None,
            can_rename: true,
            ask_name: false,
            actions: vec![
                ProfileAction {
                    id: "keep-identity".into(),
                    title: "Keep your name and work".into(),
                    group: ProfileGroup::Identity,
                    icon: Icon::Fingerprint,
                },
                ProfileAction {
                    id: "pair-device".into(),
                    title: "Pair a new device".into(),
                    group: ProfileGroup::Devices,
                    icon: Icon::MonitorSmartphone,
                },
            ],
            linked: Vec::new(),
            ..ProfileModel::default()
        }
    }

    fn discord(state: LinkState) -> LinkedAccount {
        LinkedAccount {
            app: "so.tilo.discord".into(),
            service: "Discord".into(),
            state,
        }
    }

    fn code() -> tilo_app::linked_accounts::LinkCode {
        tilo_app::linked_accounts::LinkCode {
            code: "ABCDEFGHJKLM".into(),
            instruction: "In Discord, type /link with this code. It works for 10 minutes.".into(),
        }
    }

    /// The card open with `linked`, in a frame that clicks.
    fn framed(linked: Vec<LinkedAccount>) -> ui::headless::Framed<ProfileCard> {
        ui::i18n::I18nManager::init("en-US");
        let mut card = ProfileCard::new();
        card.toggle(
            &ProfileModel { linked, ..model() },
            Rect::new(1200.0, 2.0, 1220.0, 22.0),
            Rect::new(0.0, 0.0, 1280.0, 720.0),
        );
        ui::headless::Framed::new(ui::State::new(), card, (1280, 720), Theme::dark())
    }

    fn click(frame: &mut ui::headless::Framed<ProfileCard>, name: &str) {
        let harness = ui::headless::Framed::harness_mut(frame);
        harness.poll_frame().unwrap();
        harness.click_named(name).unwrap();
        harness.poll_frame().unwrap();
    }

    /// Everything the open card says, as assistive technology reads it.
    fn says(frame: &mut ui::headless::Framed<ProfileCard>) -> String {
        let harness = ui::headless::Framed::harness_mut(frame);
        harness.poll_frame().unwrap();
        harness.inspection_json().unwrap()
    }

    /// Opening the card is not editing: focus rests on the card itself, and
    /// Tab (or a click) enters the name field.
    #[test]
    fn the_card_opens_focused_on_itself_and_tab_enters_the_name() {
        // The card's keyboard stops in tree order, as both shells' focus
        // order walks them; a trap that opens focuses the first.
        fn stops(widget: &dyn Widget, out: &mut Vec<ui::a11y::AccessNode>) {
            if widget.is_focusable()
                && let Some(node) = widget.accessibility()
            {
                out.push(node);
            }
            for child in widget.children() {
                stops(child, out);
            }
        }
        ui::i18n::I18nManager::init("en-US");
        let mut card = ProfileCard::new();
        card.toggle(
            &model(),
            Rect::new(1200.0, 2.0, 1220.0, 22.0),
            Rect::new(0.0, 0.0, 1280.0, 720.0),
        );
        let mut order = Vec::new();
        stops(&card, &mut order);
        let names: Vec<_> = order.iter().map(|node| node.name.clone()).collect();
        let first = order.first().expect("the open card has keyboard stops");
        assert_eq!(
            first.semantic_key.as_deref(),
            Some("profile-card-focus"),
            "the card opens focused on itself, not its name field: {names:?}"
        );
        assert_eq!(first.name, Some(t("profile-card")));
        assert_eq!(
            order.get(1).and_then(|node| node.name.clone()),
            Some(t("profile-name")),
            "Tab enters the name field: {names:?}"
        );
    }

    /// A person who has not named themselves is asked, gently, in the card:
    /// "Add your name", in place of the hint, until they have.
    #[test]
    fn a_person_without_a_name_is_asked_for_one() {
        let open = |ask_name: bool| {
            ui::i18n::I18nManager::init("en-US");
            let mut card = ProfileCard::new();
            card.toggle(
                &ProfileModel {
                    name: "bob".into(),
                    can_rename: false,
                    ask_name,
                    ..model()
                },
                Rect::new(1200.0, 2.0, 1220.0, 22.0),
                Rect::new(0.0, 0.0, 1280.0, 720.0),
            );
            ui::headless::Framed::new(ui::State::new(), card, (1280, 720), Theme::dark())
        };
        let mut asked = open(true);
        let said = says(&mut asked);
        assert!(said.contains("Add your name"), "{said}");
        let mut named = open(false);
        let said = says(&mut named);
        assert!(!said.contains("Add your name"), "{said}");
        assert!(said.contains(&t("profile-name-hint")));
    }

    /// A spectator's card says nobody here sees them, and asks nothing
    /// about what they share.
    #[test]
    fn a_spectators_card_says_nobody_sees_them() {
        ui::i18n::I18nManager::init("en-US");
        let mut card = ProfileCard::new();
        card.toggle(
            &ProfileModel {
                name: "Spectator".into(),
                can_rename: false,
                seen: Seen::Watching,
                sharing: None,
                ..model()
            },
            Rect::new(1200.0, 2.0, 1220.0, 22.0),
            Rect::new(0.0, 0.0, 1280.0, 720.0),
        );
        let mut card =
            ui::headless::Framed::new(ui::State::new(), card, (1280, 720), Theme::dark());
        let said = says(&mut card);
        assert!(said.contains(&t("profile-watching-hint")), "{said}");
        assert!(!said.contains(&t("profile-name-hint")), "{said}");
        assert!(!said.contains(&t("profile-sharing")), "{said}");
    }

    /// Of the host's ways to keep who you are, the card offers one: the
    /// first, which the host lists best first. Devices stay as listed.
    #[test]
    fn the_card_offers_one_way_to_keep_your_identity() {
        ui::i18n::I18nManager::init("en-US");
        let identity = |id: &str, title: &str| ProfileAction {
            id: id.into(),
            title: title.into(),
            group: ProfileGroup::Identity,
            icon: Icon::Fingerprint,
        };
        let mut actions = vec![
            identity("keep-identity", "Keep your name and work"),
            identity("save-recovery-key", "Save a recovery key"),
            identity("sign-in-passkey", "Sign in with a passkey"),
        ];
        actions.extend(
            model()
                .actions
                .into_iter()
                .filter(|a| a.group == ProfileGroup::Devices),
        );
        let mut card = ProfileCard::new();
        card.toggle(
            &ProfileModel { actions, ..model() },
            Rect::new(1200.0, 2.0, 1220.0, 22.0),
            Rect::new(0.0, 0.0, 1280.0, 720.0),
        );
        let mut frame =
            ui::headless::Framed::new(ui::State::new(), card, (1280, 720), Theme::dark());
        let said = says(&mut frame);
        assert!(said.contains("Keep your name and work"), "{said}");
        assert!(!said.contains("Save a recovery key"), "{said}");
        assert!(!said.contains("Sign in with a passkey"), "{said}");
        assert!(said.contains("Pair a new device"), "{said}");
    }

    #[test]
    fn no_linked_accounts_section_without_a_connected_service() {
        let mut frame = framed(Vec::new());
        assert!(!says(&mut frame).contains("Linked accounts"));
        let mut frame = framed(vec![discord(LinkState::Unlinked)]);
        assert!(says(&mut frame).contains("Linked accounts"));
    }

    #[test]
    fn linking_keeps_the_card_open_and_shows_the_code_to_copy() {
        let mut frame = framed(vec![discord(LinkState::Unlinked)]);
        click(&mut frame, "profile-link-so.tilo.discord");
        assert_eq!(
            frame.take_choice(),
            Some(ProfileChoice::Link("so.tilo.discord".into()))
        );
        assert!(frame.is_open(), "the card waits for the code");
        frame.set_linked(vec![discord(LinkState::Linking(code()))]);
        let said = says(&mut frame);
        assert!(
            said.contains("In Discord, type /link with this code") && said.contains("ABCDEFGHJKLM"),
            "{said}"
        );
        assert!(said.contains("profile-copy-so.tilo.discord"), "{said}");
        assert!(frame.is_open());
        assert_eq!(frame.draft(), "Kim", "the name field keeps what it had");
    }

    #[test]
    fn a_linked_account_offers_unlink() {
        let mut frame = framed(vec![discord(LinkState::Linked(
            tilo_app::linked_accounts::AccountName {
                user_name: "ada.l".into(),
                display_name: Some("Ada L".into()),
            },
        ))]);
        assert!(says(&mut frame).contains("Ada L · ada.l"), "shown by name");
        click(&mut frame, "profile-unlink-so.tilo.discord");
        assert_eq!(
            frame.take_choice(),
            Some(ProfileChoice::Unlink("so.tilo.discord".into()))
        );
        assert!(frame.is_open());
        frame.set_linked(vec![discord(LinkState::Unlinked)]);
        assert_eq!(frame.linked(), [discord(LinkState::Unlinked)]);
    }

    /// Hosts the card as a shell does: the linked accounts change during a
    /// frame's update, not through the harness's own access to its root
    /// (which lays out from scratch).
    struct Shell {
        card: ProfileCard,
        next: Rc<RefCell<Option<Vec<LinkedAccount>>>>,
    }

    impl Widget for Shell {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.card.build_layout(taffy, theme, map)
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            self.card.apply_layout(taffy, node, origin);
        }

        fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
            let linked = self.next.borrow_mut().take();
            let changed = linked.is_some();
            if let Some(linked) = linked {
                self.card.set_linked(linked);
            }
            self.card.update(state, path) | changed
        }

        fn children(&self) -> Vec<&dyn Widget> {
            vec![&self.card]
        }

        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            vec![&mut self.card]
        }

        fn needs_redraw(&self) -> bool {
            self.next.borrow().is_some() || self.card.needs_redraw()
        }

        fn name(&self) -> &'static str {
            "Shell"
        }

        fn get_rect(&self) -> Rect {
            self.card.get_rect()
        }
    }

    /// The card builds its widgets again when a linked account changes.
    /// A layout kept from before would still point at the ones they
    /// replaced, freed: the page read them as garbage once their memory was
    /// reused (after an app's code loaded), and trapped in an indirect call.
    #[test]
    fn a_rebuilt_card_is_laid_out_again_before_a_frame_reaches_it() {
        ui::i18n::I18nManager::init("en-US");
        let mut card = ProfileCard::new();
        card.toggle(
            &ProfileModel {
                linked: vec![discord(LinkState::Unlinked)],
                ..model()
            },
            Rect::new(1200.0, 2.0, 1220.0, 22.0),
            Rect::new(0.0, 0.0, 1280.0, 720.0),
        );
        let next = Rc::new(RefCell::new(None));
        let shell = Shell {
            card,
            next: Rc::clone(&next),
        };
        let mut frame =
            ui::headless::Framed::new(ui::State::new(), shell, (1280, 720), Theme::dark());
        let harness = ui::headless::Framed::harness_mut(&mut frame);
        harness.poll_frame().unwrap();
        let before = harness.rebuilt_frames();
        *next.borrow_mut() = Some(vec![discord(LinkState::Linking(code()))]);
        assert!(harness.poll_frame().unwrap(), "the change is drawn");
        assert!(
            harness.rebuilt_frames() > before,
            "the frame lays the rebuilt card out again instead of keeping the old layout"
        );
        assert!(harness.inspection_json().unwrap().contains("ABCDEFGHJKLM"));
    }

    /// Where the host keeps only how what they do is shared (a page), the
    /// card still has the field for what they are doing in their own
    /// words, holding what they said; a spectator's has none.
    #[test]
    fn a_page_card_offers_what_they_are_doing_in_their_own_words() {
        ui::i18n::I18nManager::init("en-US");
        let open = |sharing| {
            let mut card = ProfileCard::new();
            card.toggle(
                &ProfileModel {
                    sharing,
                    offers_presence: false,
                    status: "Reviewing the launch plan".into(),
                    ..model()
                },
                Rect::new(1200.0, 2.0, 1220.0, 22.0),
                Rect::new(0.0, 0.0, 1280.0, 720.0),
            );
            ui::headless::Framed::new(ui::State::new(), card, (1280, 720), Theme::dark())
        };
        let said = says(&mut open(Some(ui::StatusSharing::Automatic)));
        assert!(said.contains("profile-status"), "{said}");
        assert!(said.contains("Reviewing the launch plan"), "{said}");
        let said = says(&mut open(None));
        assert!(!said.contains("profile-status"), "{said}");
    }

    fn opened() -> ProfileCard {
        ui::i18n::I18nManager::init("en-US");
        let mut card = ProfileCard::new();
        card.toggle(
            &model(),
            Rect::new(1200.0, 2.0, 1220.0, 22.0),
            Rect::new(0.0, 0.0, 1280.0, 720.0),
        );
        card
    }

    #[test]
    fn the_card_opens_with_the_name_in_its_field() {
        let card = opened();
        assert!(card.is_open());
        assert_eq!(card.draft(), "Kim");
    }

    #[test]
    fn a_typed_name_is_kept_when_the_card_closes() {
        let mut card = opened();
        card.set_draft("  Kim Lee ");
        card.close();
        assert_eq!(
            card.take_choice(),
            Some(ProfileChoice::Rename("Kim Lee".into()))
        );
        assert_eq!(card.take_choice(), None);
    }

    #[test]
    fn an_unchanged_or_blank_name_changes_nothing() {
        let mut card = opened();
        card.close();
        assert_eq!(card.take_choice(), None);
        let mut card = opened();
        card.set_draft("   ");
        card.close();
        assert_eq!(card.take_choice(), None);
    }

    #[test]
    fn toggling_again_closes() {
        let mut card = opened();
        card.toggle(&model(), Rect::ZERO, Rect::new(0.0, 0.0, 800.0, 600.0));
        assert!(!card.is_open());
    }
}

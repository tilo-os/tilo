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
//! Where the workspace is connected to a service elsewhere (a Discord server it
//! bridges), the card lists the person's account there under "Linked
//! accounts" ([`ProfileModel::linked`]): connect it (signing in on the
//! service's consent page), confirm the account a link from there offers
//! (Discord's /link), or unlink it. The host runs
//! those on the connector's workspace record and tells the card what came of it
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
    Divider, Fact, Facts, ListRow, Popover, PopoverSide, Row, Segment, Text, TextInput, Toggle,
    ToggleGroup, Widget,
};
use ui::{LocalState, TextRole};
use vello::kurbo::{Point, Rect};
use vello::peniko::Color;
use winit::event::WindowEvent;

use crate::i18n::{t, t_args};
use tilo_app::linked_accounts::service_icon;
pub use tilo_app::linked_accounts::{LinkState, LinkedAccount};
use ui::widget::DrawChild;

/// The words for a status preset. The words for one way of sharing what you are doing.
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
    /// The stable identity the avatar's colour and initials derive from when the person chose no
    /// colour.
    pub identity: String,
    pub colour: Option<Color>,
    pub can_rename: bool,
    /// The person has not named themselves yet (`name` is their login account's): the card asks,
    /// once, for the name people here should see, until they give one.
    pub ask_name: bool,
    pub actions: Vec<ProfileAction>,
    /// The services the workspace is connected to, with the person's account at each; none hides
    /// the section.
    pub linked: Vec<LinkedAccount>,
    /// How available the person says they are.
    pub availability: ui::Availability,
    /// The short status they wrote, if any.
    pub status: String,
    /// Whether what they are doing is shared on its own, only in their words, or not at all; `None`
    /// where the host keeps no such choice.
    pub sharing: Option<ui::StatusSharing>,
    /// The colour they chose, by its id; `None` while their identity's own colour is worn.
    pub colour_id: Option<String>,
    /// What the people in the workspace on screen see of them; `None` outside any shared workspace.
    pub sight: Option<WorkspaceSight>,
    /// The host keeps the person's status, colour and what people see: the card offers them.
    pub offers_presence: bool,
    /// Whether the desktop's control hints show; `None` where the host has none to offer.
    pub control_hints: Option<bool>,
    /// The person's root key (hex): the id that stays theirs on every device and never changes,
    /// shown short with Copy.
    pub key: Option<String>,
    /// The devices the person is here on, each as it calls itself and as it said it was: which of
    /// their machines are here.
    pub devices: Vec<crate::here::PersonDevice>,
    /// The card is about someone else: what they say they are doing and what this person can do
    /// with them. `None` for the person's own card.
    pub other: Option<OtherPerson>,
}

/// Someone else, as their card shows them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OtherPerson {
    /// Their roster id (the device their presence comes from).
    pub id: String,
    /// What they are doing: their own words, or a line software wrote,
    /// which the card marks as automatic.
    pub status: Option<crate::collaborators::RosterStatus>,
    /// Where they are ("In Chat"), when it adds to the status.
    pub place: Option<String>,
    /// How they said they are here; `None` for an agent, which says nothing of it.
    pub presence: Option<ui::Availability>,
    /// This person follows their view now.
    pub following: bool,
    /// The host can follow them (bring forward what they are in).
    pub can_follow: bool,
    /// The host can open the conversation this person has with them: their root key is known and
    /// the app that holds it is here.
    pub can_message: bool,
    /// Their accounts elsewhere, as their profile names them.
    pub linked: Vec<tilo_app::linked_accounts::ProfileLink>,
    /// They are an agent: the card offers what can be done with it, each through its card in
    /// Console. `None` for a person.
    pub agent: Option<AgentFacts>,
}

/// An agent as its face's card shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentFacts {
    /// The client it is, by the name it goes by ("Claude Code"): what its card in Console is
    /// addressed by.
    pub client: String,
}

/// What a person asks of an agent from its face's card. Each is the agent's own action in Console
/// (`console.agent:<client>`), which also brings its card forward: nothing done to an agent
/// happens out of sight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentAsk {
    /// Pull its card out into a window beside the work.
    Card,
    Stop,
    UndoRun,
    TakeOver,
    Revoke,
}

impl AgentAsk {
    /// The action of the agent's resource in Console it is.
    #[must_use]
    pub const fn action(self) -> &'static str {
        match self {
            Self::Card => "view-agent",
            Self::Stop => "stop-agent",
            Self::UndoRun => "undo-agent-run",
            Self::TakeOver => "take-over-agent",
            Self::Revoke => "revoke-agent",
        }
    }
}

/// The most characters a linked account's line holds beside Unlink: the card is one utility panel
/// wide, so longer is cut short, never wrapped.
const LINKED_LINE_CHARS: usize = 22;

/// A linked account on one line: its handle, and the name it shows when that is another, each cut
/// short with an ellipsis to fit [`LINKED_LINE_CHARS`]; the name is left out before the handle is
/// cut.
fn linked_line(name: &tilo_app::linked_accounts::AccountName) -> (String, Option<String>) {
    let cut = |text: &str, room: usize| -> String {
        if text.chars().count() <= room {
            return text.to_owned();
        }
        let kept: String = text.chars().take(room.saturating_sub(1)).collect();
        format!("{}…", kept.trim_end())
    };
    let handle = cut(&name.handle(), LINKED_LINE_CHARS);
    let room = LINKED_LINE_CHARS.saturating_sub(handle.chars().count().saturating_add(1));
    let shown = name
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|shown| !shown.is_empty() && *shown != name.user_name)
        .filter(|_| room >= 4)
        .map(|shown| cut(shown, room));
    (handle, shown)
}

/// The host actions about the person's picture, which the card offers beside their avatar rather
/// than in its list of identity actions.
pub const SET_PHOTO: &str = "set-photo";
pub const RESET_PHOTO: &str = "reset-photo";

/// A root key shown short: its first eight and last four characters.
#[must_use]
pub fn short_key(key: &str) -> String {
    let key = key.trim();
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 14 {
        return key.to_owned();
    }
    let head: String = chars.iter().take(8).collect();
    let tail: String = chars.iter().skip(chars.len().saturating_sub(4)).collect();
    format!("{head}…{tail}")
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
    /// Link the person's account with this connector's service (its app id). The card stays open
    /// for the code.
    Link(String),
    /// Unlink the person's account with this connector's service.
    Unlink(String),
    /// Link the account this connector's service offers.
    Accept(String),
    /// Turn that offer down.
    Decline(String),
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
    /// Follow this person (roster id), or stop following them.
    Follow(String),
    /// Open the conversation this person has with them in Chat (root key,
    /// hex), on Chat's row for it, its composer ready.
    Message(String),
    /// Ask this of the agent of this client (`AgentFacts::client`).
    Agent {
        client: String,
        ask: AgentAsk,
    },
}

impl ProfileChoice {
    /// Whether the card closes on it: a linked account's choice keeps it open to show what came of
    /// it.
    fn closes(&self) -> bool {
        matches!(
            self,
            Self::Rename(_)
                | Self::Action(_)
                | Self::Follow(_)
                | Self::Message(_)
                | Self::Agent { .. }
        )
        // The rest are settings the card goes on showing.
    }
}

/// The card behind the bar's own avatar. The card itself as a place keyboard focus can rest:
/// opening the card is not editing, so focus lands here, on the whole card, rather than in its name
/// field. A click on the field, or Tab, enters it. It lies under the card's content, filling it,
/// draws nothing and takes no input; assistive technology reads it as the card.
struct CardFocus {
    rect: Rect,
    focused: bool,
    /// Its paint changed since the last frame drew it.
    dirty: bool,
    /// What the card is called: theirs, or yours.
    label: String,
}

impl CardFocus {
    fn new(label: String) -> Self {
        Self {
            rect: Rect::ZERO,
            focused: false,
            dirty: true,
            label,
        }
    }
}

impl Widget for CardFocus {
    fn needs_redraw(&self) -> bool {
        ui::widget::redraw_or_children(self.dirty, self)
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        ui::widget::Widget::mark_children_drawn(self);
    }

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
        if self.focused != focused {
            self.dirty = true;
        }
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Group)
                .with_name(self.label.clone())
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
    /// What the open card shows, to build it again when a linked account changes, and the anchor
    /// and viewport it opened at.
    model: Option<ProfileModel>,
    placed: (Rect, Rect),
    /// The card's widgets were built again (it opened, or a linked account changed while open): a
    /// layout that still maps the ones they replaced must be built again before it reaches them.
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
            .on_submit_view(move |typed| {
                let typed = typed.trim();
                if !typed.is_empty() {
                    *chosen.borrow_mut() = Some(ProfileChoice::Rename(typed.to_owned()));
                }
            })
    }

    /// The person's stable id, short, with a button that copies it whole. Their own card keeps it
    /// quiet, under their identity: it says who they are, it is not what they came to change.
    fn key_row(key: &str, quiet: bool) -> Box<dyn Widget> {
        let copy = key.to_owned();
        let shown = Text::new(&short_key(key))
            .with_id("profile-key")
            .with_role(TextRole::Code);
        let shown = if quiet { shown.dimmed() } else { shown };
        Box::new(
            Row::new()
                .no_wrap()
                .with_align_items(taffy::AlignItems::Center)
                .with_justify(taffy::JustifyContent::SpaceBetween)
                .push(Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_rhythm(ui::tokens::Rhythm::Tight)
                        .push(Box::new(
                            Text::new(&t("profile-id"))
                                .with_role(TextRole::Caption)
                                .dimmed(),
                        ))
                        .push(Box::new(shown)),
                ))
                .push(Box::new(
                    Button::icon_only("profile-copy-key", Icon::Copy, &t("profile-copy-id"))
                        .role(ButtonRole::Plain)
                        .on_own_device_effect(move || {
                            ui::text_selection::clipboard_copy(&copy);
                        }),
                )),
        )
    }

    /// What someone says they are doing: their own words plainly, a line software wrote marked as
    /// automatic.
    fn status_row(status: &crate::collaborators::RosterStatus) -> Box<dyn Widget> {
        let written = status.source == crate::collaborators::LineSource::Written;
        let row = ListRow::new("profile-other-status", status.text.clone()).two_line_title();
        Box::new(if written {
            row.secondary(t("profile-status-own-words"))
        } else {
            row.mark(Icon::Sparkles)
                .secondary(t("profile-status-automatic"))
        })
    }

    /// Someone else's card: who they are, what they are doing, and what this person can do with
    /// them.
    fn other_body(&self, model: &ProfileModel, other: &OtherPerson) -> Column {
        let header = Self::other_header(model, other);
        let mut body = Column::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .with_padding_rhythm(ui::tokens::Rhythm::Related, ui::tokens::Rhythm::Related)
            .with_intrinsic_height()
            .push(Box::new(CardFocus::new(t_args(
                "profile-card-of",
                &[("name", model.name.clone().into())],
            ))))
            .push(Box::new(header));
        if let Some(status) = &other.status {
            body = body.push(if other.agent.is_some() {
                // What an agent is at is its live line, not words it chose: it reads as now.
                Box::new(
                    ListRow::new("profile-other-status", status.text.clone())
                        .two_line_title()
                        .secondary(t("profile-agent-now")),
                ) as Box<dyn Widget>
            } else {
                Self::status_row(status)
            });
        }
        for link in &other.linked {
            body = body.push(Self::linked_to(link));
        }
        if !model.devices.is_empty() {
            body = body
                .push(Box::new(Divider::horizontal()))
                .push(Box::new(Self::devices_section(&model.devices)));
        }
        if let Some(key) = &model.key {
            body = body
                .push(Box::new(Divider::horizontal()))
                .push(Self::key_row(key, false));
        }
        if let Some(actions) = self.other_actions(model, other) {
            body = body
                .push(Box::new(Divider::horizontal()))
                .push(Box::new(actions));
        }
        if let Some(agent) = &other.agent {
            body = body.push(Box::new(self.agent_commands(agent)));
        }
        body
    }

    /// Someone else's face, name and how and where they are here, side by side.
    fn other_header(model: &ProfileModel, other: &OtherPerson) -> Row {
        let mut avatar = Avatar::person(&model.name, &model.identity).size(AvatarSize::Large);
        if let Some(colour) = model.colour {
            avatar = avatar.with_presence(colour);
        }
        // An agent's face is square with the agent mark (its colour is its client's hue, as the
        // host gives it): never a person's.
        if other.agent.is_some() {
            avatar = avatar.agent();
        }
        let mut who = Column::new()
            .with_no_padding()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .push(Box::new(
                Text::new(&model.name)
                    .with_id("profile-other-name")
                    .with_role(TextRole::Subheading)
                    .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph),
            ));
        // How they are here, under their name: their presence, before anything about where they
        // are.
        if let Some(presence) = other.presence {
            who = who.push(Box::new(
                Text::new(&t(availability_key(presence)))
                    .with_id("profile-other-availability")
                    .with_role(TextRole::Caption)
                    .dimmed(),
            ));
        }
        if let Some(place) = &other.place {
            who = who.push(Box::new(
                Text::new(place).with_role(TextRole::Caption).dimmed(),
            ));
        }
        Row::new()
            .no_wrap()
            .with_align_items(taffy::AlignItems::Center)
            .push(Box::new(avatar))
            .push(Box::new(who))
    }

    /// What this person can do with someone else: message, follow, or open an agent's card;
    /// `None` when there is nothing.
    fn other_actions(&self, model: &ProfileModel, other: &OtherPerson) -> Option<Row> {
        let mut actions = Row::new()
            .no_wrap()
            .with_align_items(taffy::AlignItems::Center);
        let mut any = false;
        if other.can_message
            && let Some(key) = model.key.clone()
        {
            let chosen = Rc::clone(&self.chosen);
            actions = actions.push(Box::new(
                Button::new("profile-message", &t("profile-message"))
                    .with_icon(Icon::MessageCircle)
                    .role(ButtonRole::Primary)
                    .on_own_device_effect(move || {
                        *chosen.borrow_mut() = Some(ProfileChoice::Message(key.clone()));
                    }),
            ));
            any = true;
        }
        if other.can_follow {
            let chosen = Rc::clone(&self.chosen);
            let id = other.id.clone();
            let label = if other.following {
                t("profile-stop-following")
            } else {
                t("profile-follow")
            };
            actions = actions.push(Box::new(
                Button::new("profile-follow", &label)
                    .with_icon(Icon::Eye)
                    .on_effect(move || {
                        *chosen.borrow_mut() = Some(ProfileChoice::Follow(id.clone()));
                    }),
            ));
            any = true;
        }
        if let Some(agent) = &other.agent {
            let chosen = Rc::clone(&self.chosen);
            let client = agent.client.clone();
            actions = actions.push(Box::new(
                Button::new("profile-agent-card", &t("profile-agent-card"))
                    .with_icon(Icon::PanelRight)
                    .role(ButtonRole::Primary)
                    .on_effect(move || {
                        *chosen.borrow_mut() = Some(ProfileChoice::Agent {
                            client: client.clone(),
                            ask: AgentAsk::Card,
                        });
                    }),
            ));
            any = true;
        }
        any.then_some(actions)
    }

    /// What can be done with an agent from its face: Stop, Undo run, Take over and Revoke, each
    /// carried out by its card in Console, which comes forward to show what came of it.
    fn agent_commands(&self, agent: &AgentFacts) -> Row {
        // Labelled, never bare icons: Stop and Revoke must not be guessed at. The row wraps on the
        // narrow card.
        let mut row = Row::new().with_align_items(taffy::AlignItems::Center);
        for (id, label, icon, ask, role) in [
            (
                "profile-agent-stop",
                "profile-agent-stop",
                Icon::Square,
                AgentAsk::Stop,
                ButtonRole::Plain,
            ),
            (
                "profile-agent-undo-run",
                "profile-agent-undo-run",
                Icon::Undo2,
                AgentAsk::UndoRun,
                ButtonRole::Plain,
            ),
            (
                "profile-agent-take-over",
                "profile-agent-take-over",
                Icon::Hand,
                AgentAsk::TakeOver,
                ButtonRole::Plain,
            ),
            (
                "profile-agent-revoke",
                "profile-agent-revoke",
                Icon::Ban,
                AgentAsk::Revoke,
                ButtonRole::Destructive,
            ),
        ] {
            let chosen = Rc::clone(&self.chosen);
            let client = agent.client.clone();
            row = row.push(Box::new(
                Button::new(id, &t(label))
                    .with_icon(icon)
                    .role(role)
                    .on_effect(move || {
                        *chosen.borrow_mut() = Some(ProfileChoice::Agent {
                            client: client.clone(),
                            ask,
                        });
                    }),
            ));
        }
        row
    }

    /// The devices someone is here on, each named as it calls itself and as it said it was: the
    /// secondary row under who they are. The two are separate facts, laid out with space between
    /// them.
    fn devices_section(devices: &[crate::here::PersonDevice]) -> Column {
        let mut column = Column::new()
            .with_no_padding()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .with_intrinsic_height()
            .push(Box::new(
                Text::new(&t("profile-section-devices"))
                    .with_role(TextRole::Caption)
                    .dimmed(),
            ));
        for row in Self::device_rows(devices) {
            column = column.push(row);
        }
        column
    }

    /// One row per device: what it calls itself (or, on a device that named itself nothing, which
    /// one it is) and how available its person is there. Its key is kept for a copy affordance,
    /// never shown as its name.
    fn device_rows(devices: &[crate::here::PersonDevice]) -> Vec<Box<dyn Widget>> {
        devices
            .iter()
            .enumerate()
            .map(|(index, device)| {
                let named = device.label.trim();
                let name = if named.is_empty() {
                    t(if device.this {
                        "profile-device-this"
                    } else {
                        "profile-device-another"
                    })
                } else {
                    named.to_owned()
                };
                let mut facts = vec![Fact::text(name)];
                if device.this {
                    facts.push(Fact::text(t("profile-device-mark")));
                }
                facts.push(Fact::text(t(availability_key(device.availability))));
                let key = device.id.clone();
                Box::new(
                    Row::new()
                        .no_wrap()
                        .with_align_items(taffy::AlignItems::Center)
                        .with_justify(taffy::JustifyContent::SpaceBetween)
                        .push(Box::new(Facts::new(facts)))
                        .push(Box::new(
                            Button::icon_only(
                                &format!("profile-device-key-{index}"),
                                Icon::Copy,
                                &t("profile-copy-device-key"),
                            )
                            .role(ButtonRole::Plain)
                            .on_own_device_effect(move || {
                                ui::text_selection::clipboard_copy(&key);
                            }),
                        )),
                ) as Box<dyn Widget>
            })
            .collect()
    }

    /// Change the picture, and take it away again, where the host offers it: beside the avatar, not
    /// among the identity actions.
    fn photo_buttons(&self, model: &ProfileModel) -> Option<Box<dyn Widget>> {
        let offered = |id: &str| model.actions.iter().find(|action| action.id == id);
        let set = offered(SET_PHOTO)?;
        let mut row = Row::new()
            .no_wrap()
            .with_align_items(taffy::AlignItems::Center);
        for (action, label, icon) in [
            (Some(set), t("profile-photo-change"), Icon::Camera),
            (offered(RESET_PHOTO), t("profile-photo-remove"), Icon::X),
        ] {
            let Some(action) = action else { continue };
            let chosen = Rc::clone(&self.chosen);
            let id = action.id.clone();
            row = row.push(Box::new(
                Button::new(&format!("profile-action-{id}"), &label)
                    .with_icon(icon)
                    .role(ButtonRole::Plain)
                    .on_effect(move || {
                        *chosen.borrow_mut() = Some(ProfileChoice::Action(id.clone()));
                    }),
            ));
        }
        Some(Box::new(row))
    }

    /// Your avatar, your name (or a field for it) and the hint beneath.
    fn own_header(&self, model: &ProfileModel) -> Row {
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
            // A name with no spaces (a key, before one is chosen) still wraps inside the card.
            Box::new(
                Text::new(&model.name)
                    .with_role(TextRole::Subheading)
                    .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph),
            )
        };
        // Not named yet: in place of the hint, a field asking for the name people here should see.
        // Gone once they give one.
        let below: Box<dyn Widget> = if model.ask_name && !model.can_rename {
            Box::new(self.name_field(
                "profile-add-name",
                t("profile-add-your-name"),
                t("profile-add-your-name"),
            ))
        } else {
            Box::new(
                Text::new(&t("profile-name-hint"))
                    .with_role(TextRole::Caption)
                    .dimmed(),
            )
        };
        let who = Column::new()
            .with_no_padding()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .push(name)
            .push(below);
        Row::new()
            .no_wrap()
            .with_align_items(taffy::AlignItems::Center)
            .push(Box::new(avatar))
            .push(Box::new(who))
    }

    fn body(&self, model: &ProfileModel) -> Column {
        if let Some(other) = &model.other {
            return self.other_body(model, other);
        }
        let header = self.own_header(model);
        // As tall as what the card says: the card is placed by its own content, so a column filling
        // its parent would measure the card short and draw its last rows outside it.
        let mut body = Column::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .with_padding_rhythm(ui::tokens::Rhythm::Related, ui::tokens::Rhythm::Related)
            .with_intrinsic_height()
            // First in the card's focus order: the card opens focused on itself, not in its name
            // field.
            .push(Box::new(CardFocus::new(t("profile-card"))))
            .push(Box::new(header));
        if let Some(photo) = self.photo_buttons(model) {
            body = body.push(photo);
        }
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
                // The picture's are offered beside the avatar.
                .filter(|action| action.id != SET_PHOTO && action.id != RESET_PHOTO)
                // Keeping who you are and backing it up: the two clear asks, so a third overlapping
                // one never competes here; the rest stay in search.
                .take(if group == ProfileGroup::Identity {
                    2
                } else {
                    usize::MAX
                })
                .collect();
            // The devices this person is here on are listed once, under the heading their pairing
            // action shares.
            let devices = (group == ProfileGroup::Devices && !model.devices.is_empty())
                .then_some(model.devices.as_slice());
            // Their own id, kept quiet under their identity: it says who they are, not what they
            // came here to change.
            let key = (group == ProfileGroup::Identity)
                .then_some(model.key.as_deref())
                .flatten();
            if actions.is_empty() && devices.is_none() && key.is_none() {
                continue;
            }
            body = body.push(Box::new(Divider::horizontal()));
            if let Some(heading) = heading {
                body = body.push(Box::new(
                    Text::new(&t(heading)).with_role(TextRole::Caption).dimmed(),
                ));
            }
            for row in devices.map_or_else(Vec::new, Self::device_rows) {
                body = body.push(row);
            }
            for action in actions {
                let chosen = Rc::clone(&self.chosen);
                let id = action.id.clone();
                body = body.push(Box::new(
                    Button::new(&format!("profile-action-{}", action.id), &action.title)
                        .with_icon(action.icon)
                        .role(ButtonRole::Plain)
                        .on_effect(move || {
                            *chosen.borrow_mut() = Some(ProfileChoice::Action(id.clone()));
                        }),
                ));
            }
            if let Some(key) = key {
                body = body.push(Self::key_row(key, true));
            }
        }
        if !model.linked.is_empty() {
            body = body.push(Box::new(Divider::horizontal())).push(Box::new(
                Text::new(&t("profile-section-linked"))
                    .with_role(TextRole::Caption)
                    .dimmed(),
            ));
            for account in &model.linked {
                body = body.push(self.linked_row(account, &model.name));
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
                    .on_view("profile-control-hints-toggle", move |on| {
                        *chosen.borrow_mut() = Some(ProfileChoice::ControlHints(on));
                    }),
            )),
        ))
    }

    /// Where the host keeps only how what they do is shared (a page): what they are doing in their
    /// own words, and how it is shared.
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

    /// What they are doing in their own words: Enter says it, an empty field clears it.
    fn status_field(&self) -> TextInput {
        let chosen = Rc::clone(&self.chosen);
        TextInput::from_state(&self.status_draft)
            .with_id("profile-status")
            .with_placeholder(t("profile-status-placeholder"))
            .with_accessible_name(t("profile-status"))
            .on_submit_view(move |typed| {
                *chosen.borrow_mut() = Some(ProfileChoice::Status(typed.trim().to_owned()));
            })
    }

    /// Status, colour and what people see: how the person shows to others. How what they are doing
    /// is shared, when the host keeps that choice.
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
            .on_view("profile-sharing", move |index| {
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
        .on_view("profile-availability", move |index| {
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
        // The colours as people travel in them: the dark material's presence palette, the one
        // identity colours are defined at.
        let palette = Theme::dark().tokens.colors.presence;
        let mut colours = ColorSwatchPicker::new(
            hues.iter()
                .map(|hue| ColorSwatch::named(palette.hue(*hue), hue.label())),
            // Two rows of six: the card's width, never past it.
            hues.len() / 2,
        )
        .on_change_view(move |index| {
            if let Some(hue) = hues.get(index) {
                *chosen.borrow_mut() = Some(ProfileChoice::Colour(hue.id().to_owned()));
            }
        });
        // The colour they chose, or, until they choose one, the colour their identity wears: the
        // card marks the one their face is drawn in.
        let worn = model
            .colour_id
            .as_deref()
            .and_then(ui::tokens::PresenceHue::from_id)
            .unwrap_or_else(|| ui::tokens::PresenceHue::of_identity(&model.identity));
        colours = colours.with_selected(worn.slot());
        body = body
            .push(Box::new(Divider::horizontal()))
            .push(heading("profile-section-colour"))
            .push(Box::new(colours));

        self.push_visibility(body, model)
    }

    /// What the people in this workspace see of the person, when the host keeps that choice.
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
        .on_view("profile-visibility", move |index| {
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
    /// A dimmed caption a screen reader reads when it changes.
    fn dimmed(text: &str) -> Box<dyn Widget> {
        Box::new(
            Text::new(text)
                .with_role(TextRole::Caption)
                .dimmed()
                .with_live(ui::a11y::Politeness::Polite),
        )
    }

    /// One service's row: connecting it, the account a link offers with Link and Cancel, or the
    /// linked account with Unlink, and what went wrong last. `me` is the person's name, as the
    /// offer asks it.
    fn linked_row(&self, account: &LinkedAccount, me: &str) -> Box<dyn Widget> {
        let service = || ui::i18n::ArgValue::Text(account.service.clone());
        let icon = service_icon(&account.app);
        let chosen = Rc::clone(&self.chosen);
        let app = account.app.clone();
        let mut column = Column::new()
            .with_no_padding()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            // As tall as what it says: the card's body measures itself by its rows, and this one
            // must answer with its own height.
            .with_intrinsic_height();
        match &account.state {
            LinkState::Unlinked { connect, elsewhere } => {
                if *connect {
                    column = column.push(Box::new(
                        Button::new(
                            &format!("profile-link-{}", account.app),
                            &t_args("profile-link", &[("service", service())]),
                        )
                        .with_icon(icon)
                        .role(ButtonRole::Primary)
                        .on_effect(move || {
                            *chosen.borrow_mut() = Some(ProfileChoice::Link(app.clone()));
                        }),
                    ));
                }
                if let Some(elsewhere) = elsewhere {
                    column = column.push(Self::dimmed(elsewhere));
                }
            }
            LinkState::Connecting => {
                column = column.push(Self::dimmed(&t_args(
                    "profile-connecting",
                    &[("service", service())],
                )));
            }
            LinkState::Looking => {
                column = column.push(Self::dimmed(&t_args(
                    "profile-looking",
                    &[("service", service())],
                )));
            }
            LinkState::Offered(offer) => {
                column = self.offered(column, account, offer, me);
            }
            LinkState::Linked(name) => {
                column = column.push(self.linked_as(account, name));
            }
        }
        if let Some(problem) = &account.problem {
            column = column.push(Box::new(
                Text::new(problem)
                    .with_id(format!("profile-link-problem-{}", account.app))
                    .with_role(TextRole::Caption)
                    .with_live(ui::a11y::Politeness::Assertive),
            ));
        }
        Box::new(column)
    }

    /// The account a link offers, asking whether to link it to `me` , with Link and Cancel.
    fn offered(
        &self,
        column: Column,
        account: &LinkedAccount,
        offer: &tilo_app::linked_accounts::OfferReading,
        me: &str,
    ) -> Column {
        let service = || ui::i18n::ArgValue::Text(account.service.clone());
        let icon = service_icon(&account.app);
        let chosen = Rc::clone(&self.chosen);
        let app = account.app.clone();
        let declined = Rc::clone(&chosen);
        let declining = app.clone();
        column
            .push(Box::new(
                Text::new(&t_args(
                    "profile-link-offer",
                    &[
                        ("service", service()),
                        ("account", ui::i18n::ArgValue::Text(offer.account.label())),
                        ("name", ui::i18n::ArgValue::Text(me.to_owned())),
                    ],
                ))
                .with_id(format!("profile-link-offer-{}", account.app))
                .with_live(ui::a11y::Politeness::Polite),
            ))
            .push(Self::dimmed(&t_args(
                "profile-link-offer-why",
                &[("service", service())],
            )))
            .push(Box::new(
                Row::new()
                    .no_wrap()
                    .with_align_items(taffy::AlignItems::Center)
                    .push(Box::new(
                        Button::new(
                            &format!("profile-link-accept-{}", account.app),
                            &t("profile-link-accept"),
                        )
                        .with_icon(icon)
                        .role(ButtonRole::Primary)
                        .on_effect(move || {
                            *chosen.borrow_mut() = Some(ProfileChoice::Accept(app.clone()));
                        }),
                    ))
                    .push(Box::new(
                        Button::new(
                            &format!("profile-link-decline-{}", account.app),
                            &t("profile-link-decline"),
                        )
                        .role(ButtonRole::Plain)
                        .on_effect(move || {
                            *declined.borrow_mut() =
                                Some(ProfileChoice::Decline(declining.clone()));
                        }),
                    )),
            ))
    }

    /// Someone's account elsewhere: "Linked to @ada on Discord".
    fn linked_to(link: &tilo_app::linked_accounts::ProfileLink) -> Box<dyn Widget> {
        let icon = tilo_app::linked_accounts::service_name_icon(&link.service);
        Box::new(
            Row::new()
                .no_wrap()
                .with_align_items(taffy::AlignItems::Center)
                .push(Box::new(ui::widget::IconWidget::new(icon)))
                .push(Box::new(
                    Text::new(&t_args(
                        "profile-linked-to",
                        &[
                            ("handle", format!("@{}", link.handle).into()),
                            ("service", link.service.clone().into()),
                        ],
                    ))
                    .with_id(format!(
                        "profile-other-linked-{}",
                        link.service.to_lowercase()
                    ))
                    .with_role(TextRole::Caption),
                )),
        )
    }

    /// The linked account, with Unlink.
    fn linked_as(
        &self,
        account: &LinkedAccount,
        name: &tilo_app::linked_accounts::AccountName,
    ) -> Box<dyn Widget> {
        let icon = service_icon(&account.app);
        let chosen = Rc::clone(&self.chosen);
        let app = account.app.clone();
        // One line under the section's "Linked accounts": the handle, and the name the account
        // shows when that is another, muted. Cut short rather than wrapped; read out whole.
        let (handle, shown) = linked_line(name);
        let mut who = Row::new()
            .no_wrap()
            .with_align_items(taffy::AlignItems::Center)
            .push(Box::new(ui::widget::IconWidget::new(icon)))
            .push(Box::new(
                Text::new(&handle)
                    .with_id(format!("profile-linked-{}", account.app))
                    .with_spoken(t_args(
                        "profile-linked-as",
                        &[("account", ui::i18n::ArgValue::Text(name.label()))],
                    ))
                    .with_live(ui::a11y::Politeness::Polite),
            ));
        if let Some(shown) = shown {
            who = who.push(Box::new(Text::new(&shown).dimmed()));
        }
        Box::new(
            Row::new()
                .no_wrap()
                .with_align_items(taffy::AlignItems::Center)
                .with_justify(taffy::JustifyContent::SpaceBetween)
                .push(Box::new(who))
                .push(Box::new(
                    Button::new(
                        &format!("profile-unlink-{}", account.app),
                        &t("profile-unlink"),
                    )
                    .with_icon(Icon::Unlink)
                    .on_effect(move || {
                        *chosen.borrow_mut() = Some(ProfileChoice::Unlink(app.clone()));
                    }),
                )),
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
        .with_label(if model.other.is_some() {
            t_args("profile-card-of", &[("name", model.name.clone().into())])
        } else {
            t("profile-card")
        });
        self.popover.set_viewport(viewport);
        self.popover.set_anchor(anchor);
        self.popover.set_open(true);
        self.replaced = true;
    }

    /// Open the card for `model` under `anchor` (the own avatar) within `viewport` , or close it
    /// when it is open.
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

    /// Keep the open card under `anchor` within `viewport` as they move: a card opened while the
    /// page was still finding its size goes where it belongs once it has. Whether it moved.
    pub fn follow_anchor(&mut self, anchor: Rect, viewport: Rect) -> bool {
        if !self.is_open() || self.placed == (anchor, viewport) || anchor.is_zero_area() {
            return false;
        }
        self.placed = (anchor, viewport);
        self.popover.set_viewport(viewport);
        self.popover.set_anchor(anchor);
        self.replaced = true;
        true
    }

    /// What came of a linked account's choice (a link made its code, an account was linked or
    /// unlinked): the open card shows `linked` now. They are the person's own, so someone else's
    /// card stays as it is.
    pub fn set_linked(&mut self, linked: Vec<LinkedAccount>) {
        let Some(model) = self.model.as_mut().filter(|model| model.other.is_none()) else {
            return;
        };
        if model.linked == linked {
            return;
        }
        model.linked = linked;
        let model = model.clone();
        if self.is_open() {
            // The same card, its contents built again: the popover keeps where it is and the
            // pointer it is tracking.
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

    /// Whether the card already shows `model`: someone else's card says what is said of them now
    /// ([`Self::updated`]), so a host reads this before laying the card out again. Their own card
    /// says yes always: it is not built again for anyone else's word.
    #[must_use]
    pub fn shows(&self, model: &ProfileModel) -> bool {
        match self.model.as_ref() {
            Some(held) if held.other.is_some() && model.other.is_some() => held == model,
            _ => true,
        }
    }

    /// Someone else's card kept to what is said of them now: what they are doing, where they are
    /// and whose view they follow move while their card stands (an agent narrates every action),
    /// so the card says now, never what was said when it opened. Their own card is left as it is:
    /// what the person types there is theirs, and a field rebuilt is a field they lost. Whether
    /// anything changed.
    pub fn updated(&mut self, model: &ProfileModel) -> bool {
        if self.shows(model) {
            return false;
        }
        self.model = Some(model.clone());
        if self.is_open() {
            // The same card, its contents built again: the popover keeps where it is and the
            // pointer it is tracking.
            let body = Box::new(self.body(model));
            self.popover.content_mut().set_child(body);
            self.replaced = true;
            // The control under the pointer is gone; focus goes back into the card.
            self.focus_reset = true;
        }
        true
    }

    /// Close the card. A name typed but not yet entered is kept, as a field that loses focus keeps
    /// what was typed.
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
        self.popover.needs_redraw() || self.popover.needs_animation_frame()
    }

    fn name(&self) -> &'static str {
        "ProfileCard"
    }

    fn get_rect(&self) -> Rect {
        self.popover.get_rect()
    }
}

/// What the desktop asks of a profile card, as the trait object [`PROFILE_CARD`] makes.
pub trait ProfileCardApi: Widget {
    fn toggle(&mut self, model: &ProfileModel, anchor: Rect, viewport: Rect);
    fn follow_anchor(&mut self, anchor: Rect, viewport: Rect) -> bool;
    fn set_linked(&mut self, linked: Vec<LinkedAccount>);
    fn close(&mut self);
    fn is_open(&self) -> bool;
    fn take_choice(&mut self) -> Option<ProfileChoice>;
}

impl ProfileCardApi for ProfileCard {
    fn toggle(&mut self, model: &ProfileModel, anchor: Rect, viewport: Rect) {
        Self::toggle(self, model, anchor, viewport);
    }
    fn follow_anchor(&mut self, anchor: Rect, viewport: Rect) -> bool {
        Self::follow_anchor(self, anchor, viewport)
    }
    fn set_linked(&mut self, linked: Vec<LinkedAccount>) {
        Self::set_linked(self, linked);
    }
    fn close(&mut self) {
        Self::close(self);
    }
    fn is_open(&self) -> bool {
        Self::is_open(self)
    }
    fn take_choice(&mut self) -> Option<ProfileChoice> {
        Self::take_choice(self)
    }
}

/// The profile card's code, as a program that fetches it on first use hands it over.
#[derive(Clone, Copy)]
pub struct MakeProfileCard(fn() -> Box<dyn ProfileCardApi>);

/// The profile card this build links.
#[must_use]
pub fn builtin_profile_card() -> MakeProfileCard {
    MakeProfileCard(|| Box::new(ProfileCard::new()))
}

ui::lazy_code! {
    /// The profile card's code: linked, or with `lazy-profile-card` fetched the first time a card
    /// opens.
    pub static PROFILE_CARD: MakeProfileCard = builtin_profile_card, feature = "lazy-profile-card";
}

/// A profile card whose code may still be on its way: it opens once it is in, and a quiet panel
/// stands under the avatar meanwhile.
pub struct LazyProfileCard {
    /// Where the card's code comes from: [`PROFILE_CARD`].
    code: &'static ui::lazy_code::LazyCode<MakeProfileCard>,
    card: Option<Box<dyn ProfileCardApi>>,
    /// The card was asked to open before its code was in: with what, under which anchor and in
    /// which viewport. Linked accounts that arrive meanwhile go into this model, so the card opens
    /// showing them.
    opening: Option<(ProfileModel, Rect, Rect)>,
    /// The empty node a card that is not made yet lays out as.
    empty: Option<NodeId>,
}

impl Default for LazyProfileCard {
    fn default() -> Self {
        Self::with_code(&PROFILE_CARD)
    }
}

impl LazyProfileCard {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A card whose code comes from `code` .
    fn with_code(code: &'static ui::lazy_code::LazyCode<MakeProfileCard>) -> Self {
        Self {
            code,
            card: None,
            opening: None,
            empty: None,
        }
    }

    /// The card, made once its code is in; `None` until then.
    fn card(&mut self) -> Option<&mut Box<dyn ProfileCardApi>> {
        if self.card.is_none() {
            let make = self.code.get()?;
            self.card = Some((make.0)());
        }
        self.card.as_mut()
    }

    /// Open the card, or close it when it is open.
    pub fn toggle(&mut self, model: &ProfileModel, anchor: Rect, viewport: Rect) {
        if self.opening.take().is_some() {
            return;
        }
        match self.card() {
            Some(card) => card.toggle(model, anchor, viewport),
            None => self.opening = Some((model.clone(), anchor, viewport)),
        }
    }

    /// Follow the anchor; whether the card moved. A card asked to open before its code was in opens
    /// here once it is.
    pub fn follow_anchor(&mut self, anchor: Rect, viewport: Rect) -> bool {
        if let Some((model, _, _)) = self.opening.as_ref()
            && self.code.get().is_some()
        {
            let model = model.clone();
            self.opening = None;
            if let Some(card) = self.card() {
                card.toggle(&model, anchor, viewport);
            }
            return true;
        }
        if let Some((_, placed, in_view)) = self.opening.as_mut() {
            let moved = *placed != anchor || *in_view != viewport;
            *placed = anchor;
            *in_view = viewport;
            return moved;
        }
        self.card
            .as_mut()
            .is_some_and(|card| card.follow_anchor(anchor, viewport))
    }

    /// As [`ProfileCard::set_linked`]. A card still waiting on its code takes them into the model
    /// it opens with: they answer the refresh its opening asked for, and arrive before a card made
    /// later would take them (a card that never opened has no model to put them in).
    pub fn set_linked(&mut self, linked: Vec<LinkedAccount>) {
        if let Some(card) = self.card.as_mut() {
            card.set_linked(linked);
        } else if let Some((model, _, _)) = self.opening.as_mut()
            && model.other.is_none()
        {
            model.linked = linked;
        }
    }

    pub fn close(&mut self) {
        self.opening = None;
        if let Some(card) = self.card.as_mut() {
            card.close();
        }
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.opening.is_some() || self.card.as_ref().is_some_and(|card| card.is_open())
    }

    pub fn take_choice(&mut self) -> Option<ProfileChoice> {
        self.card.as_mut().and_then(|card| card.take_choice())
    }

    /// Where the quiet panel stands while the card's code is on its way: under the anchor, as wide
    /// as the card, inside the viewport.
    fn waiting_rect(anchor: Rect, viewport: Rect, theme: &Theme) -> Rect {
        let width = f64::from(theme.tokens.metrics.panel_width_utility * theme.text_scale.max(1.0));
        let height = f64::from(theme.text(TextRole::Body).line_height_px())
            + 2.0 * f64::from(theme.padding_standard);
        let x0 = (anchor.x1 - width)
            .max(viewport.x0)
            .min(viewport.x1 - width);
        let y0 = anchor.y1 + f64::from(theme.gap_standard);
        Rect::new(x0, y0, x0 + width, y0 + height)
    }
}

impl Widget for LazyProfileCard {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if let Some(card) = self.card.as_mut() {
            return card.build_layout(taffy, theme, map);
        }
        let node = taffy
            .new_leaf(taffy::Style::default())
            .unwrap_or_else(|_| taffy::NodeId::from(0_u64));
        self.empty = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Some(card) = self.card.as_mut() {
            card.apply_layout(taffy, node, origin);
        }
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        self.card
            .as_mut()
            .is_some_and(|card| card.update(state, path))
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        if let Some(card) = self.card.as_mut() {
            card.sync(state, path);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(card) = self.card.as_mut() {
            card.handle_event(event, ctx);
        }
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some((_, anchor, viewport)) = self.opening {
            let rect = Self::waiting_rect(anchor, viewport, theme);
            ui::lazy_code::draw_loading(scene, transform, text, theme, rect, "lazy-loading");
            return;
        }
        if let Some(card) = self.card.as_mut() {
            let card: &mut dyn Widget = &mut **card;
            card.draw_child(scene, transform, text, theme, presence);
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.card
            .as_deref()
            .map(|card| card as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        match self.card.as_mut() {
            Some(card) => vec![card.as_mut() as &mut dyn Widget],
            None => Vec::new(),
        }
    }

    fn take_focus_reset(&mut self) -> bool {
        self.card
            .as_mut()
            .is_some_and(|card| card.take_focus_reset())
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.card
            .as_mut()
            .is_some_and(|card| card.take_structure_dirty())
    }

    fn traps_focus(&self) -> bool {
        self.card.as_ref().is_some_and(|card| card.traps_focus())
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.card.as_ref().and_then(|card| card.modal_capture())
    }

    fn wants_pointer_capture(&self) -> bool {
        self.card
            .as_ref()
            .is_some_and(|card| card.wants_pointer_capture())
    }

    fn needs_redraw(&self) -> bool {
        self.opening.is_some()
            || self
                .card
                .as_ref()
                .is_some_and(|card| card.needs_redraw() || card.needs_animation_frame())
    }

    // Before its code is in, the card is an empty leaf whose size never
    // changes. A widget without children reads as layout-dirty by default,
    // which had every frame of the page re-solve its layout for a card
    // nobody opened.
    fn is_layout_dirty(&self) -> bool {
        self.card
            .as_ref()
            .is_some_and(|card| card.is_layout_dirty())
    }

    fn layout_needs_measure(&self) -> bool {
        self.card
            .as_ref()
            .is_some_and(|card| card.layout_needs_measure())
    }

    fn name(&self) -> &'static str {
        "ProfileCard"
    }

    fn get_rect(&self) -> Rect {
        self.card
            .as_ref()
            .map_or(Rect::ZERO, |card| card.get_rect())
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
            problem: None,
        }
    }

    fn unlinked() -> LinkState {
        LinkState::Unlinked {
            connect: true,
            elsewhere: Some("In the Discord server, type /link to link from there.".into()),
        }
    }

    fn ada_account() -> tilo_app::linked_accounts::AccountName {
        tilo_app::linked_accounts::AccountName {
            user_name: "ada.l".into(),
            display_name: Some("Ada L".into()),
        }
    }

    fn offered() -> LinkState {
        LinkState::Offered(tilo_app::linked_accounts::OfferReading {
            service: "Discord".into(),
            account: ada_account(),
            started_here: false,
        })
    }

    /// The card open with `linked` , in a frame that clicks.
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

    /// A build that links the card makes it at once: the lazy card opens,
    /// shows it as its child and closes as the card does.
    #[test]
    fn a_linked_lazy_card_opens_and_closes_as_the_card() {
        let mut card = LazyProfileCard::new();
        assert!(!card.is_open());
        assert!(card.children().is_empty(), "made on first use");
        let anchor = Rect::new(300.0, 0.0, 332.0, 32.0);
        let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
        card.toggle(&model(), anchor, viewport);
        assert!(card.is_open());
        assert_eq!(card.children().len(), 1);
        card.close();
        assert!(!card.is_open());
    }

    /// A card nobody opened is an empty leaf of a fixed size: it asks no frame of the page to
    /// re-solve the layout, as it did by reading as layout-dirty, which made every frame (each
    /// keystroke's, each of an animation's) measure the whole desktop.
    #[test]
    fn an_unopened_lazy_card_asks_no_frame_to_measure() {
        let card = LazyProfileCard::new();
        assert!(!card.is_layout_dirty());
        assert!(!card.layout_needs_measure());
    }

    /// Linked accounts that arrive while the card's code is on its way (the refresh opening the own
    /// card asks for) show once it opens: the Discord row is there, as it is on a card whose code
    /// was linked.
    #[test]
    fn a_lazy_card_opens_with_accounts_that_arrived_while_it_loaded() {
        static CODE: ui::lazy_code::LazyCode<MakeProfileCard> = ui::lazy_code::LazyCode::fetched();
        ui::i18n::I18nManager::init("en-US");
        let mut card = LazyProfileCard::with_code(&CODE);
        let anchor = Rect::new(1200.0, 2.0, 1220.0, 22.0);
        let viewport = Rect::new(0.0, 0.0, 1280.0, 720.0);
        card.toggle(&model(), anchor, viewport);
        assert!(
            card.is_open() && card.children().is_empty(),
            "still loading"
        );
        card.set_linked(vec![discord(unlinked())]);
        CODE.install(builtin_profile_card());
        assert!(
            card.follow_anchor(anchor, viewport),
            "opens once the code is in"
        );
        let mut frame =
            ui::headless::Framed::new(ui::State::new(), card, (1280, 720), Theme::dark());
        let harness = ui::headless::Framed::harness_mut(&mut frame);
        harness.poll_frame().unwrap();
        let said = harness.inspection_json().unwrap();
        assert!(said.contains("Connect Discord"), "{said}");
        assert!(said.contains("type /link"), "{said}");
    }

    /// Opening the card is not editing: focus rests on the card itself, and Tab (or a click) enters
    /// the name field.
    #[test]
    fn the_card_opens_focused_on_itself_and_tab_enters_the_name() {
        // The card's keyboard stops in tree order, as both shells' focus order walks them; a trap
        // that opens focuses the first.
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

    /// A person who has not named themselves is asked, gently, in the card: "Add your name", in
    /// place of the hint, until they have.
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

    /// Of the host's ways to keep who you are, the card offers the two it lists first: keeping the
    /// identity and backing it up. A third overlapping ask stays in search, where it does not
    /// compete here. Devices stay as listed.
    #[test]
    fn the_card_offers_two_ways_to_keep_your_identity() {
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
        assert!(said.contains("Save a recovery key"), "{said}");
        assert!(!said.contains("Sign in with a passkey"), "{said}");
        assert!(said.contains("Pair a new device"), "{said}");
    }

    #[test]
    fn no_linked_accounts_section_without_a_connected_service() {
        let mut frame = framed(Vec::new());
        assert!(!says(&mut frame).contains("Linked accounts"));
        let mut frame = framed(vec![discord(unlinked())]);
        assert!(says(&mut frame).contains("Linked accounts"));
    }

    #[test]
    fn connect_keeps_the_card_open_and_says_where_it_goes() {
        let mut frame = framed(vec![discord(unlinked())]);
        let said = says(&mut frame);
        assert!(said.contains("Connect Discord"), "{said}");
        assert!(said.contains("type /link"), "the other way in: {said}");
        click(&mut frame, "profile-link-so.tilo.discord");
        assert_eq!(
            frame.take_choice(),
            Some(ProfileChoice::Link("so.tilo.discord".into()))
        );
        assert!(frame.is_open(), "the card waits for the consent page");
        frame.set_linked(vec![discord(LinkState::Connecting)]);
        let said = says(&mut frame);
        assert!(said.contains("Opening Discord"), "{said}");
        assert_eq!(frame.draft(), "Kim", "the name field keeps what it had");
    }

    #[test]
    fn a_service_without_sign_in_only_says_how_to_link_from_there() {
        let mut frame = framed(vec![discord(LinkState::Unlinked {
            connect: false,
            elsewhere: Some("In the Discord server, type /link to link from there.".into()),
        })]);
        let said = says(&mut frame);
        assert!(!said.contains("profile-link-so.tilo.discord"), "{said}");
        assert!(said.contains("type /link"), "{said}");
    }

    /// A link from Discord's /link asks before it links: the account it offers and the person it
    /// links to, with Link and Cancel.
    #[test]
    fn an_offered_account_asks_before_it_links() {
        let mut frame = framed(vec![discord(offered())]);
        let said = says(&mut frame);
        assert!(
            said.contains("Link Discord Ada L · @ada.l to Kim?"),
            "{said}"
        );
        click(&mut frame, "profile-link-accept-so.tilo.discord");
        assert_eq!(
            frame.take_choice(),
            Some(ProfileChoice::Accept("so.tilo.discord".into()))
        );
        click(&mut frame, "profile-link-decline-so.tilo.discord");
        assert_eq!(
            frame.take_choice(),
            Some(ProfileChoice::Decline("so.tilo.discord".into()))
        );
        assert!(frame.is_open());
    }

    /// A page's own card with a linked account keeps its status field its height, and its rows
    /// follow one another: a linked row that outgrew its first measure squashed the field above it
    /// to nothing.
    #[test]
    fn a_linked_card_keeps_its_status_field() {
        ui::i18n::I18nManager::init("en-US");
        let mut card = ProfileCard::new();
        card.toggle(
            &ProfileModel {
                linked: vec![discord(LinkState::Linked(
                    tilo_app::linked_accounts::AccountName {
                        user_name: "quinn.davis".into(),
                        display_name: Some("Quinn".into()),
                    },
                ))],
                sharing: Some(ui::StatusSharing::Automatic),
                status: "Sketching".into(),
                control_hints: Some(true),
                ..model()
            },
            Rect::new(1350.0, 2.0, 1370.0, 22.0),
            Rect::new(0.0, 0.0, 1440.0, 900.0),
        );
        let mut frame =
            ui::headless::Framed::new(ui::State::new(), card, (1440, 900), Theme::dark());
        let harness = ui::headless::Framed::harness_mut(&mut frame);
        harness.poll_frame().unwrap();
        let targets = harness.targets();
        let at = |name: &str| {
            *targets
                .get(name)
                .unwrap_or_else(|| panic!("{name} is laid out: {targets:?}"))
        };
        let status = at("profile-status");
        let sharing = at("profile-sharing");
        let unlink = at("profile-unlink-so.tilo.discord");
        let hints = at("profile-control-hints-toggle");
        assert!(status.height() > 0.0, "the status field has its height");
        assert!(status.y1 <= sharing.y0, "{status:?} above {sharing:?}");
        assert!(sharing.y1 <= unlink.y0, "{sharing:?} above {unlink:?}");
        assert!(unlink.y1 <= hints.y0, "{unlink:?} above {hints:?}");
        // The linked row is one line, and the switch follows it closely,
        // inside the viewport: no gap where a wrapped line was measured.
        let linked = at("profile-linked-so.tilo.discord");
        assert!(linked.height() < 2.0 * 16.0 * 1.2, "one line: {linked:?}");
        assert!(
            hints.y0 - unlink.y1 < 72.0,
            "the switch follows the linked row: {unlink:?} then {hints:?}"
        );
        assert!(hints.y1 <= 900.0, "the switch is on screen: {hints:?}");
    }

    #[test]
    fn a_linked_line_is_cut_short_never_wrapped() {
        let account = |user: &str, shown: Option<&str>| tilo_app::linked_accounts::AccountName {
            user_name: user.into(),
            display_name: shown.map(str::to_owned),
        };
        assert_eq!(
            linked_line(&account("quinn.davis", Some("Quinn"))),
            ("@quinn.davis".into(), Some("Quinn".into()))
        );
        assert_eq!(
            linked_line(&account("quinn.davis", Some("quinn.davis"))),
            ("@quinn.davis".into(), None),
            "the name only when it is another"
        );
        let (handle, shown) =
            linked_line(&account("a_very_long_discord_username_x", Some("Someone")));
        assert_eq!(handle.chars().count(), LINKED_LINE_CHARS);
        assert!(handle.ends_with('…'), "{handle}");
        assert_eq!(shown, None, "the name gives way first");
        let (handle, shown) = linked_line(&account("quinn", Some("Quinn Davis-Montgomery")));
        assert_eq!(handle, "@quinn");
        let shown = shown.expect("room for some of the name");
        assert!(shown.ends_with('…'), "{shown}");
        assert!(handle.chars().count() + 1 + shown.chars().count() <= LINKED_LINE_CHARS);
    }

    #[test]
    fn a_linked_account_offers_unlink() {
        let mut frame = framed(vec![discord(LinkState::Linked(ada_account()))]);
        let said = says(&mut frame);
        assert!(
            said.contains("@ada.l") && said.contains("Ada L"),
            "shown by handle and name: {said}"
        );
        click(&mut frame, "profile-unlink-so.tilo.discord");
        assert_eq!(
            frame.take_choice(),
            Some(ProfileChoice::Unlink("so.tilo.discord".into()))
        );
        assert!(frame.is_open());
        frame.set_linked(vec![discord(unlinked())]);
        assert_eq!(frame.linked(), [discord(unlinked())]);
    }

    #[test]
    fn what_went_wrong_is_said_on_the_row() {
        let mut frame = framed(vec![LinkedAccount {
            problem: Some("This link ran out after ten minutes. Run /link again.".into()),
            ..discord(unlinked())
        }]);
        assert!(says(&mut frame).contains("ran out after ten minutes"));
    }

    /// Hosts the card as a shell does: the linked accounts change during a frame's update, not
    /// through the harness's own access to its root (which lays out from scratch).
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

        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
            self.card.handle_event(event, ctx);
        }

        fn children(&self) -> Vec<&dyn Widget> {
            vec![&self.card]
        }

        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            vec![&mut self.card]
        }

        fn needs_redraw(&self) -> bool {
            self.next.borrow().is_some()
                || (self.card.needs_redraw() || self.card.needs_animation_frame())
        }

        fn name(&self) -> &'static str {
            "Shell"
        }

        fn get_rect(&self) -> Rect {
            self.card.get_rect()
        }
    }

    /// The card builds its widgets again when a linked account changes. A layout kept from before
    /// would still point at the ones they replaced, freed: the page read them as garbage once their
    /// memory was reused (after an app's code loaded), and trapped in an indirect call.
    #[test]
    fn a_rebuilt_card_is_laid_out_again_before_a_frame_reaches_it() {
        ui::i18n::I18nManager::init("en-US");
        let mut card = ProfileCard::new();
        card.toggle(
            &ProfileModel {
                linked: vec![discord(unlinked())],
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
        *next.borrow_mut() = Some(vec![discord(offered())]);
        assert!(harness.poll_frame().unwrap(), "the change is drawn");
        assert!(
            harness.rebuilt_frames() > before,
            "the frame lays the rebuilt card out again instead of keeping the old layout"
        );
        assert!(harness.inspection_json().unwrap().contains("@ada.l"));
    }

    /// The linked accounts the host hears about are the person's own. While someone else's card is
    /// open (opened from an author in Chat) they keep arriving, the first answer a couple of
    /// seconds after opening: the card is not built again under the pointer for them, it stays open
    /// as it was and goes on answering presses.
    #[test]
    fn someone_elses_card_stays_as_it_was_when_your_linked_accounts_arrive() {
        ui::i18n::I18nManager::init("en-US");
        let mut card = ProfileCard::new();
        card.toggle(
            &ada(true, crate::collaborators::LineSource::Generated),
            Rect::new(600.0, 300.0, 602.0, 302.0),
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
        *next.borrow_mut() = Some(vec![discord(unlinked())]);
        harness.poll_frame().unwrap();
        assert_eq!(
            harness.rebuilt_frames(),
            before,
            "nothing of the card was built again"
        );
        let said = harness.inspection_json().unwrap();
        assert!(!said.contains("Linked accounts"), "{said}");
        harness.click_named("profile-follow").unwrap();
        harness.poll_frame().unwrap();
        let card = &mut frame.card;
        assert!(card.is_open(), "the card stays open");
        assert_eq!(
            card.take_choice(),
            Some(ProfileChoice::Follow("ada-device".into()))
        );
    }

    /// Where the host keeps only how what they do is shared (a page), the card still has the field
    /// for what they are doing in their own words, holding what they said.
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
    fn an_open_card_follows_its_avatar_once_the_page_has_its_size() {
        ui::i18n::I18nManager::init("en-US");
        let mut card = ProfileCard::new();
        let tiny = Rect::new(0.0, 0.0, 40.0, 40.0);
        card.toggle(&model(), Rect::new(10.0, 0.0, 30.0, 20.0), tiny);
        let (avatar, page) = (
            Rect::new(1340.0, 4.0, 1364.0, 28.0),
            Rect::new(0.0, 0.0, 1440.0, 900.0),
        );
        assert!(
            card.follow_anchor(avatar, page),
            "moves to where it belongs"
        );
        assert!(!card.follow_anchor(avatar, page), "and stays");
        assert!(
            !card.follow_anchor(Rect::ZERO, page),
            "not to an avatar not drawn"
        );
        card.close();
        assert!(
            !card.follow_anchor(avatar, tiny),
            "a closed card stays closed"
        );
    }

    #[test]
    fn toggling_again_closes() {
        let mut card = opened();
        card.toggle(&model(), Rect::ZERO, Rect::new(0.0, 0.0, 800.0, 600.0));
        assert!(!card.is_open());
    }

    const ADA_KEY: &str = "a6d40c19e7b25f83c1d9e0a4b7f26c58d3e1a9b0c7f4e2d6a8b5c3e1f0d9a7c2";

    fn ada(can_message: bool, status: crate::collaborators::LineSource) -> ProfileModel {
        ProfileModel {
            name: "Ada".into(),
            identity: "ada-device".into(),
            key: Some(ADA_KEY.into()),
            other: Some(OtherPerson {
                id: "ada-device".into(),
                status: Some(crate::collaborators::RosterStatus {
                    text: "Reviewing the checklist".into(),
                    source: status,
                }),
                place: None,
                presence: Some(ui::Availability::Available),
                following: false,
                can_follow: true,
                can_message,
                linked: Vec::new(),
                agent: None,
            }),
            ..ProfileModel::default()
        }
    }

    fn framed_model(model: &ProfileModel) -> ui::headless::Framed<ProfileCard> {
        ui::i18n::I18nManager::init("en-US");
        let mut card = ProfileCard::new();
        card.toggle(
            model,
            Rect::new(1000.0, 2.0, 1020.0, 22.0),
            Rect::new(0.0, 0.0, 1280.0, 720.0),
        );
        ui::headless::Framed::new(ui::State::new(), card, (1280, 720), Theme::dark())
    }

    fn claude(now: &str) -> ProfileModel {
        ProfileModel {
            name: "Claude Code".into(),
            identity: "claude-run".into(),
            other: Some(OtherPerson {
                id: "claude-run".into(),
                status: Some(crate::collaborators::RosterStatus {
                    text: now.into(),
                    source: crate::collaborators::LineSource::Observed,
                }),
                place: None,
                presence: None,
                following: false,
                can_follow: true,
                can_message: false,
                linked: Vec::new(),
                agent: Some(AgentFacts {
                    client: "Claude Code".into(),
                }),
            }),
            ..ProfileModel::default()
        }
    }

    /// An agent's card says what it is at now: the run narrates each action while its card stands,
    /// and the card must not go on quoting what it was at when it opened.
    #[test]
    fn an_agents_card_says_what_it_is_doing_now_while_it_stands() {
        let mut frame = framed_model(&claude("Looking at the apps"));
        let said = says(&mut frame);
        assert!(said.contains("Looking at the apps"), "{said}");
        assert!(
            said.contains("Now"),
            "it reads as what it is at now: {said}"
        );
        let (card, _) = ui::headless::Framed::split(&mut frame);
        assert!(
            !card.shows(&claude("Working in Notes")),
            "the row moved past the card"
        );
        assert!(card.updated(&claude("Working in Notes")), "the line moved");
        assert!(
            card.shows(&claude("Working in Notes")),
            "and the card has it"
        );
        let said = says(&mut frame);
        assert!(said.contains("Working in Notes"), "{said}");
        assert!(
            !said.contains("Looking at the apps"),
            "not what it opened with: {said}"
        );
        assert!(frame.is_open(), "the card keeps standing");
        let (card, _) = ui::headless::Framed::split(&mut frame);
        assert!(
            !card.updated(&claude("Working in Notes")),
            "the same line again builds nothing"
        );
    }

    /// Your own card is yours to type in: what is said of other people never rebuilds it.
    #[test]
    fn your_own_card_is_never_built_again_from_what_is_said_of_someone_else() {
        let mut frame = framed_model(&model());
        assert!(says(&mut frame).contains("Your name"));
        let (card, _) = ui::headless::Framed::split(&mut frame);
        assert!(!card.updated(&claude("Working in Notes")));
        assert!(
            says(&mut frame).contains("Your name"),
            "still your own card, untouched"
        );
    }

    #[test]
    fn someone_elses_card_says_who_they_are_and_offers_writing_and_following() {
        let mut frame = framed_model(&ada(true, crate::collaborators::LineSource::Generated));
        let said = says(&mut frame);
        assert!(said.contains("Ada’s profile"), "{said}");
        assert!(said.contains("a6d40c19…a7c2"), "the id, short: {said}");
        assert!(said.contains("Written automatically"), "{said}");
        // Nothing of the person's own card: no name field, no identity.
        assert!(!said.contains("Your name"), "{said}");
        click(&mut frame, "profile-message");
        assert_eq!(
            frame.take_choice(),
            Some(ProfileChoice::Message(ADA_KEY.into()))
        );

        let mut frame = framed_model(&ada(false, crate::collaborators::LineSource::Written));
        let said = says(&mut frame);
        assert!(said.contains("In their own words"), "{said}");
        assert!(
            !said.contains("\"Message\""),
            "no conversation to open: {said}"
        );
        click(&mut frame, "profile-follow");
        assert_eq!(
            frame.take_choice(),
            Some(ProfileChoice::Follow("ada-device".into()))
        );
    }

    #[test]
    fn your_own_card_shows_your_id_and_your_pictures_buttons() {
        let mut own = model();
        own.key = Some(ADA_KEY.into());
        for id in [SET_PHOTO, RESET_PHOTO] {
            own.actions.push(ProfileAction {
                id: id.into(),
                title: id.into(),
                group: ProfileGroup::Identity,
                icon: Icon::Fingerprint,
            });
        }
        let mut frame = framed_model(&own);
        let said = says(&mut frame);
        assert!(said.contains("Change picture"), "{said}");
        assert!(said.contains("Remove picture"), "{said}");
        assert!(said.contains("Copy ID"), "{said}");
        click(&mut frame, "profile-action-set-photo");
        assert_eq!(
            frame.take_choice(),
            Some(ProfileChoice::Action(SET_PHOTO.into()))
        );
        assert_eq!(short_key("abc"), "abc");
    }

    #[test]
    fn someone_elses_card_names_their_linked_accounts() {
        let mut model = ada(true, crate::collaborators::LineSource::Written);
        let mut frame = framed_model(&model);
        assert!(!says(&mut frame).contains("Linked to"));
        if let Some(other) = &mut model.other {
            other.linked = vec![tilo_app::linked_accounts::ProfileLink {
                service: "Discord".into(),
                handle: "ada".into(),
                picture: None,
            }];
        }
        let mut frame = framed_model(&model);
        let said = says(&mut frame);
        assert!(said.contains("Linked to @ada on Discord"), "{said}");
        assert!(
            !said.contains("Unlink"),
            "only their own card unlinks: {said}"
        );
    }
}

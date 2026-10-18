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
    Avatar, AvatarSize, Button, ButtonRole, Column, Container, Divider, Popover, PopoverSide, Row,
    Text, TextInput, Widget,
};
use ui::{LocalState, TextRole};
use vello::kurbo::{Point, Rect};
use vello::peniko::Color;
use winit::event::WindowEvent;

use crate::i18n::{t, t_args};
pub use tilo_app::linked_accounts::{LinkState, LinkedAccount};

/// What a profile action is about, so the card can group them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileGroup {
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
    /// The person's colour.
    pub colour: Option<Color>,
    /// The person may change their name here.
    pub can_rename: bool,
    pub actions: Vec<ProfileAction>,
    /// The services the room is connected to, with the person's account
    /// at each; none hides the section.
    pub linked: Vec<LinkedAccount>,
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
}

impl ProfileChoice {
    /// Whether the card closes on it: a linked account's choice keeps it
    /// open to show what came of it.
    fn closes(&self) -> bool {
        matches!(self, Self::Rename(_) | Self::Action(_))
    }
}

/// The card behind the bar's own avatar.
pub struct ProfileCard {
    popover: Popover<Container>,
    /// The name being typed.
    draft: LocalState<String>,
    /// The name the card opened with.
    shown: String,
    chosen: Rc<RefCell<Option<ProfileChoice>>>,
    focus_reset: bool,
    /// What the open card shows, to build it again when a linked account
    /// changes, and the anchor and viewport it opened at.
    model: Option<ProfileModel>,
    placed: (Rect, Rect),
}

impl Default for ProfileCard {
    fn default() -> Self {
        Self {
            popover: Popover::with_content(Container::new()),
            draft: LocalState::new(String::new()),
            shown: String::new(),
            chosen: Rc::default(),
            focus_reset: false,
            model: None,
            placed: (Rect::ZERO, Rect::ZERO),
        }
    }
}

impl ProfileCard {
    pub fn new() -> Self {
        Self::default()
    }

    fn body(&self, model: &ProfileModel) -> Column {
        let mut avatar = Avatar::person(&model.name, &model.identity).size(AvatarSize::Large);
        if let Some(colour) = model.colour {
            avatar = avatar.with_presence(colour);
        }
        let chosen = Rc::clone(&self.chosen);
        let name: Box<dyn Widget> = if model.can_rename {
            Box::new(
                TextInput::from_state(&self.draft)
                    .with_id("profile-name")
                    .with_placeholder(t("profile-name-placeholder"))
                    .with_accessible_name(t("profile-name"))
                    .on_submit(move |typed| {
                        let typed = typed.trim();
                        if !typed.is_empty() {
                            *chosen.borrow_mut() = Some(ProfileChoice::Rename(typed.to_owned()));
                        }
                    }),
            )
        } else {
            Box::new(Text::new(&model.name).with_role(TextRole::Subheading))
        };
        let who = Column::new()
            .with_no_padding()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .push(name)
            .push(Box::new(
                Text::new(&t("profile-name-hint"))
                    .with_role(TextRole::Caption)
                    .dimmed(),
            ));
        let header = Row::new()
            .no_wrap()
            .with_align_items(taffy::AlignItems::Center)
            .push(Box::new(avatar))
            .push(Box::new(who));
        let mut body = Column::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .with_padding_rhythm(ui::tokens::Rhythm::Related, ui::tokens::Rhythm::Related)
            .push(Box::new(header));
        for (group, heading) in [
            (ProfileGroup::Identity, "profile-section-identity"),
            (ProfileGroup::Devices, "profile-section-devices"),
        ] {
            let actions: Vec<&ProfileAction> = model
                .actions
                .iter()
                .filter(|action| action.group == group)
                .collect();
            if actions.is_empty() {
                continue;
            }
            body = body.push(Box::new(Divider::horizontal())).push(Box::new(
                Text::new(&t(heading)).with_role(TextRole::Caption).dimmed(),
            ));
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
    }

    /// Open the card for `model` under `anchor` (the own avatar) within
    /// `viewport`, or close it when it is open.
    pub fn toggle(&mut self, model: &ProfileModel, anchor: Rect, viewport: Rect) {
        if self.is_open() {
            self.close();
            return;
        }
        self.shown.clone_from(&model.name);
        self.draft.set(model.name.clone());
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
            // The control chosen is gone; focus goes back into the card.
            self.focus_reset = true;
        }
    }

    /// The linked accounts the card shows.
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

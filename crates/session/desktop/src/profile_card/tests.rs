//! What the person's own card does, driven as a person drives it: the fields that keep what is
//! typed without a Save button, the face that changes the picture, and what the card says of
//! someone else.

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
    let mut frame = ui::headless::Framed::new(ui::State::new(), card, (1280, 720), Theme::dark());
    let harness = ui::headless::Framed::harness_mut(&mut frame);
    harness.poll_frame().unwrap();
    let said = harness.inspection_json().unwrap();
    assert!(said.contains("Connect Discord"), "{said}");
    assert!(said.contains("type /link"), "{said}");
}

/// Someone else's card asked open while its code is on its way opens saying what is said of them
/// once it is in, not what was said when it was asked for: the roster hands the waiting card each
/// change, as it hands an open card.
#[test]
fn a_lazy_card_for_someone_else_opens_with_what_is_said_of_them_now() {
    static CODE: ui::lazy_code::LazyCode<MakeProfileCard> = ui::lazy_code::LazyCode::fetched();
    ui::i18n::I18nManager::init("en-US");
    let said = |text: &str| ProfileModel {
        name: "Ada".into(),
        identity: "ada-root".into(),
        other: Some(OtherPerson {
            id: "ada-device".into(),
            status: Some(crate::collaborators::RosterStatus {
                text: text.into(),
                source: crate::collaborators::LineSource::Observed,
            }),
            ..OtherPerson::default()
        }),
        ..ProfileModel::default()
    };
    let mut card = LazyProfileCard::with_code(&CODE);
    let anchor = Rect::new(1200.0, 2.0, 1220.0, 22.0);
    let viewport = Rect::new(0.0, 0.0, 1280.0, 720.0);
    card.toggle(&said("Reading the brief"), anchor, viewport);
    assert!(card.shows(&said("Reading the brief")));
    assert!(!card.shows(&said("Reviewing the checklist")));
    assert!(card.updated(&said("Reviewing the checklist")));
    assert!(card.shows(&said("Reviewing the checklist")));
    CODE.install(builtin_profile_card());
    assert!(
        card.follow_anchor(anchor, viewport),
        "opens once the code is in"
    );
    let mut frame = ui::headless::Framed::new(ui::State::new(), card, (1280, 720), Theme::dark());
    let harness = ui::headless::Framed::harness_mut(&mut frame);
    harness.poll_frame().unwrap();
    let shown = harness.inspection_json().unwrap();
    assert!(shown.contains("Reviewing the checklist"), "{shown}");
    assert!(!shown.contains("Reading the brief"), "{shown}");
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
    let mut frame = ui::headless::Framed::new(ui::State::new(), card, (1280, 720), Theme::dark());
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
    let mut frame = ui::headless::Framed::new(ui::State::new(), card, (1440, 900), Theme::dark());
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
    let (handle, shown) = linked_line(&account("a_very_long_discord_username_x", Some("Someone")));
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
    let mut frame = ui::headless::Framed::new(ui::State::new(), shell, (1280, 720), Theme::dark());
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
    let mut frame = ui::headless::Framed::new(ui::State::new(), shell, (1280, 720), Theme::dark());
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

/// A person whose card has a picture, a status and a way of sharing it: the card as the web
/// page shows it, with the fields the page's host offers.
fn with_status_and_photo() -> ProfileModel {
    let mut model = model();
    model.actions.push(ProfileAction {
        id: SET_PHOTO.into(),
        title: "Change picture".into(),
        group: ProfileGroup::Identity,
        icon: Icon::Camera,
    });
    model.sharing = Some(ui::StatusSharing::Automatic);
    model.status = "Building".into();
    model
}

/// Type `text` into the card's field `id`, as the person would: point at it, take what is
/// there, and type.
fn type_into(frame: &mut ui::headless::Framed<ProfileCard>, id: &str, text: &str) {
    let harness = ui::headless::Framed::harness_mut(frame);
    harness.poll_frame().unwrap();
    harness.click_named(id).unwrap();
    harness.poll_frame().unwrap();
    harness.key("Ctrl+A");
    harness.type_text(text);
    harness.poll_frame().unwrap();
}

/// The card has no Save button: typing a name and going on to something else keeps it, as a
/// settings page does. It used to be kept only on Enter or when the card closed.
#[test]
fn a_name_typed_and_left_is_kept() {
    let mut frame = framed_model(&with_status_and_photo());
    type_into(&mut frame, "profile-name", " Grace ");
    // Pointing at the status field leaves the name field.
    let harness = ui::headless::Framed::harness_mut(&mut frame);
    harness.click_named("profile-status").unwrap();
    harness.poll_frame().unwrap();
    let (card, _) = ui::headless::Framed::split(&mut frame);
    assert_eq!(
        card.take_choice(),
        Some(ProfileChoice::Rename("Grace".into())),
        "the name they typed is theirs, without Enter"
    );
    assert!(
        card.take_choice().is_none(),
        "and it is said once, never again on every frame"
    );
}

/// What they are doing is kept the same way: typed, then the card closed, with no Enter.
#[test]
fn a_status_typed_is_kept_when_the_card_closes() {
    let mut frame = framed_model(&with_status_and_photo());
    type_into(&mut frame, "profile-status", "Reviewing the signup");
    ui::headless::Framed::harness_mut(&mut frame).key("Escape");
    ui::headless::Framed::harness_mut(&mut frame)
        .poll_frame()
        .unwrap();
    assert!(!frame.is_open(), "Escape closed the card");
    assert_eq!(
        frame.take_choice(),
        Some(ProfileChoice::Status("Reviewing the signup".into()))
    );
    assert!(frame.take_choice().is_none(), "said once");
}

/// Typing and stopping is the same as leaving the field: a pause keeps what was typed, so a
/// person who writes a status and waits has said it.
#[test]
fn a_pause_after_typing_keeps_what_was_typed() {
    let mut frame = framed_model(&with_status_and_photo());
    type_into(&mut frame, "profile-status", "Reviewing the signup");
    assert!(frame.take_choice().is_none(), "not while it is typed");
    let due = Widget::next_wake_deadline(&*frame).expect("a typed field asks to be kept");
    assert!(
        Widget::serve_deadline(&mut *frame, due + TYPING_SETTLE),
        "the pause came"
    );
    assert_eq!(
        frame.take_choice(),
        Some(ProfileChoice::Status("Reviewing the signup".into()))
    );
    assert!(frame.take_choice().is_none(), "said once");
}

/// A field that says what is already kept says nothing: opening the card and pressing nothing
/// keeps nothing.
#[test]
fn a_card_left_alone_keeps_nothing() {
    let mut frame = framed_model(&with_status_and_photo());
    let harness = ui::headless::Framed::harness_mut(&mut frame);
    harness.poll_frame().unwrap();
    harness.key("Escape");
    harness.poll_frame().unwrap();
    assert!(frame.take_choice().is_none());
}

/// Your picture is the button that changes it: a press on the face opens the picker, as
/// "Change picture" does, and a reader hears the face called that.
#[test]
fn your_picture_is_the_button_that_changes_it() {
    let mut frame = framed_model(&with_status_and_photo());
    let said = says(&mut frame);
    assert!(said.contains("Change picture"), "{said}");
    click(&mut frame, "profile-photo");
    assert_eq!(
        frame.take_choice(),
        Some(ProfileChoice::Action(SET_PHOTO.into()))
    );
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

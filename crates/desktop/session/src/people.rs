//! Who this person is here, and what the people surfaces ask of the session: the shell draws
//! who is here from the `session:people` reading ([`crate::people_device`]), and what its rows
//! and cards ask comes back as a [`PeopleAsk`].

/// What the roster's card asks the session to do: what only it can.
pub enum PeopleAsk {
    /// Open the conversation this person has with `key`'s person in Chat,
    /// its composer ready: the app is started when it is not up, and the
    /// pair's record is shown either way.
    Message { key: String },
    /// The name the person gave themselves on their own card.
    Rename(String),
    /// The picture the person chose for themselves, by its address in the content store; empty
    /// goes back to their initials.
    Picture(String),
    /// What they say they are doing, in their own words.
    Status(String),
    /// How available they say they are.
    Availability(ui::Availability),
    /// The presence colour they chose, by its id.
    Colour(String),
    /// Open the Settings page the person chose.
    Settings(String),
    /// Do the host action this person's own card offers, as the palette and a banner hand theirs
    /// back: the choice the ask on their avatar waits for is answered here too.
    HostAction(String),
    /// Go to where `who` (by its presence key) works: the window of theirs shown here, else their
    /// app, as following them brings it forward once.
    GoTo { who: String },
    /// Ask this of the agent of `client`, whose card stands for the member `who` (by its presence
    /// key): an agent this device started is stopped here, anything else through its card in Console.
    Agent {
        who: String,
        client: String,
        ask: tilo_session_core::desktop::launchpad::AgentAsk,
    },
    /// Follow the person `who` (by their presence key), or stop following when `following` is off:
    /// the bar keeps their view in front while this person follows them.
    Follow { who: String, following: bool },
    /// Wave at the person `who` (by their presence key): their bar says so for a moment.
    Wave { who: String },
    /// Add another of this person's devices: the session's sheet prints the link a new one joins
    /// with.
    PairDevice,
}

/// The colour this person wears here: the one they chose in their own settings, else the one their
/// identity derives, so their face is the same on every surface and on every device that paints
/// them.
pub(crate) fn own_rgb(identity: &str) -> (u8, u8, u8) {
    tilo_ui_base::presence::identity_rgb(own_hue(identity))
}

/// The hue of [`own_rgb`].
pub(crate) fn own_hue(identity: &str) -> tilo_ui_base::tokens::PresenceHue {
    ::collaboration::local_identity::own_colour()
        .unwrap_or_else(|| tilo_ui_base::tokens::PresenceHue::of_identity(identity))
}

/// The colour this person's own avatar and card wear: in a live room, the one everyone here draws
/// them in (`HereState::own_rgb`, settled over the whole room and published on their pointer), so
/// they see themselves in the colour everyone else sees; with no room settled yet, [`own_rgb`].
pub(crate) fn own_live_rgb(sync: Option<&crate::sync::Client>, identity: &str) -> (u8, u8, u8) {
    sync.and_then(|sync| sync.here().borrow().own_rgb())
        .filter(|rgb| *rgb != tilo_ui_base::presence::UNSETTLED_RGB)
        .unwrap_or_else(|| own_rgb(identity))
}

/// The name and identity of this person's face, shared by their own surfaces and their presence.
/// In a live session the name is the one they gave, and stays empty until one is known so each
/// surface localizes the unnamed label. With no peer network here (`own` is `None`) nothing
/// publishes a name, so the person goes by the name this host knows whoever is at it by, as
/// [`crate::platform::person::person_name`] reads it: their own, else the account's.
/// The identity is the root key they are here by, else the id this device sits as.
pub(crate) fn own_face(
    own: Option<&crate::sync::OwnPresence>,
    device_person: &str,
) -> (String, String) {
    let Some(own) = own else {
        return (
            crate::platform::person::person_name(
                ::collaboration::local_identity::load().map(|identity| identity.display_name),
                "",
            ),
            device_person.to_owned(),
        );
    };
    let identity = own
        .user
        .as_deref()
        .map(str::trim)
        .filter(|user| !user.is_empty())
        .unwrap_or(device_person)
        .to_owned();
    let name = crate::platform::person::given_name(
        ::collaboration::local_identity::load().map(|identity| identity.display_name),
    )
    .unwrap_or_else(|| own.label.clone());
    (name, identity)
}

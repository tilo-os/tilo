//! The notification centre and its banners as the device publishes them.

use tilo_resource::Effect;

/// What the centre resource offers: what a notification kept there does.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.notifications")]
pub enum CentreAction {
    /// Dismiss a notification, or every one an app grouped
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Dismiss {
        /// The notification's id, as the rows carry it.
        #[param("The notification")]
        notification: Option<u64>,
        /// The app group its rows are listed under.
        #[param("The app group")]
        group: Option<String>,
    },
    /// Run one of a notification's own actions
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Act {
        /// The notification's id, as the rows carry it.
        #[param("The notification")]
        notification: u64,
        /// The key of one of the actions it offers.
        #[param("One of its actions")]
        action: String,
    },
    /// Clear the centre, keeping what the person pinned
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Clear,
    /// Bring back what the last Clear all took
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    UndoClear,
    /// Reply in a banner that offers a reply: what the person typed reaches its sender, and the
    /// banner goes
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Reply {
        /// The notification's id, as the banners carry it.
        #[param("The notification")]
        notification: u64,
        /// What the person typed.
        #[param("What to say back")]
        text: String,
    },
    /// Open one of the notifications the centre kept
    #[effect(kind = "outside")]
    #[effect(own_device)]
    OpenNotice {
        /// The notification's id, as the rows carry it.
        #[param("The notification")]
        notification: u64,
    },
    /// Hand a notification to the agent the person keeps: it works on what it was told, and
    /// its answer comes where their conversation with it is
    #[effect(kind = "outside")]
    #[effect(own_device)]
    HandToAgent {
        /// The notification's id, as the rows carry it.
        #[param("The notification")]
        notification: u64,
    },
    /// Tell the person something, as an app's `notify` does: it rises as a banner and waits in
    /// the centre until they dismiss it
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Notify {
        #[param("What happened, in a short sentence")]
        title: String,
        #[param("What it says, under the title")]
        body: Option<String>,
        #[param("Who it is from: an app's name, or the agent's; empty for the session's own")]
        from: Option<String>,
        #[param("Its kind: `info`, `success`, `warning` or `error`")]
        kind: Option<String>,
    },
    /// Hold fresh banners back until turned off again
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    SetQuiet {
        /// Whether to hold them back
        #[param("Whether to hold banners back")]
        on: bool,
    },
}

/// What a banner on screen does, by the id its row carries; left out, the newest banner does (for
/// a call, the ringing one).
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.banners")]
pub enum BannerAction {
    /// Open the notification
    #[effect(kind = "outside")]
    #[effect(own_device, attended)]
    OpenBanner {
        /// The banner to open; the newest when left out
        #[param("The banner to open; the newest when left out")]
        banner: Option<String>,
    },
    /// Dismiss the notification
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Dismiss {
        /// The banner to close; the newest when left out
        #[param("The banner to close; the newest when left out")]
        banner: Option<String>,
    },
    /// Choose on the notification
    #[effect(kind = "outside")]
    #[effect(own_device, attended)]
    Act {
        /// The banner; the newest when left out
        #[param("The banner; the newest when left out")]
        banner: Option<String>,
        /// The choice, by its key or its label (Snooze, Approve…)
        #[param("The choice, by its key or its label (Snooze, Approve…)")]
        choice: String,
    },
    /// Reply to the notification
    #[effect(kind = "outside")]
    #[effect(own_device, attended)]
    Reply {
        /// The banner; the newest when left out
        #[param("The banner; the newest when left out")]
        banner: Option<String>,
        /// What to reply
        #[param("What to reply")]
        text: String,
    },
    /// Snooze the notification
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Snooze {
        /// The banner; the newest when left out
        #[param("The banner; the newest when left out")]
        banner: Option<String>,
    },
    /// Focus the newest notification
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    FocusNewest,
    /// Read it in full
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Read {
        /// The banner; the newest when left out
        #[param("The banner; the newest when left out")]
        banner: Option<String>,
    },
    /// Answer the call
    #[effect(kind = "outside")]
    #[effect(own_device, attended)]
    AnswerCall {
        /// The call's banner; the ringing call when left out
        #[param("The call's banner; the ringing call when left out")]
        banner: Option<String>,
    },
    /// Decline the call
    #[effect(kind = "outside")]
    #[effect(own_device)]
    DeclineCall {
        /// The call's banner; the ringing call when left out
        #[param("The call's banner; the ringing call when left out")]
        banner: Option<String>,
    },
}

/// The centre as a view of another language reads it: what it keeps, why fresh banners are held,
/// and how long ago each notice arrived, which a notice carries as an age rather than a clock so a
/// view can say "3 min ago" without one.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct NotificationsView {
    /// How many the person has not read.
    pub count: usize,
    /// Why fresh banners are held (Do Not Disturb, presenting, a focus schedule); `None` while
    /// they show.
    #[serde(default)]
    pub quiet: Option<String>,
    /// The notices the centre keeps, newest first.
    #[serde(default)]
    pub notices: Vec<view::Notice>,
    /// The live banners, newest first, by their ids: what a shell that draws the stack itself
    /// shows.
    #[serde(default)]
    pub banners: Vec<u64>,
    /// The notice up as a banner now, by its id.
    #[serde(default)]
    pub banner: Option<u64>,
    /// Whether a Clear all can still be undone, so the centre offers Undo.
    #[serde(default)]
    pub undo: bool,
    /// The banner the keyboard is on, while it stands.
    #[serde(default)]
    pub focused: Option<u64>,
}

/// The notice a view reads, kept in a module of its own so its published name stays `Notice`
/// while the session's own message keeps that name here.
pub mod view {
    /// A notice as a view of another language reads it.
    #[derive(
        Clone,
        Debug,
        Default,
        PartialEq,
        Eq,
        serde::Serialize,
        serde::Deserialize,
        tilo_resource::JsonSchema,
    )]
    pub struct Notice {
        /// What the verbs name it by.
        pub number: u64,
        pub title: String,
        pub body: String,
        /// The app's name, or the agent's; empty for the session's own.
        pub from: String,
        /// `info`, `success`, `warning` or `error`.
        pub kind: String,
        pub seen: bool,
        /// What its sender offers, in its sender's order.
        #[serde(default)]
        pub actions: Vec<NoticeAction>,
        /// How long ago it arrived, in seconds.
        #[serde(default)]
        pub age_seconds: u64,
        /// Whether its body may show as a banner: a sender that asked for no preview keeps it in
        /// the centre alone.
        #[serde(default)]
        pub preview: bool,
    }

    /// One action a notice offers, as a view of another language reads it.
    #[derive(
        Clone,
        Debug,
        Default,
        PartialEq,
        Eq,
        serde::Serialize,
        serde::Deserialize,
        tilo_resource::JsonSchema,
    )]
    pub struct NoticeAction {
        pub key: String,
        pub label: String,
    }
}

/// The banners as a view of another language reads them (`device.banners`): the same centre the
/// shell draws them from, so a view presses a row it read.
#[must_use]
pub fn banners_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<NotificationsView, BannerAction>(
        "banners",
        "Notifications",
    )
}

/// The centre as a view of another language reads it (`device.notifications`).
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<NotificationsView, CentreAction>(
        "notifications",
        "Notifications",
    )
}

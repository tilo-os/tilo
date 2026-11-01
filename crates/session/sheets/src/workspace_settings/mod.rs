//! Workspace settings: the lens the top bar's workspace name opens, with a
//! tab for each part of the workspace a person looks after.
//!
//! The sheet shows and asks; it decides nothing. What it shows comes from
//! the shell, already in the person's words and already saying which
//! controls the person may use ([`Settings`]). What the person chooses goes
//! back to the shell as a request ([`WorkspaceSettingsEffect`]), which the
//! shell carries out on the workspace's own authenticated paths: those
//! check again whether the person may, whatever the sheet showed.

use std::cell::RefCell;
use std::rc::Rc;

use ui::advanced::WindowEvent;
use ui::framework::{Dispatcher, Effected, ResourceApp, ViewCx};
use ui::prelude::*;
use ui::widget::{Column, Element, ModalDialog, Segment, ViewTabs, Widget};

use crate::presentation::Presentation;

mod agents;
mod apps;
mod emoji;
mod general;
mod people;
mod permissions;

pub use agents::{
    AccessApp, Agent, AgentReview, Agents, Bot, Exception, Intro, KindLine, ModelChoice, Models,
};
pub use apps::{AppRow, Apps};
pub use emoji::{Emoji, EmojiPane, Notice, Picture};
pub use general::{Archived, Device, General, Group, Public, PublicControls};
pub use people::{Decision, Offers, People, Person, RoleChoice};
pub use permissions::{Choice, Composer, Holders, Permission, Permissions};

/// The app the shell mounts the sheet as.
pub const APP_ID: &str = "so.tilo.workspace-settings";

/// Widest the sheet is drawn: room for a tab's rows with their controls
/// beside them.
const WIDTH: f64 = 760.0;

/// The sheet's resource: the door its actions are published on. It holds
/// nothing; the workspace is the shell's.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct WorkspaceSettings {}

#[resource(kind = "workspace-settings.lens", label = "Workspace settings")]
impl WorkspaceSettings {}

/// What the sheet asks of the shell. Every change to the workspace is
/// decided again by the shell, on the path that checks who may make it.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "so.tilo.workspace-settings.change")]
pub enum WorkspaceSettingsEffect {
    /// Show a tab of workspace settings
    #[effect(own_device, kind = "read")]
    ShowTab {
        #[param("general, people, agents, permissions, apps or emoji")]
        tab: String,
    },
    /// Close workspace settings
    #[effect(own_device, kind = "read")]
    Close,
    /// Rename the workspace
    Rename {
        #[param("The workspace's new name")]
        name: String,
    },
    /// Invite people to the workspace
    #[effect(own_device, kind = "read")]
    Invite,
    /// Show the workspace on one of your devices, or keep it off
    #[effect(own_device)]
    ToggleDevice {
        #[param("The device, by its key in the list")]
        device: String,
    },
    /// Move the workspace one place earlier or later among yours
    #[effect(own_device)]
    Move {
        #[param("-1 for earlier, 1 for later")]
        step: i32,
    },
    /// List the workspace in its host's public directory, or take it out
    #[effect(attended)]
    SetPublic {
        #[param("Whether the workspace is listed")]
        public: bool,
    },
    /// Give people who join from the directory the next role
    #[effect(attended)]
    NextJoinRole,
    /// Say what the public workspace is
    #[effect(own_device, kind = "read")]
    Describe,
    /// Bring an archived workspace back
    #[effect(own_device)]
    Restore {
        #[param("The archived workspace, by its key in the list")]
        workspace: String,
    },
    /// Delete an archived workspace
    #[effect(own_device, destructive, attended)]
    DeleteArchived {
        #[param("The archived workspace, by its key in the list")]
        workspace: String,
    },
    /// Delete this workspace
    #[effect(destructive, attended)]
    Delete,
    /// Give a member of the workspace another role
    #[effect(attended)]
    SetRole {
        #[param("The member, by their key in the list")]
        member: String,
        #[param("viewer, commenter or editor")]
        role: String,
    },
    /// Remove a member from the workspace
    #[effect(destructive, attended)]
    RemoveMember {
        #[param("The member, by their key in the list")]
        member: String,
    },
    /// Look at what was done in the workspace
    #[effect(own_device, kind = "read")]
    ReviewActivity,
    /// Transfer the workspace's ownership, or answer a transfer
    #[effect(own_device, kind = "read")]
    Ownership,
    /// Admit an agent to the workspace
    #[effect(own_device, kind = "read")]
    AddAgent,
    /// Show what an agent may do in the workspace, app by app
    #[effect(own_device, kind = "read")]
    ReviewAgent {
        #[param("The agent, by its key in the list")]
        agent: String,
    },
    /// Back from an agent's review to the agents
    #[effect(own_device, kind = "read")]
    EndReview,
    /// Answer what an agent may do of one kind in one app
    #[effect(attended)]
    SetAgentAnswer {
        #[param("The agent, by its key in the list")]
        agent: String,
        #[param("The app, by its key in the review")]
        scope: String,
        #[param("read, change or outside")]
        kind: String,
        #[param("allow, ask or deny; empty to follow the agent's admission")]
        answer: String,
    },
    /// Return an agent's answers in one app to its admission's
    #[effect(attended)]
    ResetAgentApp {
        #[param("The agent, by its key in the list")]
        agent: String,
        #[param("The app, by its key in the review")]
        scope: String,
    },
    /// Choose the model an agent plans with
    #[effect(own_device)]
    SetAgentModel {
        #[param("The agent, by its key in the list")]
        agent: String,
        #[param("The model, by its key in the review; empty for the default")]
        model: String,
    },
    /// Take an agent out of the workspace
    #[effect(destructive, attended)]
    RemoveAgent {
        #[param("The agent, by its key in the list")]
        agent: String,
    },
    /// Find a bot for the workspace
    #[effect(own_device, kind = "read")]
    AddBot,
    /// Take back the workspace's request for a bot: its host stops it
    #[effect(destructive, attended)]
    RemoveBot {
        #[param("The bot, by its directory entry")]
        bot: String,
    },
    /// Give someone in the workspace a permission beyond their role
    #[effect(privileged, attended)]
    Grant {
        #[param("Who: member:…, agent:… or app:…, by its key in the composer")]
        subject: String,
        #[param("What it allows, by its key in the composer")]
        permission: String,
        #[param("never, session or once")]
        expiry: String,
    },
    /// Revoke a permission the workspace gives
    #[effect(destructive, attended)]
    Revoke {
        #[param("The permission, by its key in the list")]
        grant: String,
    },
    /// Open the workspace's saved roles
    #[effect(own_device, kind = "read")]
    SavedRoles,
    /// Have the next app open a kind of file or link for everyone here
    #[effect(attended)]
    NextWorkspaceDefault {
        #[param("The kind of file or link, by its key in the list")]
        resource: String,
    },
    /// Have the next app open a kind of file or link for you here
    #[effect(own_device)]
    NextPersonalDefault {
        #[param("The kind of file or link, by its key in the list")]
        resource: String,
    },
    /// Choose a picture and add it as one of the workspace's emoji
    #[effect(own_device, attended)]
    AddEmoji,
    /// Remove one of the workspace's emoji
    #[effect(destructive)]
    RemoveEmoji {
        #[param("The emoji's name, without colons")]
        name: String,
    },
    /// Put away the note about the last emoji not added or removed
    #[effect(own_device, kind = "read")]
    DismissEmojiNotice,
}

/// A tab of the sheet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Tab {
    #[default]
    General,
    People,
    Agents,
    Permissions,
    Apps,
    Emoji,
}

impl Tab {
    /// Every tab, in the order the strip shows them.
    pub const ALL: [Self; 6] = [
        Self::General,
        Self::People,
        Self::Agents,
        Self::Permissions,
        Self::Apps,
        Self::Emoji,
    ];

    /// What the tab is asked for by.
    pub fn key(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::People => "people",
            Self::Agents => "agents",
            Self::Permissions => "permissions",
            Self::Apps => "apps",
            Self::Emoji => "emoji",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.key() == key)
    }
}

/// One tab's name in the strip, and the shorter one a narrow sheet uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TabLabel {
    pub tab: Tab,
    pub label: String,
    pub short: String,
}

/// What the sheet shows: plain data the shell keeps current, in the
/// person's words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// What the sheet is called, for the tab strip and assistive tools.
    pub title: String,
    /// The workspace's name.
    pub name: String,
    /// The person's role in it.
    pub role: String,
    /// The label of the button that closes the sheet.
    pub close: String,
    pub tabs: Vec<TabLabel>,
    /// The tab shown.
    pub tab: Tab,
    pub general: General,
    pub people: People,
    pub agents: Agents,
    pub permissions: Permissions,
    pub apps: Apps,
    pub emoji: EmojiPane,
}

/// A change the sheet asks about before asking the shell for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Confirm {
    /// Remove the member `member`, called `name`.
    RemoveMember { member: String, name: String },
    /// Revoke the permission `grant`, which `holder` holds.
    Revoke { grant: String, holder: String },
}

/// What the sheet keeps to itself while it is up: the question it is
/// asking, and what of an agent's review is open.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Local {
    confirming: Option<Confirm>,
    /// The app whose answers are open in an agent's review.
    open_app: Option<String>,
    models_open: bool,
    /// What the permission composer has chosen.
    draft: permissions::Draft,
}

/// What the shell lends the sheet: whether it is up, what it shows, and
/// where its requests go.
#[derive(Clone, Default)]
pub struct WorkspaceSettingsSeat {
    presentation: Presentation,
    settings: Rc<RefCell<Settings>>,
    shown: Rc<RefCell<Option<LocalState<Settings>>>>,
    /// The name as typed, while the person renames the workspace.
    name: Rc<RefCell<Option<LocalState<String>>>>,
    asked: Rc<RefCell<Vec<WorkspaceSettingsEffect>>>,
}

impl std::fmt::Debug for WorkspaceSettingsSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceSettingsSeat")
            .field("presentation", &self.presentation)
            .finish_non_exhaustive()
    }
}

impl crate::Seat for WorkspaceSettingsSeat {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl WorkspaceSettingsSeat {
    /// Show `settings`, or the same settings as they stand now. A name the
    /// person has not started changing follows the workspace's.
    pub fn show(&self, settings: Settings) {
        if *self.settings.borrow() == settings {
            return;
        }
        let was = self.settings.borrow().name.clone();
        if let Some(name) = self.name.borrow().as_ref()
            && settings.name != was
            && name.get() == was
        {
            name.set(settings.name.clone());
        }
        if let Some(shown) = self.shown.borrow().as_ref() {
            shown.set(settings.clone());
        }
        *self.settings.borrow_mut() = settings;
    }

    /// What the sheet shows now.
    pub fn settings(&self) -> Settings {
        self.settings.borrow().clone()
    }

    /// The name as it stands in the field now.
    pub fn typed_name(&self) -> String {
        self.name
            .borrow()
            .as_ref()
            .map_or_else(|| self.settings.borrow().name.clone(), LocalState::get)
    }

    /// Start afresh, as a new opening does: the name field shows the
    /// workspace's name again.
    pub fn reset(&self) {
        if let Some(name) = self.name.borrow().as_ref() {
            name.set(self.settings.borrow().name.clone());
        }
    }

    /// The requests made since the shell last asked, oldest first.
    pub fn take_asked(&self) -> Vec<WorkspaceSettingsEffect> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }

    fn ask(&self, effect: WorkspaceSettingsEffect) {
        if effect == WorkspaceSettingsEffect::Close {
            self.presentation.gone();
        }
        self.asked.borrow_mut().push(effect);
    }
}

/// The sheet as an app.
pub struct WorkspaceSettingsApp;

impl ResourceApp for WorkspaceSettingsApp {
    type Resource = WorkspaceSettings;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Workspace settings";
    const ICON: Option<&'static str> = Some("layout-grid");
    // Over the desktop: what is not the sheet shows the desktop under it.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: the workspace keeps its own history.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn view(cx: &mut ViewCx<WorkspaceSettings>) -> Element {
        let seat = cx.take_lent::<WorkspaceSettingsSeat>();
        let initial = match &seat {
            Some(seat) => seat.settings(),
            None => cx
                .fixture::<Settings>()
                .map(|settings| (*settings).clone())
                .unwrap_or_default(),
        };
        let name = cx.local(initial.name.clone());
        let shown = cx.local(initial);
        let staged = cx.fixture::<Local>().map(|local| (*local).clone());
        let local = cx.local(staged.unwrap_or_default());
        if let Some(seat) = &seat {
            *seat.shown.borrow_mut() = Some(shown.clone());
            *seat.name.borrow_mut() = Some(name.clone());
            seat.presentation.wake_with(cx.waker());
        }
        let asking = seat.clone();
        cx.effects(move |effect: WorkspaceSettingsEffect, _cx| match &asking {
            Some(seat) => {
                seat.ask(effect);
                Ok(Effected::done())
            }
            None => Err("there is no workspace to change here".to_owned()),
        });
        let parts = Parts {
            dispatch: cx.dispatcher(),
            shown: shown.clone(),
            name,
            local: local.clone(),
            seat,
        };
        let sheet_parts = parts.clone();
        let content = cx.switch(&shown, move |cx, settings| {
            let settings = settings.clone();
            let parts = sheet_parts.clone();
            cx.switch(&local, move |_cx, local| match &local.confirming {
                Some(confirm) => decision(&settings, confirm, &parts),
                None => sheet(&settings, &parts),
            })
        });
        Box::new(Keys { content, parts })
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "general",
            "general-viewer",
            "people",
            "people-member",
            "people-removing",
            "agents",
            "agents-empty",
            "agent-review",
            "permissions",
            "permissions-revoking",
            "apps",
            "apps-empty",
            "emoji",
            "emoji-refused",
        ]
    }

    fn fixture(fixture: &mut ui::framework::Fixture<WorkspaceSettings>) {
        let settings = scenario(fixture.scenario());
        if fixture.scenario() == Some("agent-review") {
            fixture.provide(Local {
                open_app: Some("so.tilo.notes".to_owned()),
                models_open: true,
                ..Local::default()
            });
        }
        if fixture.scenario() == Some("permissions-revoking") {
            fixture.provide(Local {
                confirming: Some(Confirm::Revoke {
                    grant: "grant-2".to_owned(),
                    holder: "Ada Lovelace-Byron".to_owned(),
                }),
                ..Local::default()
            });
        }
        if fixture.scenario() == Some("people-removing") {
            fixture.provide(Local {
                confirming: Some(Confirm::RemoveMember {
                    member: "ada".to_owned(),
                    name: "Ada Lovelace-Byron".to_owned(),
                }),
                ..Local::default()
            });
        }
        fixture.provide(settings);
    }
}

/// What the sheet shows in one of its scenarios, for captures and tests.
pub fn scenario(name: Option<&str>) -> Settings {
    match name {
        Some("general-viewer") => Settings {
            role: "Viewer".to_owned(),
            general: general::sample_viewer(),
            ..sample()
        },
        Some("people" | "people-removing") => Settings {
            tab: Tab::People,
            ..sample()
        },
        Some("agents") => Settings {
            tab: Tab::Agents,
            ..sample()
        },
        Some("agents-empty") => Settings {
            tab: Tab::Agents,
            agents: Agents {
                agents: Vec::new(),
                bots: Vec::new(),
                intro: Intro {
                    title: "No agents".to_owned(),
                    ..agents::sample().intro
                },
                ..agents::sample()
            },
            ..sample()
        },
        Some("agent-review") => Settings {
            tab: Tab::Agents,
            agents: Agents {
                review: Some(agents::sample_review()),
                ..agents::sample()
            },
            ..sample()
        },
        Some("permissions" | "permissions-revoking") => Settings {
            tab: Tab::Permissions,
            ..sample()
        },
        Some("apps") => Settings {
            tab: Tab::Apps,
            ..sample()
        },
        Some("apps-empty") => Settings {
            tab: Tab::Apps,
            apps: Apps {
                rows: Vec::new(),
                ..apps::sample()
            },
            ..sample()
        },
        Some("emoji") => Settings {
            tab: Tab::Emoji,
            ..sample()
        },
        Some("emoji-refused") => Settings {
            tab: Tab::Emoji,
            role: "Viewer".to_owned(),
            emoji: emoji::sample_refused(),
            ..sample()
        },
        Some("people-member") => Settings {
            tab: Tab::People,
            role: "Editor".to_owned(),
            people: people::sample_member(),
            ..sample()
        },
        _ => sample(),
    }
}

/// A workspace as its owner sees it, for captures.
fn sample() -> Settings {
    let tab = |tab: Tab, label: &str| TabLabel {
        tab,
        label: label.to_owned(),
        short: label.to_owned(),
    };
    Settings {
        title: "Workspace settings".to_owned(),
        name: "Studio".to_owned(),
        role: "Owner".to_owned(),
        close: "Close".to_owned(),
        tabs: vec![
            tab(Tab::General, "General"),
            tab(Tab::People, "People"),
            tab(Tab::Agents, "Agents"),
            tab(Tab::Permissions, "Privacy & Security"),
            tab(Tab::Apps, "Apps & Files"),
            tab(Tab::Emoji, "Emoji"),
        ],
        tab: Tab::General,
        general: general::sample(),
        people: people::sample(),
        agents: agents::sample(),
        permissions: permissions::sample(),
        apps: apps::sample(),
        emoji: emoji::sample(),
    }
}

/// What every part of the view acts through.
#[derive(Clone)]
struct Parts {
    dispatch: Dispatcher<WorkspaceSettings>,
    shown: LocalState<Settings>,
    /// The name as typed.
    name: LocalState<String>,
    /// What the sheet keeps to itself while it is up.
    local: LocalState<Local>,
    seat: Option<WorkspaceSettingsSeat>,
}

impl Parts {
    /// Ask the shell for `effect`. The person's own press is their
    /// decision, as a key is; anyone acting for them goes through the
    /// door, which asks first where the effect says so.
    fn ask(&self, effect: WorkspaceSettingsEffect) {
        if matches!(
            effect,
            WorkspaceSettingsEffect::ReviewAgent { .. } | WorkspaceSettingsEffect::EndReview
        ) {
            // Each review opens with everything closed.
            self.local.update(|local| {
                local.open_app = None;
                local.models_open = false;
            });
        }
        match &self.seat {
            Some(seat) if self.dispatch.is_own_gesture(&effect) => seat.ask(effect),
            _ => self.dispatch.effect(&effect),
        }
    }

    /// Hand a changed name to the shell before the sheet moves on, as
    /// leaving the field does.
    fn commit_name(&self) {
        let settings = self.shown.get();
        let typed = self.name.get();
        let name = typed.trim();
        if settings.general.can_rename && !name.is_empty() && name != settings.name {
            self.ask(WorkspaceSettingsEffect::Rename {
                name: name.to_owned(),
            });
        } else if typed != settings.name {
            self.name.set(settings.name);
        }
    }

    fn show_tab(&self, tab: Tab) {
        self.commit_name();
        self.ask(WorkspaceSettingsEffect::ShowTab {
            tab: tab.key().to_owned(),
        });
    }

    fn close(&self) {
        self.commit_name();
        self.ask(WorkspaceSettingsEffect::Close);
    }

    /// Ask whether to make `change` before asking the shell for it.
    fn confirm(&self, change: Confirm) {
        self.local.update(|local| local.confirming = Some(change));
    }

    /// Open the answers of the app `scope` in an agent's review, or close
    /// them when they are open.
    fn toggle_app(&self, scope: &str) {
        self.local.update(|local| {
            local.open_app = if local.open_app.as_deref() == Some(scope) {
                None
            } else {
                Some(scope.to_owned())
            };
        });
    }

    /// Show or hide the models an agent may plan with.
    fn toggle_models(&self) {
        self.local
            .update(|local| local.models_open = !local.models_open);
    }

    /// Whether an agent's review is shown.
    fn reviewing(&self) -> bool {
        let settings = self.shown.get();
        settings.tab == Tab::Agents && settings.agents.review.is_some()
    }

    /// The person answered the question: keep things as they are, look at
    /// the workspace's activity, or go ahead.
    fn decide(&self, answer: Answer) {
        let Some(change) = self.local.get().confirming else {
            return;
        };
        self.local.update(|local| local.confirming = None);
        match answer {
            Answer::Keep => {}
            Answer::Review => self.ask(WorkspaceSettingsEffect::ReviewActivity),
            Answer::Confirm => self.ask(change.effect()),
        }
    }

    /// Whether the person is part way through renaming the workspace.
    fn renaming(&self) -> bool {
        self.name.get() != self.shown.get().name
    }
}

/// The whole sheet: the tab strip over the tab shown.
fn sheet(settings: &Settings, parts: &Parts) -> Element {
    let tabs: Vec<Tab> = settings.tabs.iter().map(|label| label.tab).collect();
    let selected = tabs
        .iter()
        .position(|tab| *tab == settings.tab)
        .unwrap_or_default();
    let segments = settings
        .tabs
        .iter()
        .map(|label| Segment::new(label.label.clone()).short(label.short.clone()))
        .collect();
    let switch = parts.clone();
    let strip = ViewTabs::new(segments, selected)
        .with_id("ws-settings-tabs")
        .with_accessible_name(settings.title.clone())
        .on_view("ws-settings-tabs", move |index| {
            if let Some(tab) = tabs.get(index) {
                switch.show_tab(*tab);
            }
        });
    let pane = match settings.tab {
        Tab::General => general::pane(settings, parts),
        Tab::People => people::pane(settings, parts),
        Tab::Agents => agents::pane(settings, parts),
        Tab::Permissions => permissions::pane(settings, parts),
        Tab::Apps => apps::pane(settings, parts),
        Tab::Emoji => emoji::pane(settings, parts),
    };
    let body = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(ui::tokens::Spacing::BASE.s4)
        .push(Box::new(strip))
        .push(pane);
    let close = parts.clone();
    Box::new(
        ModalDialog::new(settings.name.clone(), body)
            .with_test_id("ws-settings")
            .with_icon(ui::icons::Icon::LayoutGrid)
            .with_description(settings.role.clone())
            .with_max_width(WIDTH)
            .open(true)
            .secondary_action(settings.close.clone(), move || close.close()),
    )
}

impl Confirm {
    /// What the shell is asked for once the person confirms.
    fn effect(&self) -> WorkspaceSettingsEffect {
        match self {
            Self::RemoveMember { member, .. } => WorkspaceSettingsEffect::RemoveMember {
                member: member.clone(),
            },
            Self::Revoke { grant, .. } => WorkspaceSettingsEffect::Revoke {
                grant: grant.clone(),
            },
        }
    }

    /// The question it asks, and whom or what it is about.
    fn question<'a>(&self, settings: &'a Settings) -> (&'a Decision, String) {
        match self {
            Self::RemoveMember { name, .. } => (&settings.people.remove_decision, name.clone()),
            Self::Revoke { holder, .. } => (&settings.permissions.revoke_decision, holder.clone()),
        }
    }
}

/// How the person answered a [`Confirm`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    Keep,
    Review,
    Confirm,
}

/// The question a change asks, over the desktop in the sheet's place: what
/// it changes and for whom, with Keep (which Enter and Escape answer), the
/// way to the workspace's activity, and the change itself, which takes a
/// press of its own.
fn decision(settings: &Settings, confirm: &Confirm, parts: &Parts) -> Element {
    let (question, subject) = confirm.question(settings);
    let body = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(general::caption(&subject));
    let (keep, review, go) = (parts.clone(), parts.clone(), parts.clone());
    Box::new(
        ModalDialog::new(question.title.clone(), body)
            .with_test_id("ws-settings-decision")
            .with_description(question.description.clone())
            .open(true)
            .primary_action(question.confirm.clone(), move || go.decide(Answer::Confirm))
            .with_destructive(true)
            .secondary_action(question.keep.clone(), move || keep.decide(Answer::Keep))
            .tertiary_action(question.review.clone(), move || {
                review.decide(Answer::Review);
            }),
    )
}

/// Escape takes back a name being typed, and leaves an agent's review,
/// before it closes the sheet; the
/// sheet's own dialog walks its controls with Tab and presses them with
/// Enter and Space.
struct Keys {
    content: Element,
    parts: Parts,
}

impl ui::widget::Decorator for Keys {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        use ui::winit::keyboard::{Key, NamedKey};
        let escape = matches!(
            event,
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ui::winit::event::ElementState::Pressed
                    && key.logical_key == Key::Named(NamedKey::Escape)
        );
        let asking = self.parts.local.get().confirming.is_some();
        if escape && self.parts.renaming() {
            self.parts.name.set(self.parts.shown.get().name);
            *ctx.handled = true;
            return;
        }
        // Escape in an agent's review goes back to the agents.
        if escape && !asking && self.parts.reviewing() {
            self.parts.ask(WorkspaceSettingsEffect::EndReview);
            *ctx.handled = true;
            return;
        }
        self.content.handle_event(event, ctx);
    }
}

/// The app's definition, which the shell mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(WorkspaceSettingsApp)
}

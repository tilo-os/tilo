//! The calendar as a bound view: the [`Store`] the window shares with the
//! view's effect handler and what the view publishes, and [`window`], which
//! takes the window into the view.
//!
//! The effects answer here, from the calendar the document holds and what
//! the window last showed of it; what they ask of the window itself (a
//! view to show, an item to open, a toast) waits in [`Store::asks`] until
//! the window next looks.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::NaiveDate;
use tilo_contact_directory::ContactDirectory;
use ui::framework::{
    EffectAnswer, EffectCx, EffectReply, EffectStatus, Effected, Live, LiveRow, PersonRecord,
    Publishes, Setting, ViewCx,
};
use ui::widget::Element;
use ui::widget::toast::ToastKind;

use super::CalendarApp;
use super::opens::Opened;
use crate::account::AccountStore;
use crate::actions::CalendarCollectionResource;
use crate::display::CalendarDisplay;
use crate::ics;
use crate::model::{Calendar, Id, Palette, View};
use crate::outbox::DeviceAccounts;
use crate::resource::{CalendarEffect, SampleAccounts, ShotNow, t};

/// How many attendees the calendar publishes as rows of their own.
const ATTENDEE_ROWS: usize = 256;

/// The files the calendar imports and exports.
fn ics_filter() -> ui::portal::FileFilter {
    ui::portal::FileFilter::new("iCalendar files")
        .mimetype("text/calendar")
        .glob("*.ics")
}

/// What an effect asked of the window, done when it next looks.
pub(crate) enum Ask {
    /// Show what an `open` found.
    Open(Opened),
    /// Show the item a resource id names: a search hit, a clicked reminder.
    OpenItem(String),
    /// Show every calendar again.
    ShowAllCalendars,
    /// Switch to a view of the calendar.
    ShowView(View),
    /// Show the inbox of tasks.
    ShowTasks,
    /// Open the platform search on the contacts offered for an event.
    SearchContacts,
    /// Say something in the window's bar.
    Say(String, ToastKind),
    /// Stage a capture's scenario (`None` for a plain capture).
    Stage(Option<String>),
}

/// What the window and the view's effect handler share, and what the view
/// publishes beside the calendar.
pub(crate) struct Store {
    /// The calendar as the document holds it; `None` for a window outside
    /// the framework.
    pub(super) live: Option<Live<Calendar>>,
    /// The accounts configured on this device.
    device_accounts: DeviceAccounts,
    /// Where the person's display choices are kept, once followed.
    pub(super) display: Option<Setting<CalendarDisplay>>,
    /// The calendars the window shows: the one on this device and those of
    /// the accounts, as display choices apply to them.
    pub(super) calendars: Vec<CalendarCollectionResource>,
    /// Whether there are accounts to sync, real or a capture's sample ones.
    pub(super) has_accounts: bool,
    /// The day the window is on, which an exported file is named after.
    pub(super) anchor: NaiveDate,
    /// True while a background sync is running, which keeps the event loop
    /// ticking (`needs_animation_frame`) so the worker's result lands
    /// promptly without waiting for the next unrelated input.
    pub(super) syncing: bool,
    /// A sync effect asked for a pull, started on the next sync with the
    /// document.
    pub(super) sync_requested: bool,
    /// The caller of the sync effect, answered when its pull lands.
    pub(super) sync_reply: Option<EffectReply>,
    /// The person's address book, read in a live window to offer its
    /// people as attendees.
    pub(super) contact_directory: Option<PersonRecord<ContactDirectory>>,
    /// The event the Contacts picker was last opened for.
    pub(super) contact_event: Option<Id>,
    /// What effects asked of the window since it last looked.
    pub(super) asks: Vec<Ask>,
}

/// The [`Store`] as the window and the view share it.
pub(crate) type Shared = Rc<RefCell<Store>>;

impl Store {
    pub(super) fn new(anchor: NaiveDate, accounts: AccountStore) -> Self {
        Self {
            live: None,
            device_accounts: Rc::new(RefCell::new(accounts)),
            display: None,
            calendars: Vec::new(),
            has_accounts: false,
            anchor,
            syncing: false,
            sync_requested: false,
            sync_reply: None,
            contact_directory: None,
            contact_event: None,
            asks: Vec::new(),
        }
    }

    /// The accounts configured on this device, as the window keeps them.
    pub(super) fn device_accounts(&self) -> DeviceAccounts {
        Rc::clone(&self.device_accounts)
    }

    /// Ask `ask` of the window, done when it next looks.
    pub(crate) fn ask(&mut self, ask: Ask) {
        self.asks.push(ask);
    }

    /// Answer one of the calendar's effects, invoked by a control, the
    /// palette, an agent or a script.
    fn answer(
        &mut self,
        effect: CalendarEffect,
        me: &Shared,
        cx: &mut EffectCx<Calendar>,
    ) -> EffectAnswer {
        match effect {
            CalendarEffect::Open { item } => {
                let opened = Opened::find(&item, &cx.live().get(), &self.calendars)?;
                self.asks.push(Ask::Open(opened));
            }
            CalendarEffect::ReadCalendar { id } => {
                return self
                    .read_calendar(&id, &cx.live().get())
                    .map(Effected::value);
            }
            CalendarEffect::ShowAllCalendars => self.asks.push(Ask::ShowAllCalendars),
            CalendarEffect::ShowView { view } => {
                let view = match view.as_str() {
                    "month" => View::Month,
                    "week" => View::Week,
                    "day" => View::Day,
                    "agenda" => View::Agenda,
                    "tasks" => View::Tasks,
                    other => return Err(format!("there is no `{other}` view")),
                };
                self.asks.push(Ask::ShowView(view));
            }
            CalendarEffect::ShowMonth => self.asks.push(Ask::ShowView(View::Month)),
            CalendarEffect::ShowWeek => self.asks.push(Ask::ShowView(View::Week)),
            CalendarEffect::ShowDay => self.asks.push(Ask::ShowView(View::Day)),
            CalendarEffect::ShowTasks => self.asks.push(Ask::ShowTasks),
            CalendarEffect::ShowAgenda => self.asks.push(Ask::ShowView(View::Agenda)),
            CalendarEffect::Sync => {
                if self.syncing || self.sync_requested {
                    return Err("calendar synchronization is already running".into());
                }
                self.sync_requested = true;
                self.sync_reply = Some(cx.reply());
                return Ok(Effected::Pending);
            }
            CalendarEffect::ExportIcs => return self.export_ics(me, cx),
            CalendarEffect::ExportIcsContent { event } => {
                return crate::content_export::ics(&cx.live().get(), event).map(Effected::value);
            }
            CalendarEffect::ImportIcs => return Ok(import_ics(me, cx)),
            CalendarEffect::ContactAttendees { event_id } => {
                self.open_contact_attendees(&cx.live().get(), &event_id)?;
            }
            CalendarEffect::PrepareInMail {
                event_id,
                attendee_id,
            } => {
                let draft = invitation(&cx.live().get(), &event_id, &attendee_id)?;
                self.asks
                    .push(Ask::Say(t("calendar-invite-prepared"), ToastKind::Info));
                return Ok(Effected::value(draft));
            }
            CalendarEffect::SetColor { id, color } => {
                let color = Palette::from_name(&color)
                    .ok_or_else(|| format!("no colour is called {color}"))?;
                let slot = u8::try_from(color.index()).unwrap_or_default();
                self.change_display(&id, |display| {
                    display.colors.insert(id.clone(), slot);
                })?;
            }
            CalendarEffect::SetVisibility { id, visible } => {
                self.change_display(&id, |display| {
                    display.visible.insert(id.clone(), visible);
                })?;
                self.asks.push(Ask::Say(
                    CalendarApp::visibility_status(visible),
                    ToastKind::Info,
                ));
            }
        }
        Ok(Effected::done())
    }

    /// Change the person's display choices for the configured calendar `id`
    /// and keep them; the window shows them when it next looks.
    fn change_display(
        &self,
        id: &str,
        change: impl FnOnce(&mut CalendarDisplay),
    ) -> Result<(), String> {
        if !self
            .device_accounts
            .borrow()
            .all_calendars()
            .any(|(_, calendar)| calendar.id == id)
        {
            return Err("That calendar is no longer configured".into());
        }
        let setting = self
            .display
            .as_ref()
            .ok_or_else(|| "This window keeps no display choices".to_string())?;
        let mut display = setting.get();
        change(&mut display);
        setting
            .set(display)
            .map_err(|error| format!("Couldn't save the calendar display: {error}"))
    }

    /// Save every event and task to an iCalendar file the person chooses.
    fn export_ics(&mut self, me: &Shared, cx: &mut EffectCx<Calendar>) -> EffectAnswer {
        let calendar = cx.live().get();
        if calendar.events.is_empty() && calendar.tasks.is_empty() {
            self.asks
                .push(Ask::Say(t("calendar-export-empty"), ToastKind::Warning));
            return Err("There are no events or tasks to export".into());
        }
        let name = ics::suggested_filename(self.anchor);
        let request =
            ui::portal::PickRequest::save("Export calendar", "Export", &name).filter(ics_filter());
        let me = Rc::clone(me);
        Ok(cx.save_file(
            request,
            ics::export(&calendar).into_bytes(),
            move |(), saved, cx| {
                let said = if saved {
                    let calendar = cx.live().get();
                    let count = |items: usize| {
                        ui::i18n::ArgValue::from(i64::try_from(items).unwrap_or_default())
                    };
                    crate::resource::t_args(
                        "calendar-exported",
                        &[
                            ("events", count(calendar.events.len())),
                            ("tasks", count(calendar.tasks.len())),
                        ],
                    )
                } else {
                    t("calendar-export-cancelled")
                };
                me.borrow_mut().asks.push(Ask::Say(said, ToastKind::Info));
                Ok(Effected::done())
            },
        ))
    }

    /// The effects that cannot run right now, and why.
    fn statuses(&self) -> Vec<EffectStatus> {
        let mut statuses = Vec::new();
        if self.syncing || self.sync_requested {
            statuses.push(EffectStatus::unavailable(
                CalendarEffect::SYNC,
                "already_running",
                "Calendar synchronization is already running",
            ));
        } else if !self.has_accounts {
            statuses.push(EffectStatus::unavailable(
                CalendarEffect::SYNC,
                "no_accounts",
                "Add a calendar account before synchronizing",
            ));
        }
        if self.live.as_ref().is_some_and(|live| {
            let calendar = live.get();
            calendar.events.is_empty() && calendar.tasks.is_empty()
        }) {
            statuses.push(EffectStatus::unavailable(
                CalendarEffect::EXPORT_ICS,
                "calendar_empty",
                "There are no events or tasks to export",
            ));
        }
        statuses
    }

    /// What the calendar `id` is, its row's read: its name, whether its
    /// events are shown, its colour and how many of `model`'s events it
    /// holds.
    fn read_calendar(&self, id: &str, model: &Calendar) -> Result<serde_json::Value, String> {
        let calendar = self
            .calendars
            .iter()
            .find(|calendar| calendar.id == id)
            .ok_or_else(|| "calendar no longer exists".to_string())?;
        let local = id == crate::actions::LOCAL_CALENDAR_ID;
        let events = model
            .events
            .iter()
            .filter(|event| match &event.calendar_id {
                Some(of) => of == id,
                None => local,
            })
            .count();
        Ok(serde_json::json!({
            "id": calendar.id,
            "name": calendar.label,
            "visible": calendar.visible,
            "color": calendar
                .color
                .map(|color| Palette::from_index(color).name().to_ascii_lowercase()),
            "events": events,
        }))
    }

    /// The calendars of the accounts on this device, which can be opened,
    /// shown and hidden, and each event's attendees, whose invitation Mail
    /// prepares, and the people the Contacts picker offers.
    fn live_rows(&self) -> Vec<LiveRow> {
        let mut rows: Vec<LiveRow> = self
            .calendars
            .iter()
            .map(crate::actions::calendar_collection_row)
            .collect();
        rows.extend(self.attendee_rows());
        rows.extend(self.contact_rows());
        rows
    }

    /// Each event's attendees as rows offering to prepare their invitation
    /// in Mail, as many as [`ATTENDEE_ROWS`].
    fn attendee_rows(&self) -> Vec<LiveRow> {
        let Some(live) = &self.live else {
            return Vec::new();
        };
        let calendar = live.get();
        calendar
            .events
            .iter()
            .flat_map(|event| {
                event.attendees.iter().map(move |attendee| {
                    LiveRow::new(
                        crate::actions::attendee_resource_id(event.id, attendee.id),
                        "calendar.attendee",
                        attendee.email.clone(),
                    )
                    .effect(&CalendarEffect::PrepareInMail {
                        event_id: event.id.to_string(),
                        attendee_id: attendee.id.to_string(),
                    })
                })
            })
            .take(ATTENDEE_ROWS)
            .collect()
    }
}

/// Import the events and tasks of an iCalendar file the person chooses
/// through the real [`ics::parse`] path, as one action. Items already in
/// the calendar are not added twice.
fn import_ics(me: &Shared, cx: &mut EffectCx<Calendar>) -> Effected {
    let request = ui::portal::PickRequest::open("Import a calendar", "Import")
        .filter(ics_filter())
        .all_files();
    let me = Rc::clone(me);
    cx.choose_file(request, move |(), chosen, cx| {
        let Some(file) = chosen else {
            return Ok(Effected::done());
        };
        let payload = String::from_utf8(file.bytes)
            .map_err(|_| format!("{} is not an iCalendar file", file.name))?;
        let (import, unknown_zones) = crate::resource::import_reporting(&payload)
            .ok_or_else(|| format!("{} holds no events or tasks", file.name))?;
        cx.submit(import);
        me.borrow_mut().asks.push(Ask::Say(
            crate::resource::imported_status(&file.name, &unknown_zones),
            ToastKind::Info,
        ));
        Ok(Effected::done())
    })
}

/// The arguments of Mail's invitation draft for one attendee of an event
/// of `calendar`: a reviewable draft, never a claim that anything was sent.
fn invitation(
    calendar: &Calendar,
    event_id: &str,
    attendee_id: &str,
) -> Result<serde_json::Value, String> {
    let event_id = event_id
        .parse::<Id>()
        .map_err(|_| "invalid event id".to_string())?;
    let attendee_id = attendee_id
        .parse::<Id>()
        .map_err(|_| "invalid attendee id".to_string())?;
    let event = calendar
        .event(event_id)
        .ok_or_else(|| "event no longer exists".to_string())?;
    let attendee = event
        .attendees
        .iter()
        .find(|attendee| attendee.id == attendee_id)
        .ok_or_else(|| "attendee no longer exists".to_string())?;
    let subject = format!("Invitation: {}", event.display_title());
    let body = format!(
        "You’re invited to {} on {} at {} ({}).\n\nReview the attached calendar invitation for details.",
        event.display_title(),
        ui::format::day_heading(event.start.date(), true),
        ui::clock::time_of_day(&event.start),
        event.timezone
    );
    let calendar_ics = ics::invitation_request(event);
    Ok(serde_json::json!({
        "delivery": "not_sent",
        "target": {
            "resource": "mail.mailbox",
            "action": "prepare-calendar-invitation"
        },
        "arguments": {
            "to": attendee.email,
            "subject": subject,
            "body": body,
            "calendar_ics": calendar_ics
        },
        "next": "Invoke the target through the action broker; Mail may refuse with account_offline or invite permission denied."
    }))
}

/// The view of the calendar: `app` follows the document, its [`Store`]
/// answers the effects, and the view publishes the calendars, attendees and
/// Contacts choices beside it.
pub(crate) fn window(cx: &ViewCx<Calendar>, mut app: CalendarApp) -> Element {
    app.live = Some(cx.live().clone());
    app.state().live = Some(cx.live().clone());
    if let Some(now) = cx.fixture::<ShotNow>() {
        app.pin_now(now.0);
    }
    let live = cx.mode().is_live();
    let sample = cx.fixture::<SampleAccounts>();
    let accounts = if let Some(sample) = &sample {
        Some(sample.0.clone())
    } else if live {
        Some(app.bind_profile(cx.storage().config_root()))
    } else {
        None
    };
    app.follow_display(cx.settings::<CalendarDisplay>());
    if live {
        crate::outbox::drain(cx, app.device_accounts());
    }
    app.attach_framework(cx.dispatcher(), accounts, sample.is_some());
    if live {
        app.notify_with(ui::host::current().notifications, cx.waker());
        if let Some(accounts) = cx.shared_person::<accounts::person::PersonAccounts>() {
            app.follow_person_accounts(accounts);
        }
        app.state()
            .follow_contacts(cx.shared_person::<ContactDirectory>());
    }
    let store = Rc::clone(&app.store);
    {
        let (answer, status) = (Rc::clone(&store), Rc::clone(&store));
        cx.effects(move |effect: CalendarEffect, cx| {
            answer.borrow_mut().answer(effect, &answer, cx)
        })
        .status(move || status.borrow().statuses());
    }
    {
        let store = Rc::clone(&store);
        cx.on_stage(move |scenario| {
            store
                .borrow_mut()
                .ask(Ask::Stage(scenario.map(str::to_owned)));
        });
    }
    let (rows, row, edits, entries) = (
        Rc::clone(&store),
        Rc::clone(&store),
        Rc::clone(&store),
        store,
    );
    cx.publishing(
        Publishes::new()
            .live_rows(move || rows.borrow().live_rows())
            .live_row(move |id| row.borrow().contact_row(id))
            // A Contacts choice adds its person to the event through
            // `invite`.
            .row_edits(move |row| edits.borrow().contact_declarations(row))
            // The people of the address book the Contacts picker offers.
            .search_entries(move || entries.borrow().contact_search_entries()),
        Box::new(app),
    )
}

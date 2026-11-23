//! The calendar as a bound view: the [`Store`] the window shares with the
//! view's effect handler and what the view publishes, and [`window`], which
//! takes the window into the view.

use std::cell::RefCell;
use std::rc::Rc;

use jiff::civil::Date;
use tilo_contact_directory::ContactDirectory;
use ui::framework::{
    EffectAnswer, EffectCx, EffectReply, EffectStatus, Effected, Live, LiveRow, Publishes, Setting,
    ViewCx, WorkspaceResource,
};
use ui::tilo_resource::SharedResource as _;
use ui::widget::Element;
use ui::widget::toast::ToastKind;

use super::CalendarApp;
use super::opens::Opened;
use crate::account::CalendarAccounts;
use crate::actions::CalendarCollectionResource;
use crate::display::CalendarDisplay;
use crate::ics;
use crate::model::{Calendar, Id, Palette, View};
use crate::outbox::DeviceAccounts;
use crate::resource::{CalendarEffect, SampleAccounts, ShotNow, t};

const ATTENDEE_ROWS: usize = 256;

fn ics_filter() -> ui::portal::FileFilter {
    ui::portal::FileFilter::new("iCalendar files")
        .mimetype("text/calendar")
        .glob("*.ics")
}

pub(crate) enum Ask {
    Open(Opened),
    OpenItem(String),
    ShowAllCalendars,
    ShowView(View),
    ShowTasks,
    StartEvent,
    StartTask,
    SearchContacts,
    Say(String, ToastKind),
    Stage(Option<String>),
    /// A row of an event's menu chosen.
    Act(super::event_menu::EventAction),
}

pub(crate) struct Store {
    pub(super) live: Option<Live<Calendar>>,
    device_accounts: DeviceAccounts,
    pub(super) display: Option<Setting<CalendarDisplay>>,
    pub(super) calendars: Vec<CalendarCollectionResource>,
    pub(super) has_accounts: bool,
    pub(super) anchor: Date,
    pub(super) syncing: bool,
    pub(super) sync_requested: bool,
    pub(super) sync_reply: Option<EffectReply>,
    pub(super) contact_directory: Option<WorkspaceResource<ContactDirectory>>,
    pub(super) contact_event: Option<Id>,
    pub(super) asks: Vec<Ask>,
}

pub(crate) type Shared = Rc<RefCell<Store>>;

impl Store {
    pub(super) fn new(anchor: Date, accounts: CalendarAccounts) -> Self {
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

    pub(super) fn device_accounts(&self) -> DeviceAccounts {
        Rc::clone(&self.device_accounts)
    }

    pub(crate) fn ask(&mut self, ask: Ask) {
        self.asks.push(ask);
    }

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
                    .read_calendar(crate::rows::calendar_of_row(&id), &cx.live().get())
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
            CalendarEffect::StartEvent => self.asks.push(Ask::StartEvent),
            CalendarEffect::StartTask => self.asks.push(Ask::StartTask),
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
            CalendarEffect::PrepareInMail { attendee } => {
                let (event_id, attendee_id) = crate::rows::attendee_of_row(&attendee)
                    .ok_or_else(|| "that is not a guest of an event".to_string())?;
                let draft = invitation(&cx.live().get(), &event_id, &attendee_id)?;
                self.asks
                    .push(Ask::Say(t("calendar-invite-prepared"), ToastKind::Info));
                return Ok(Effected::value(draft));
            }
            CalendarEffect::SetColor { id, color } => {
                let color = Palette::from_name(&color)
                    .ok_or_else(|| format!("no colour is called {color}"))?;
                let id = crate::rows::calendar_of_row(&id);
                let slot = u8::try_from(color.index()).unwrap_or_default();
                self.change_display(id, |display| {
                    display.colors.insert(id.to_owned(), slot);
                })?;
            }
            CalendarEffect::ShowCalendar { id } => {
                self.set_visibility(crate::rows::calendar_of_row(&id), true)?;
                self.asks.push(Ask::Say(
                    CalendarApp::visibility_status(true),
                    ToastKind::Info,
                ));
            }
            CalendarEffect::HideCalendar { id } => {
                self.set_visibility(crate::rows::calendar_of_row(&id), false)?;
                self.asks.push(Ask::Say(
                    CalendarApp::visibility_status(false),
                    ToastKind::Info,
                ));
            }
        }
        Ok(Effected::done())
    }

    fn set_visibility(&self, id: &str, visible: bool) -> Result<(), String> {
        self.change_display(id, |display| {
            display.visible.insert(id.to_owned(), visible);
        })
    }

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
                    LiveRow::typed::<crate::rows::AttendeeRow>(
                        crate::actions::attendee_resource_id(event.id, attendee.id),
                        attendee.email.clone(),
                    )
                })
            })
            .take(ATTENDEE_ROWS)
            .collect()
    }

    /// What a calendar or a guest row is like now: a calendar is shown or
    /// hidden, never both, so its row offers only the change that applies.
    pub(crate) fn row_status(&self, id: &str) -> Vec<EffectStatus> {
        let Some(calendar) = self
            .calendars
            .iter()
            .find(|calendar| crate::actions::calendar_resource_id(&calendar.id) == id)
        else {
            return Vec::new();
        };
        if calendar.visible {
            vec![EffectStatus::unavailable(
                CalendarEffect::SHOW_CALENDAR,
                "already-shown",
                "This calendar's events are already shown",
            )]
        } else {
            vec![EffectStatus::unavailable(
                CalendarEffect::HIDE_CALENDAR,
                "already-hidden",
                "This calendar's events are already hidden",
            )]
        }
    }
}

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

/// The app whose action prepares an invitation's message: Mail.
pub(crate) const MAIL_APP: &str = "so.tilo.mail";
/// The workspace mailbox, whose action it is.
pub(crate) const MAIL_ROOT: &str = "tl.so.tilo.mail.Mail";
/// Mail's action that starts the message inviting one guest, by name.
pub(crate) const MAIL_INVITATION: &str = "prepare_calendar_invitation";

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
        ui::clock::time_of_day(event.start),
        event.timezone
    );
    let calendar_ics = ics::invitation_request(event);
    Ok(serde_json::json!({
        "delivery": "not_sent",
        "target": {
            "app": MAIL_APP,
            "resource": MAIL_ROOT,
            "action": MAIL_INVITATION
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

pub(crate) fn window(cx: &ViewCx<Calendar>, mut app: CalendarApp) -> Element {
    app.live = Some(cx.live().clone());
    app.state().live = Some(cx.live().clone());
    app.session = cx.session();
    if let Some(now) = cx.fixture::<ShotNow>() {
        app.pin_now(now.0);
    }
    let live = cx.mode().is_live();
    let sample = cx.fixture::<SampleAccounts>();
    let accounts = sample.as_ref().map(|sample| sample.0.clone());
    if live {
        app.bind_profile(cx.storage().config_root());
    }
    app.follow_display(cx.settings::<CalendarDisplay>());
    if live {
        crate::outbox::drain(cx, app.device_accounts());
    }
    app.attach_framework(cx.dispatcher(), accounts, sample.is_some());
    if live {
        app.notify_with(ui::process::current().notifications, cx.waker());
        if let Some(accounts) = cx
            .shared_workspace::<accounts::person::AccountList>(accounts::person::AccountList::OWNER)
        {
            app.follow_person_accounts(accounts);
        }
        app.state()
            .follow_contacts(cx.shared_workspace::<ContactDirectory>(ContactDirectory::OWNER));
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
    let (rows, row, edits, entries, statuses) = (
        Rc::clone(&store),
        Rc::clone(&store),
        Rc::clone(&store),
        Rc::clone(&store),
        store,
    );
    cx.publishing(
        Publishes::new()
            .live_rows(move || rows.borrow().live_rows())
            .live_row(move |id| row.borrow().contact_row(id))
            .row_edits(move |row| edits.borrow().contact_declarations(row))
            // A calendar's row offers showing or hiding it, whichever applies.
            .row_status(move |id| statuses.borrow().row_status(id))
            .search_entries(move || entries.borrow().contact_search_entries()),
        super::event_menu::with_event_menu(app),
    )
}

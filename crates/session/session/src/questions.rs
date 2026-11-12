//! The questions the services ask the person, shown on the session's sheets: the portal's and the
//! authentication agent's on the portal sheet, the file chooser's on the chooser sheet.
//! A question is up while it is pending, and answered through [`Questions`] once the person
//! decides. What is typed stays in the sheet's own buffers until then, so a password never travels
//! as an action's arguments.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use crate::app_names::claimant;

use serde_json::Value;
use tilo_actions::ResourceSpec;
use tilo_bluetooth_agent::{Method as PairingMethod, PairingDetails};
use tilo_network_secret_agent::questions::{
    CANNOT_JOIN_KIND, CannotJoinDetails, FieldRule, KIND as NETWORK_KIND, SecretsDetails,
    SimUnlockStep, Text as Words,
};
use tilo_polkit_agent::questions::{AuthenticateDetails, Notice};
use tilo_portal::account::AccountQuestion;
use tilo_portal::app_chooser::AppChooserQuestion;
use tilo_portal::background::{ACTIVE, BACKGROUND, BackgroundQuestion, RUNNING};
use tilo_portal::consent::{ALLOW_ALWAYS, ALLOW_ONCE, ALLOW_SESSION, AccessQuestion};
use tilo_portal::dynamic_launcher::LauncherQuestion;
use tilo_portal::email::EmailQuestion;
use tilo_portal::file_chooser::FileChooserQuestion;
use tilo_portal::global_shortcuts::{BIND_AS_PROPOSED, ShortcutsQuestion};
use tilo_portal::print::PrintQuestion;
use tilo_portal::screencast::{OfferedSource, ShareQuestion};
use tilo_portal::screenshot::ScreenshotQuestion;
use tilo_portal::streams::SourceKind;
use tilo_portal::trigger::Trigger;
use tilo_portal::usb::UsbQuestion;
use tilo_portal::wallpaper::WallpaperQuestion;
use tilo_service::{Answer, Question, Questions, Services};
use tilo_sheets::file_picker::{FileAnswer, FileMode, FilePickerSeat, FileRequest, FileType};
use tilo_sheets::portal::{
    Body, Device, Dialog, DraftRow, DraftValue, Field, PortalEffect, PortalSeat, Prompt,
    PromptField, PromptKind, Remember, Shortcut,
};
use tilo_sheets::{Seat as _, word, words};

/// The sheet that asked the person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sheet {
    /// The portal sheet: the portal's questions and the administrator's.
    Portal,
    /// The file chooser.
    Files,
}

/// What the person did with the question that was up.
enum Decision {
    Answer(Answer),
    Decline,
}

/// What came of one of the sheet's effects.
enum Step {
    /// Nothing to show for it.
    Nothing,
    /// The sheet shows what it did.
    Redraw,
    /// It cannot do that, and says why.
    Refused(String),
    /// The person decided.
    Decide(Decision),
}

/// What the sheet answers with once the person confirms.
enum Plan {
    /// The highlighted choice, answered as the number it stands for.
    Choose(Vec<usize>),
    /// Confirming is the answer; the question asks nothing else.
    Confirm,
    /// The ticked rows are the answer, named as the question names them.
    Rows { names: Vec<String>, remember: bool },
    /// The rows are shortcut bindings.
    Bind(Vec<Binding>),
    /// A launcher's name and icon.
    Launcher { name: bool, icons: usize },
    /// Confirming agrees, as choice 0.
    Agree,
    /// What was typed into the sheet is the answer, agreeing with it.
    Entry,
    /// A password, for the identity the question asked it for.
    Password { identity: usize },
    /// The secrets typed into the sheet, and whether to keep them.
    Secrets { remember: Option<bool> },
    /// The sheet asks nothing this can answer: a code on show.
    Nothing,
    /// Taking back the allowance the person just gave.
    Revoke {
        app_id: String,
        app_label: String,
        capability: String,
    },
}

/// One row of a global-shortcut question.
struct Binding {
    id: String,
    /// The chord the app proposed.
    proposed: Option<String>,
    /// The chord the row is bound to now.
    chord: Option<String>,
    /// The person bound or cleared it.
    changed: bool,
}

/// An allowance a question leaves behind, which the sheet shows at once so the person can take it
/// back.
#[derive(Clone)]
struct Grant {
    app_id: String,
    app_label: String,
    capability: String,
}

/// The question that is up, as the person left it.
struct Shown {
    /// The question it answers; `None` for what the sheet says after answering one, which asks
    /// nothing.
    handle: Option<String>,
    /// The details it was shown from: the service may change them.
    details: Value,
    sheet: Sheet,
    dialog: Dialog,
    plan: Plan,
    /// The person asked for their answer to be remembered.
    remember: bool,
    /// The allowance the question leaves, when it may leave one.
    grant: Option<Grant>,
}

/// The services' questions on the sheets.
pub(crate) struct Asker {
    questions: Option<Questions>,
    portal: Option<PortalSeat>,
    picker: Option<FilePickerSeat>,
    /// What the session publishes, to take an allowance back through the portal's own action.
    services: Rc<RefCell<Services>>,
    shown: RefCell<Option<Shown>>,
}

impl Asker {
    /// An asker showing its questions on `portal` and `picker` , taking allowances back through
    /// `services` .
    pub(crate) const fn new(
        questions: Option<Questions>,
        portal: Option<PortalSeat>,
        picker: Option<FilePickerSeat>,
        services: Rc<RefCell<Services>>,
    ) -> Self {
        Self {
            questions,
            portal,
            picker,
            services,
            shown: RefCell::new(None),
        }
    }

    /// Take what the person decided since the last pass, and put up the oldest question nothing
    /// shows yet. The sheet draws on the display the session put it on.
    pub(crate) fn follow(&self) {
        let Some(questions) = &self.questions else {
            return;
        };
        self.settle(questions);
        self.retire(questions);
        // The sheet saying what was granted makes way for a question that arrived meanwhile.
        if self.shown.borrow().is_some() && !questions.pending().is_empty() {
            let standing = self
                .shown
                .borrow()
                .as_ref()
                .is_some_and(|shown| shown.handle.is_none());
            if standing {
                self.dismiss();
            }
        }
        if self.shown.borrow().is_some() {
            return;
        }
        let showing = questions.pending().into_iter().find_map(|question| {
            let kind = question.kind.clone();
            self.show(question).then_some(kind)
        });
        if let Some(kind) = showing {
            tracing::debug!(kind, "a question is up on its sheet");
        }
    }

    /// Answer with what the person did on the sheet that is up, if they decided anything.
    fn settle(&self, questions: &Questions) {
        if self.shown.borrow().is_none() {
            return;
        }
        let mut decision = None;
        let mut redraw = false;
        {
            let mut held = self.shown.borrow_mut();
            let Some(shown) = held.as_mut() else {
                return;
            };
            match shown.sheet {
                Sheet::Files => {
                    if let Some(answer) = self.picker.as_ref().and_then(FilePickerSeat::take_answer)
                    {
                        decision = Some(match answer {
                            FileAnswer::Chosen(paths) => Decision::Answer(Answer {
                                uris: paths
                                    .iter()
                                    .filter_map(|path| tilo_portal::file_chooser::file_uri(path))
                                    .collect(),
                                ..Answer::default()
                            }),
                            FileAnswer::Cancelled => Decision::Decline,
                        });
                    }
                }
                Sheet::Portal => {
                    let Some(portal) = &self.portal else {
                        return;
                    };
                    for effect in portal.take_asked() {
                        // An answer asked of another question than the one up answers nothing: it
                        // expired with its request. What the sheet says once a question is answered
                        // names itself, and its answers name that.
                        if effect.handle() != shown.dialog.handle {
                            tracing::debug!(
                                request = effect.handle(),
                                "an answer for a question that is no longer up"
                            );
                            continue;
                        }
                        match shown.apply(&effect, portal) {
                            Step::Nothing => {}
                            Step::Redraw => redraw = true,
                            Step::Refused(why) => {
                                shown.dialog.error = Some(why);
                                redraw = true;
                            }
                            Step::Decide(done) => {
                                decision = Some(done);
                                break;
                            }
                        }
                    }
                    if decision.is_none() && redraw {
                        portal.show(Some(shown.dialog.clone()));
                    }
                }
            }
        }
        let Some(decision) = decision else {
            return;
        };
        let Some(shown) = self.shown.borrow_mut().take() else {
            return;
        };
        match (&shown.plan, decision) {
            (Plan::Revoke { .. }, Decision::Answer(_)) => {
                if let Plan::Revoke {
                    app_id,
                    app_label,
                    capability,
                } = &shown.plan
                {
                    self.revoke(app_id, app_label, capability);
                }
                self.take_down(shown.sheet);
            }
            (_, _) if shown.handle.is_none() => self.take_down(shown.sheet),
            (_, Decision::Answer(answer)) => {
                let grant = shown
                    .grant
                    .clone()
                    .filter(|_| keeps_answer(&shown, &answer));
                if let Some(handle) = &shown.handle {
                    questions.answer(handle, answer);
                }
                match grant {
                    Some(grant) => self.show_grant(shown.sheet, grant),
                    None => self.take_down(shown.sheet),
                }
            }
            (_, Decision::Decline) => {
                if let Some(handle) = &shown.handle {
                    questions.decline(handle);
                }
                self.take_down(shown.sheet);
            }
        }
    }

    /// Put up `grant` as the sheet's own word on what the person just allowed, with the press that
    /// takes it back.
    fn show_grant(&self, sheet: Sheet, grant: Grant) {
        let Sheet::Portal = sheet else {
            self.take_down(sheet);
            return;
        };
        let Some(portal) = &self.portal else {
            return;
        };
        let dialog = Dialog {
            handle: format!("portal-grant:{}:{}", grant.app_id, grant.capability),
            title: word("portal-allowed-title"),
            heading: grant.app_label.clone(),
            detail: words(
                "portal-allowed-detail",
                &[
                    ("app", &grant.app_label),
                    ("what", &what(&grant.capability)),
                ],
            ),
            confirm_label: word("portal-revoke"),
            cancel_label: word("portal-keep"),
            ..Dialog::default()
        };
        let plan = Plan::Revoke {
            app_id: grant.app_id,
            app_label: grant.app_label,
            capability: grant.capability,
        };
        portal.show(Some(dialog.clone()));
        self.shown.replace(Some(Shown {
            handle: None,
            details: Value::Null,
            sheet: Sheet::Portal,
            dialog,
            plan,
            remember: false,
            grant: None,
        }));
    }

    /// Take the allowance `app_label` gave `capability` back, through the action the portal
    /// publishes for it.
    fn revoke(&self, app_id: &str, app_label: &str, capability: &str) {
        fn standing<'a>(
            resources: &'a [ResourceSpec],
            app_id: &str,
            app_label: &str,
            capability: &str,
        ) -> Option<&'a ResourceSpec> {
            resources.iter().find_map(|resource| {
                let allowed = resource.kind == "portal.consent"
                    && (resource.label == app_id || resource.label == app_label)
                    && resource
                        .presentation
                        .as_ref()
                        .and_then(|presentation| presentation.detail.as_deref())
                        == Some(capability);
                allowed
                    .then_some(resource)
                    .or_else(|| standing(&resource.children, app_id, app_label, capability))
            })
        }
        let published = self.services.borrow().resources();
        let Some((resource, action)) = standing(&published, app_id, app_label, capability)
            .and_then(|resource| {
                resource
                    .actions
                    .first()
                    .map(|action| (resource.id.clone(), action.id.clone()))
            })
        else {
            tracing::warn!(app_id, capability, "no allowance to take back");
            return;
        };
        match self
            .services
            .borrow_mut()
            .invoke(resource.as_str(), &action, &serde_json::Map::new())
        {
            Some(Ok(_)) => tracing::debug!(app_id, capability, "allowance taken back"),
            Some(Err(error)) => tracing::warn!(%error, "the allowance was not taken back"),
            None => tracing::warn!(app_id, "the portal did not answer"),
        }
    }

    /// Take the sheet down when its question is gone from under it, or show it again when its
    /// service changed what it asks.
    fn retire(&self, questions: &Questions) {
        let current = self.shown.borrow().as_ref().map(Shown::state);
        let Some((handle, sheet, details)) = current else {
            return;
        };
        let Some(handle) = handle else {
            return;
        };
        let Some(question) = questions.question(&handle) else {
            self.shown.borrow_mut().take();
            self.take_down(sheet);
            return;
        };
        if question.details != details {
            self.shown.borrow_mut().take();
            self.show(question);
        }
    }

    /// The sheet goes away alone.
    fn dismiss(&self) {
        let Some(shown) = self.shown.borrow_mut().take() else {
            return;
        };
        self.take_down(shown.sheet);
    }

    /// The sheet is done with what it showed.
    fn take_down(&self, sheet: Sheet) {
        match sheet {
            Sheet::Portal => {
                if let Some(portal) = &self.portal {
                    portal.taken_down();
                }
            }
            Sheet::Files => {
                if let Some(picker) = &self.picker {
                    picker.taken_down();
                }
            }
        }
    }

    /// Show `question` , unless nothing shows its kind: whether it is up.
    fn show(&self, question: Question) -> bool {
        if question.kind == tilo_portal::file_chooser::FILE_CHOOSER_QUESTION {
            let Ok(asked) = serde_json::from_value::<FileChooserQuestion>(question.details.clone())
            else {
                return false;
            };
            let Some(picker) = &self.picker else {
                return false;
            };
            let request = request(&question, &asked);
            self.shown.replace(Some(Shown {
                details: question.details,
                handle: Some(question.handle),
                sheet: Sheet::Files,
                dialog: Dialog::default(),
                plan: Plan::Confirm,
                remember: false,
                grant: None,
            }));
            picker.ask(Some(request));
            picker.presentation().open(false);
            return true;
        }
        let Some(composed) = compose(&question) else {
            tracing::debug!(kind = question.kind, "no sheet shows this question");
            return false;
        };
        let Some(portal) = &self.portal else {
            return false;
        };
        self.shown.replace(Some(Shown {
            details: question.details,
            handle: Some(question.handle),
            sheet: Sheet::Portal,
            dialog: composed.dialog.clone(),
            plan: composed.plan,
            remember: false,
            grant: composed.grant,
        }));
        portal.show(Some(composed.dialog));
        portal.presentation().open(false);
        true
    }
}

impl Shown {
    /// What it holds, for the question that is now pending.
    fn state(&self) -> (Option<String>, Sheet, Value) {
        (self.handle.clone(), self.sheet, self.details.clone())
    }

    /// Take `effect` into the sheet, and what came of it.
    fn apply(&mut self, effect: &PortalEffect, seat: &PortalSeat) -> Step {
        match effect {
            PortalEffect::Select { index, .. } | PortalEffect::Icon { index, .. } => {
                self.highlight(*index);
                Step::Redraw
            }
            PortalEffect::Toggle { index, .. } => {
                self.toggle(*index);
                Step::Redraw
            }
            PortalEffect::Remember { .. } => {
                self.remember = !self.remember;
                if let Body::Answers {
                    remember: Some(row),
                    ..
                } = &mut self.dialog.body
                {
                    row.on = self.remember;
                }
                Step::Redraw
            }
            PortalEffect::Bind { keys, .. } => self.rebind(Some(keys)),
            PortalEffect::Unbind { .. } => self.rebind(None),
            // Keys and typing are the sheet's own: what is typed stays in its buffers until the
            // person confirms.
            PortalEffect::Next { .. }
            | PortalEffect::Back { .. }
            | PortalEffect::Type { .. }
            | PortalEffect::TypeSecret { .. } => Step::Nothing,
            // Confirming answers with the highlight as it stands; an answer names its own.
            PortalEffect::Confirm { .. } => self.decide(seat, None),
            PortalEffect::Answer { index, .. } => self.decide(seat, Some(*index)),
            PortalEffect::Submit { .. } => match self.plan {
                Plan::Password { identity } => {
                    let Some(answer) = seat.take_answer() else {
                        return Step::Nothing;
                    };
                    Step::Decide(Decision::Answer(Answer {
                        choice: Some(answer.choice.unwrap_or(identity)),
                        values: answer.values,
                        ..Answer::default()
                    }))
                }
                Plan::Secrets { remember } => {
                    let Some(answer) = seat.take_answer() else {
                        return Step::Nothing;
                    };
                    let remembered =
                        remember.is_some_and(|default| seat.option().unwrap_or(default));
                    Step::Decide(Decision::Answer(Answer {
                        remember: remembered,
                        values: answer.values,
                        ..Answer::default()
                    }))
                }
                _ => Step::Nothing,
            },
            PortalEffect::Cancel { .. } => Step::Decide(Decision::Decline),
        }
    }

    /// The person confirmed, having picked `index` or the highlight.
    fn decide(&self, seat: &PortalSeat, index: Option<u32>) -> Step {
        // A sheet that asks nothing of the person has nothing to decide: confirming a code on show
        // leaves the question waiting until the other device has finished.
        if matches!(self.plan, Plan::Nothing) {
            return Step::Nothing;
        }
        let mut copy = self.dialog.clone();
        if let Some(index) = index {
            copy.selected = usize::try_from(index).unwrap_or(usize::MAX);
        }
        match self.confirm(&copy, seat) {
            Ok(answer) => Step::Decide(Decision::Answer(answer)),
            Err(why) => Step::Refused(why),
        }
    }

    fn highlight(&mut self, index: u32) {
        self.dialog.selected = usize::try_from(index).unwrap_or(usize::MAX);
        self.dialog.error = None;
    }

    /// Tick or untick the row `index` .
    fn toggle(&mut self, index: u32) {
        let index = usize::try_from(index).unwrap_or(usize::MAX);
        match &mut self.dialog.body {
            Body::Devices(devices) => {
                if let Some(device) = devices.get_mut(index) {
                    device.selected = !device.selected;
                }
            }
            other => tracing::debug!(?other, "a row to tick on a sheet without rows"),
        }
    }

    /// Bind the highlighted row to `keys` , or clear it.
    fn rebind(&mut self, keys: Option<&str>) -> Step {
        let Plan::Bind(bindings) = &mut self.plan else {
            return Step::Nothing;
        };
        let row = self.dialog.selected;
        let Some(binding) = bindings.get_mut(row) else {
            return Step::Nothing;
        };
        let readable = if let Some(keys) = keys {
            let Some(trigger) = Trigger::parse(keys) else {
                if let Body::Shortcuts(rows) = &mut self.dialog.body
                    && let Some(shortcut) = rows.get_mut(row)
                {
                    shortcut.status = Some(word("portal-shortcut-not-taken"));
                }
                return Step::Redraw;
            };
            Some(trigger.display())
        } else {
            None
        };
        binding.chord = keys.map(str::to_owned);
        binding.changed = true;
        if let Body::Shortcuts(rows) = &mut self.dialog.body
            && let Some(shortcut) = rows.get_mut(row)
        {
            shortcut.keys = readable;
            shortcut.status = None;
        }
        Step::Redraw
    }

    /// The person confirmed `dialog` : what the service is answered, or why it cannot be.
    fn confirm(&self, dialog: &Dialog, seat: &PortalSeat) -> Result<Answer, String> {
        match &self.plan {
            Plan::Choose(choices) => Ok(Answer {
                choice: choices.get(dialog.selected).copied(),
                ..Answer::default()
            }),
            Plan::Confirm | Plan::Password { .. } | Plan::Secrets { .. } | Plan::Revoke { .. } => {
                Ok(Answer::default())
            }
            // `decide` answers nothing for a sheet that asks nothing, so this never runs.
            Plan::Nothing => Ok(Answer::default()),
            Plan::Agree => Ok(Answer {
                choice: Some(0),
                ..Answer::default()
            }),
            Plan::Entry => {
                let typed = seat.text(Field::Answer);
                let entry = typed.trim();
                if entry.is_empty() {
                    return Err(word("portal-secret-incomplete"));
                }
                Ok(Answer {
                    choice: Some(0),
                    values: vec![zeroize::Zeroizing::new(entry.to_owned())],
                    ..Answer::default()
                })
            }
            Plan::Rows { names, remember } => {
                let Body::Devices(devices) = &dialog.body else {
                    return Err(word("portal-no-such-field"));
                };
                Ok(Answer {
                    uris: devices
                        .iter()
                        .zip(names)
                        .filter(|(device, _)| device.selected)
                        .map(|(_, name)| name.clone())
                        .collect(),
                    remember: *remember && self.remember,
                    ..Answer::default()
                })
            }
            Plan::Bind(bindings) => Ok(bindings_answer(bindings)),
            Plan::Launcher { name, icons } => {
                let mut values = Vec::new();
                if *name {
                    let typed = seat.text(Field::Name);
                    if !valid_name(&typed) {
                        return Err(word("portal-launcher-bad-name"));
                    }
                    values.push(typed);
                }
                Ok(Answer {
                    choice: (*icons > 0).then_some(dialog.selected),
                    values,
                    ..Answer::default()
                })
            }
        }
    }
}

/// Whether `name` may name a launcher.
fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.trim() == name
        && name.len() <= 255
        && !name.chars().any(char::is_control)
}

/// The bindings `bindings` answer with.
fn bindings_answer(bindings: &[Binding]) -> Answer {
    if !bindings.iter().any(|binding| binding.changed) {
        return Answer {
            choice: Some(BIND_AS_PROPOSED),
            ..Answer::default()
        };
    }
    Answer {
        uris: bindings
            .iter()
            .filter_map(|binding| {
                let chord = binding.chord.as_ref()?;
                Some(if binding.proposed.as_deref() == Some(chord.as_str()) {
                    binding.id.clone()
                } else {
                    format!("{}={chord}", binding.id)
                })
            })
            .collect(),
        ..Answer::default()
    }
}

/// A question's sheet: what it shows, what its answer means, and the allowance it leaves when it
/// may leave one.
struct Composed {
    dialog: Dialog,
    plan: Plan,
    grant: Option<Grant>,
}

/// The sheet's dialog for `question` , and what its answer means.
fn compose(question: &Question) -> Option<Composed> {
    let who = who(question);
    let details = question.details.clone();
    let grant = grant_of(question);
    let (dialog, plan) = match question.kind.as_str() {
        tilo_portal::consent::ACCESS_QUESTION => access(&question.handle, &who, details)?,
        tilo_portal::account::ACCOUNT_QUESTION => account(&question.handle, &who, details)?,
        tilo_portal::email::EMAIL_QUESTION => email(&question.handle, &who, details)?,
        tilo_portal::background::BACKGROUND_QUESTION => {
            background(&question.handle, &who, details)?
        }
        tilo_portal::wallpaper::WALLPAPER_QUESTION => wallpaper(&question.handle, &who, details)?,
        tilo_portal::print::PRINT_QUESTION => printing(&question.handle, &who, details)?,
        tilo_portal::screenshot::SCREENSHOT_QUESTION => {
            screenshot(&question.handle, &who, details)?
        }
        tilo_portal::usb::USB_QUESTION => usb(&question.handle, &who, details)?,
        tilo_portal::app_chooser::APP_CHOOSER_QUESTION => {
            app_chooser(&question.handle, &who, details)?
        }
        tilo_portal::dynamic_launcher::LAUNCHER_QUESTION => {
            launcher(&question.handle, &who, details)?
        }
        tilo_portal::global_shortcuts::SHORTCUTS_QUESTION => {
            shortcuts(&question.handle, &who, details)?
        }
        tilo_portal::screencast::SCREENCAST_QUESTION => {
            share(&question.handle, &who, details, false)?
        }
        tilo_portal::remote_desktop::REMOTE_DESKTOP_QUESTION => {
            share(&question.handle, &who, details, true)?
        }
        tilo_polkit_agent::questions::KIND => authentication(&question.handle, details)?,
        NETWORK_KIND => network_secrets(&question.handle, details)?,
        CANNOT_JOIN_KIND => cannot_join(&question.handle, details)?,
        tilo_bluetooth_agent::KIND => pairing(&question.handle, details)?,
        crate::agent_door::KIND => admitted(&question.handle, &who, details)?,
        crate::network::AGENT_QUESTION => agent_question(&question.handle, details)?,
        crate::run_questions::KIND => run_question(&question.handle, details)?,
        crate::network::DEVICE_UNLOCK => device_unlock(&question.handle, details)?,
        crate::turn_questions::KIND => turn_question(&question.handle, details)?,
        _ => return None,
    };
    Some(Composed {
        dialog,
        plan,
        grant,
    })
}

/// The allowance `question` leaves when the person may remember their answer: what the sheet shows
/// at once so it can be taken back.
fn grant_of(question: &Question) -> Option<Grant> {
    if question.kind != tilo_portal::consent::ACCESS_QUESTION {
        return None;
    }
    let asked: AccessQuestion = serde_json::from_value(question.details.clone()).ok()?;
    asked.may_remember.then(|| Grant {
        app_id: question.app_id.clone().unwrap_or_default(),
        app_label: who(question),
        capability: asked.capability.clone(),
    })
}

/// Whether `answer` leaves an allowance that `shown` may leave.
fn keeps_answer(shown: &Shown, answer: &Answer) -> bool {
    shown.grant.is_some()
        && (answer.remember
            || shown.remember
            || matches!(
                &shown.plan,
                Plan::Choose(choices) if choices.get(shown.dialog.selected) == Some(&ALLOW_ALWAYS)
            ))
}

/// Who asks, as the person reads it: the app's own name where this device knows the app, else the
/// id it claims.
fn who(question: &Question) -> String {
    match &question.app_id {
        Some(app_id) => claimant(app_id),
        None => question.label.clone(),
    }
}

/// A sheet that asks the person to allow `detail` , and nothing else.
fn review(handle: &str, title: String, who: String, detail: String) -> Dialog {
    Dialog {
        handle: handle.to_owned(),
        title,
        heading: who,
        detail,
        body: Body::Review,
        confirm_label: word("portal-allow"),
        cancel_label: word("portal-deny"),
        ..Dialog::default()
    }
}

/// The access question: what `capability` asks for, in the person's words.
fn access(handle: &str, who: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: AccessQuestion = serde_json::from_value(details).ok()?;
    let what = what(&asked.capability);
    let (title, heading, detail) = if let Some(text) = &asked.text {
        (text.title.clone(), text.subtitle.clone(), text.body.clone())
    } else {
        let detail = asked
            .reason
            .clone()
            .unwrap_or_else(|| word("portal-access-detail"));
        (
            words("portal-access-title", &[("app", who), ("what", &what)]),
            who.to_owned(),
            detail,
        )
    };
    let mut allowances = vec![ALLOW_ONCE, ALLOW_SESSION];
    let mut labels = vec![word("portal-allow-once"), word("portal-allow-session")];
    if asked.may_remember {
        allowances.push(ALLOW_ALWAYS);
        labels.push(word("portal-allow-always"));
    }
    let mut dialog = review(handle, title, heading, detail);
    dialog.body = Body::Choices(labels);
    Some((dialog, Plan::Choose(allowances)))
}

/// The agent door's question: a program of this login wants to work in the workspace in front as an
/// agent. Answering admits it — as an agent member of that workspace, or for this connection only —
/// and refusing leaves it with no access at all.
fn admitted(handle: &str, who: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: crate::agent_door::Admission = serde_json::from_value(details).ok()?;
    if asked.as_person {
        // Acting as the person is theirs to allow for now or for this session; it is never
        // remembered beyond it, unless they name the program when they start the session.
        let title = words(
            "agent-door-admission-person",
            &[("workspace", &asked.workspace)],
        );
        let mut dialog = review(
            handle,
            title,
            who.to_owned(),
            word("agent-door-admission-person-detail"),
        );
        dialog.body = Body::Choices(vec![
            word("portal-allow-once"),
            word("portal-allow-session"),
        ]);
        return Some((dialog, Plan::Choose(vec![ALLOW_ONCE, ALLOW_SESSION])));
    }
    let title = words(
        "agent-door-admission",
        &[("workspace", &asked.workspace), ("agent", &asked.agent)],
    );
    let detail = words("agent-door-admission-detail", &[("agent", &asked.agent)]);
    let mut dialog = review(handle, title, who.to_owned(), detail);
    dialog.body = Body::Choices(vec![
        word("portal-allow-once"),
        word("portal-allow-session"),
        word("portal-allow-always"),
    ]);
    Some((
        dialog,
        Plan::Choose(vec![ALLOW_ONCE, ALLOW_SESSION, ALLOW_ALWAYS]),
    ))
}

/// An agent's question on a workspace host, on the sheet a page shows it on too.
fn agent_question(handle: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: tilo_sheets::portal::AgentQuestion = serde_json::from_value(details).ok()?;
    Some((asked.dialog(handle), Plan::Confirm))
}

/// What an agent's run asks before a step its grants leave to the person, or an app before it takes
/// another app's action: the step, what it would do, and whether to allow it once, allow the agent
/// or the app every such step, or deny it ([`crate::run_questions`]).
fn run_question(handle: &str, details: Value) -> Option<(Dialog, Plan)> {
    use crate::run_questions::{ALLOW_AGENT, ALLOW_ONCE, RunQuestion};
    let asked: RunQuestion = serde_json::from_value(details).ok()?;
    // An app asking to take another app's action is named as the person knows both apps; a
    // label that asks for more ("File a note…") is the step itself here, its arguments below it.
    let (title, detail, always) = match &asked.target {
        Some(target) => (
            words(
                "app-question-title",
                &[
                    ("app", &crate::app_names::app_name(&asked.agent)),
                    ("target", &crate::app_names::app_name(target)),
                ],
            ),
            asked.action.trim_end_matches('…').to_owned(),
            word("portal-allow-always"),
        ),
        None => (
            words(
                "run-question-title",
                &[("agent", &asked.agent), ("action", &asked.action)],
            ),
            words("run-question-detail", &[("resource", &asked.resource)]),
            words("run-question-allow-agent", &[("agent", &asked.agent)]),
        ),
    };
    let mut dialog = review(handle, title, String::new(), detail);
    dialog.facts.clone_from(&asked.fields);
    dialog.caution = (!matches!(asked.risk.as_str(), "" | "none" | "low")).then(|| {
        words(
            "network-agent-question-risk",
            &[("risk", &tilo_sheets::portal::risk_words(&asked.risk))],
        )
    });
    // Three answers in one row: Deny (Escape too), the standing grant, and once, the default.
    dialog.confirm_label = word("portal-allow-once");
    dialog.alternate_label = asked.may_allow_agent.then_some(always);
    Some((dialog, Plan::Choose(vec![ALLOW_ONCE, ALLOW_AGENT])))
}

/// Another of the person's devices asks for its disk-unlock half: the code on its boot screen is
/// shown beside the one the person answers with.
fn device_unlock(handle: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: crate::network::UnlockQuestion = serde_json::from_value(details).ok()?;
    let mut dialog = review(
        handle,
        words("network-device-unlock-title", &[("device", &asked.device)]),
        asked.device.clone(),
        words("network-device-unlock-detail", &[("slot", &asked.slot)]),
    );
    dialog.code = Some(asked.approval_code);
    dialog.confirm_label = word("network-device-unlock-allow");
    Some((dialog, Plan::Confirm))
}

/// `label` as a person reads it, or empty when it is an internal address
/// rather than a name: a control path (`i6ll2n4j~…~action-0`), or an app's
/// own resource id (`tl:app:root-…`, `desktop:tab:7`, `tilo.control:…`). A
/// step's sheet names what it acts on by the resource's published title
/// ([`crate::questions`]); a title the broker could not resolve yet is
/// dropped rather than shown as a path.
fn readable_label(label: &str) -> String {
    let label = label.trim();
    let address = label.contains('~')
        || label.contains('#')
        || label.contains("%7E")
        || label.starts_with("tl:")
        || label.starts_with("desktop:")
        || label.starts_with("app.")
        || label.starts_with("tilo.control:")
        || label.starts_with("session:")
        || label.starts_with("chat.");
    if address {
        return String::new();
    }
    label.to_owned()
}

/// An agent's step inside a conversation turn, asking the person before it
/// runs: the draft they review, row by row, and the one confirmation that
/// answers it (`docs/designs/app-language/shell.md` §1, "Trust stays
/// system-drawn"). The conversation points at this sheet; it never draws the
/// question.
fn turn_question(handle: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: crate::turn_questions::TurnDetails = serde_json::from_value(details).ok()?;
    // What the step acts on, as the person reads it: the resource's published
    // title, never the internal id a control path or a raw address carries.
    // A label that is still an address is dropped rather than shown: nothing
    // stands as the sheet's heading, and the step is named by no place.
    let resource = readable_label(&asked.resource_label);
    let mut rows = Vec::new();
    for field in &asked.draft.fields {
        // A value the sheet shows but does not edit (a list, a resource, a
        // withheld secret) is one the person confirms as it is.
        let value = match &field.value {
            tilo_actions::draft::DraftValue::Text { text }
            | tilo_actions::draft::DraftValue::Number { text } => DraftValue::Text {
                text: field.shows.clone().unwrap_or_else(|| text.clone()),
                multiline: field.multiline,
                number: matches!(field.value, tilo_actions::draft::DraftValue::Number { .. }),
                withheld: false,
            },
            tilo_actions::draft::DraftValue::Flag { on } => DraftValue::Flag(*on),
            tilo_actions::draft::DraftValue::List { .. }
            | tilo_actions::draft::DraftValue::Resource { .. }
            | tilo_actions::draft::DraftValue::Withheld => DraftValue::Text {
                text: field.shows.clone().unwrap_or_else(|| field.value.line()),
                multiline: false,
                number: false,
                withheld: matches!(field.value, tilo_actions::draft::DraftValue::Withheld),
            },
        };
        rows.push(DraftRow {
            index: rows.len(),
            label: field.label.clone(),
            value,
            editable: field.editable,
        });
    }
    let mut dialog = review(
        handle,
        words("turn-question-title", &[("action", &asked.action_label)]),
        resource.clone(),
        if resource.is_empty() {
            words(
                "turn-question-detail-here",
                &[("action", &asked.action_label)],
            )
        } else {
            words(
                "turn-question-detail",
                &[("action", &asked.action_label), ("resource", &resource)],
            )
        },
    );
    dialog.confirm_label = word("portal-allow-once");
    dialog.body = Body::Draft {
        rows,
        choices: Vec::new(),
    };
    Some((dialog, Plan::Confirm))
}

/// What `capability` is, in the person's words.
fn what(capability: &str) -> String {
    match access_key(capability) {
        "generic" => words("portal-access-generic", &[("capability", capability)]),
        key => word(&format!("portal-access-{key}")),
    }
}

/// The capability the sheet has words for.
fn access_key(capability: &str) -> &str {
    match capability {
        "screenshot" | "screencast" | "remote-desktop" | "camera" | "microphone" | "speaker"
        | "location" | "print" | "usb" | "input-capture" | "global-shortcuts" | "notification"
        | "secret" | "host-shell" => capability,
        _ => "generic",
    }
}

fn account(handle: &str, who: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: AccountQuestion = serde_json::from_value(details).ok()?;
    let detail = asked
        .reason
        .unwrap_or_else(|| words("portal-account-detail", &[("app", who)]));
    let mut dialog = review(
        handle,
        words("portal-account-title", &[("app", who)]),
        who.to_owned(),
        detail,
    );
    dialog.confirm_label = word("portal-account-share");
    Some((dialog, Plan::Confirm))
}

fn email(handle: &str, who: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: EmailQuestion = serde_json::from_value(details).ok()?;
    let mut lines = Vec::new();
    let mut list = |key: &str, addresses: &[String]| {
        if !addresses.is_empty() {
            lines.push(words(key, &[("addresses", &addresses.join(", "))]));
        }
    };
    list("portal-email-to", &asked.addresses);
    list("portal-email-cc", &asked.cc);
    list("portal-email-bcc", &asked.bcc);
    if let Some(subject) = &asked.subject {
        lines.push(words("portal-email-subject", &[("subject", subject)]));
    }
    if !asked.attachments.is_empty() {
        lines.push(words(
            "portal-email-attachments",
            &[("count", &asked.attachments.len().to_string())],
        ));
    }
    lines.insert(0, words("portal-email-detail", &[("app", who)]));
    let mut dialog = review(
        handle,
        word("portal-email-title"),
        who.to_owned(),
        lines.join("\n"),
    );
    dialog.confirm_label = word("portal-email-open");
    Some((dialog, Plan::Confirm))
}

fn background(handle: &str, who: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: BackgroundQuestion = serde_json::from_value(details).ok()?;
    let mut dialog = review(
        handle,
        words("portal-background-title", &[("app", who)]),
        who.to_owned(),
        asked.name,
    );
    dialog.body = Body::Choices(vec![
        word("portal-background-background"),
        word("portal-background-running"),
        word("portal-background-active"),
    ]);
    dialog.selected = 1;
    let rows = [BACKGROUND, RUNNING, ACTIVE].map(|result| result as usize);
    Some((dialog, Plan::Choose(rows.to_vec())))
}

fn wallpaper(handle: &str, who: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: WallpaperQuestion = serde_json::from_value(details).ok()?;
    let where_ = word(match asked.set_on.as_str() {
        "lockscreen" => "portal-wallpaper-lockscreen",
        "both" => "portal-wallpaper-both",
        _ => "portal-wallpaper-background",
    });
    let mut dialog = review(
        handle,
        words("portal-wallpaper-title", &[("app", who)]),
        who.to_owned(),
        words(
            "portal-wallpaper-detail",
            &[("app", who), ("where", &where_)],
        ),
    );
    dialog.confirm_label = word("portal-wallpaper-set");
    Some((dialog, Plan::Confirm))
}

fn printing(handle: &str, who: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: PrintQuestion = serde_json::from_value(details).ok()?;
    let copies = asked.copies.to_string();
    let mut dialog = review(
        handle,
        words("portal-print-title", &[("title", &asked.title)]),
        who.to_owned(),
        words("portal-print-detail", &[("copies", &copies)]),
    );
    dialog.selected = asked.selected.min(asked.printers.len().saturating_sub(1));
    dialog.body = Body::Choices(asked.printers.clone());
    dialog.confirm_label = word("portal-print-print");
    let printers = (0..asked.printers.len()).collect();
    Some((dialog, Plan::Choose(printers)))
}

fn screenshot(handle: &str, who: &str, details: Value) -> Option<(Dialog, Plan)> {
    let _asked: ScreenshotQuestion = serde_json::from_value(details).ok()?;
    let mut dialog = review(
        handle,
        words("portal-screenshot-title", &[("app", who)]),
        who.to_owned(),
        word("portal-screenshot-detail"),
    );
    dialog.confirm_label = word("portal-screenshot-take");
    Some((dialog, Plan::Confirm))
}

fn usb(handle: &str, who: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: UsbQuestion = serde_json::from_value(details).ok()?;
    let mut dialog = review(
        handle,
        words("portal-usb-title", &[("app", who)]),
        who.to_owned(),
        words("portal-usb-detail", &[("app", who)]),
    );
    dialog.body = Body::Devices(
        asked
            .devices
            .iter()
            .map(|device| Device {
                label: device.label.clone().unwrap_or_else(|| device.id.clone()),
                writable: device.writable,
                selected: true,
            })
            .collect(),
    );
    dialog.confirm_label = word("portal-usb-allow");
    Some((
        dialog,
        Plan::Rows {
            names: asked
                .devices
                .iter()
                .map(|device| device.id.clone())
                .collect(),
            remember: false,
        },
    ))
}

fn app_chooser(handle: &str, who: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: AppChooserQuestion = serde_json::from_value(details).ok()?;
    let what = asked
        .filename
        .clone()
        .or_else(|| asked.uri.clone())
        .unwrap_or_default();
    let mut dialog = review(
        handle,
        word("portal-app-choice-title"),
        String::new(),
        if what.is_empty() {
            word("portal-app-choice-detail-any")
        } else {
            words("portal-app-choice-detail", &[("app", who), ("what", &what)])
        },
    );
    let at = asked
        .last_choice
        .as_ref()
        .and_then(|last| asked.choices.iter().position(|app| &app.id == last));
    dialog.body = Body::Choices(asked.choices.iter().map(|app| app.name.clone()).collect());
    dialog.selected = at.unwrap_or(0);
    dialog.confirm_label = word("portal-app-choice-open");
    Some((dialog, Plan::Choose((0..asked.choices.len()).collect())))
}

fn launcher(handle: &str, who: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: LauncherQuestion = serde_json::from_value(details).ok()?;
    let icons = if asked.editable_icon {
        vec![
            word("portal-launcher-app-icon"),
            word("portal-launcher-monogram-icon"),
            word("portal-launcher-web-icon"),
        ]
    } else {
        Vec::new()
    };
    let mut dialog = review(
        handle,
        words("portal-launcher-title", &[("name", &asked.name)]),
        who.to_owned(),
        words("portal-launcher-detail", &[("app", who)]),
    );
    dialog.body = Body::Launcher {
        name: asked.editable_name.then(|| asked.name.clone()),
        icons: icons.clone(),
        icon_editable: asked.editable_icon,
    };
    dialog.confirm_label = word("portal-launcher-add");
    Some((
        dialog,
        Plan::Launcher {
            name: asked.editable_name,
            icons: icons.len(),
        },
    ))
}

fn shortcuts(handle: &str, who: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: ShortcutsQuestion = serde_json::from_value(details).ok()?;
    let mut bindings = Vec::new();
    let mut rows = Vec::new();
    for row in &asked.shortcuts {
        let status = match (row.status.as_deref(), &row.taken_by) {
            (Some("reserved"), _) => Some(word("portal-shortcut-reserved")),
            (Some("taken"), Some(app)) => {
                Some(words("portal-shortcut-taken", &[("app", &claimant(app))]))
            }
            _ => None,
        };
        bindings.push(Binding {
            id: row.id.clone(),
            proposed: row.trigger.clone(),
            chord: row.trigger.clone(),
            changed: false,
        });
        rows.push(Shortcut {
            name: row.description.clone().unwrap_or_else(|| row.id.clone()),
            keys: row.trigger.as_deref().and_then(reads),
            status,
        });
    }
    let mut dialog = review(
        handle,
        word("portal-shortcuts-title"),
        who.to_owned(),
        word("portal-shortcuts-detail"),
    );
    dialog.body = Body::Shortcuts(rows);
    dialog.confirm_label = word("portal-shortcuts-save");
    Some((dialog, Plan::Bind(bindings)))
}

/// How a chord in the portal's spelling reads.
fn reads(chord: &str) -> Option<String> {
    Trigger::parse(chord).map(|trigger| trigger.display())
}

fn share(handle: &str, who: &str, details: Value, remote: bool) -> Option<(Dialog, Plan)> {
    let asked: ShareQuestion = serde_json::from_value(details).ok()?;
    let mut detail = vec![words(
        if remote {
            "portal-remote-detail"
        } else {
            "portal-share-detail"
        },
        &[("app", who)],
    )];
    if !asked.devices.is_empty() {
        detail.push(words(
            "portal-remote-devices",
            &[("devices", &asked.devices.join(", "))],
        ));
    }
    let mut dialog = review(
        handle,
        words(
            if remote {
                "portal-remote-title"
            } else {
                "portal-share-title"
            },
            &[("app", who)],
        ),
        who.to_owned(),
        detail.join("\n"),
    );
    dialog.confirm_label = word("portal-share-share");
    let names = asked
        .sources
        .iter()
        .map(|source| source.choice.clone())
        .collect();
    if asked.sources.is_empty() {
        return Some((
            dialog,
            Plan::Rows {
                names,
                remember: asked.may_remember,
            },
        ));
    }
    dialog.body = if asked.multiple {
        Body::Devices(
            asked
                .sources
                .iter()
                .map(|source| Device {
                    label: source_label(source),
                    writable: false,
                    selected: false,
                })
                .collect(),
        )
    } else {
        Body::Answers {
            choices: asked.sources.iter().map(source_label).collect(),
            suggested: 0,
            remember: asked.may_remember.then(|| Remember {
                label: word("portal-share-remember"),
                on: false,
            }),
        }
    };
    Some((
        dialog,
        Plan::Rows {
            names,
            remember: asked.may_remember,
        },
    ))
}

/// How the source the app offers reads.
fn source_label(source: &OfferedSource) -> String {
    match (&source.app_id, source.kind) {
        (Some(app), SourceKind::Window) => format!("{}: {}", claimant(app), source.label),
        _ => source.label.clone(),
    }
}

fn authentication(handle: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: AuthenticateDetails = serde_json::from_value(details).ok()?;
    let notice = asked.notice.as_ref().map(|notice| match notice {
        Notice::Message { key } => word(key),
        Notice::Verbatim(text) => text.clone(),
    });
    let prompt = Prompt {
        icon: Some(ui::icons::Icon::Shield),
        resource: asked.action_id.clone(),
        fields: vec![PromptField {
            label: asked.prompt.clone(),
            masked: !asked.echo,
            required: false,
            initial: String::new(),
        }],
        choices: (asked.identities.len() > 1)
            .then(|| (word("portal-auth-identity"), asked.identities.clone())),
        primary_label: word("portal-auth-authenticate"),
        error: notice.map(|notice| (0, notice)),
        ..Prompt::default()
    };
    let dialog = Dialog {
        handle: handle.to_owned(),
        title: asked.message.clone(),
        heading: word("portal-auth-heading"),
        body: Body::Secret(prompt),
        confirm_label: word("portal-auth-authenticate"),
        cancel_label: word("portal-cancel"),
        selected: asked.identity,
        ..Dialog::default()
    };
    Some((
        dialog,
        Plan::Password {
            identity: asked.identity,
        },
    ))
}

/// What a service says, in the person's words.
fn say(text: &Words) -> String {
    match text {
        Words::Message { key, args } => {
            let resolved: Vec<(String, String)> = args
                .iter()
                .map(|(name, value)| (name.clone(), say(value)))
                .collect();
            let borrowed: Vec<(&str, &str)> = resolved
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            words(key, &borrowed)
        }
        Words::Verbatim(text) => text.clone(),
    }
}

/// The network secrets sheet: the fields the network asks for, typed into the sheet and answered
/// through the question, never through an effect.
fn network_secrets(handle: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: SecretsDetails = serde_json::from_value(details).ok()?;
    let retry = word("portal-network-retry");
    let (resource, retried) = what_the_network_is(&asked, &retry);
    let (kind, fields, primary_label, error) = what_it_asks(&asked, &retry, retried);
    let wifi = asked.fields.iter().any(|field| {
        field.rule == FieldRule::WpaPassphrase
            || field.key == "psk"
            || field.key.starts_with("wep-key")
    });
    let prompt = Prompt {
        kind,
        icon: Some(if wifi && asked.sim.is_none() {
            ui::icons::Icon::Wifi
        } else {
            ui::icons::Icon::Lock
        }),
        resource,
        fields,
        choices: None,
        option: if asked.sim.is_some() {
            None
        } else {
            asked
                .remember
                .map(|remember| (word("portal-remember-device"), remember))
        },
        primary_label,
        error,
        busy: None,
    };
    let dialog = Dialog {
        handle: handle.to_owned(),
        title: say(&asked.title),
        body: Body::Secret(prompt),
        confirm_label: word("portal-connect"),
        cancel_label: word("portal-cancel"),
        ..Dialog::default()
    };
    let plan = Plan::Secrets {
        remember: asked.remember,
    };
    Some((dialog, plan))
}

/// What the network is, in its own words, and whether it said the last secret was refused: that
/// reads as the reason the field is wrong, not as what the network is, so it is left out here.
fn what_the_network_is(asked: &SecretsDetails, retry: &str) -> (String, bool) {
    let mut retried = false;
    let said = asked
        .detail
        .iter()
        .map(say)
        .filter(|line| {
            retried |= line == retry;
            line != retry
        })
        .collect::<Vec<_>>()
        .join("\n");
    (said, retried)
}

/// The fields the network wants typed, what confirming it says, and why it refused the last answer.
fn what_it_asks(
    asked: &SecretsDetails,
    retry: &str,
    retried: bool,
) -> (
    PromptKind,
    Vec<PromptField>,
    String,
    Option<(usize, String)>,
) {
    let refused = asked
        .refusal
        .as_ref()
        .map(|refusal| (refusal.field, word(&refusal.key)));
    let Some(sim) = &asked.sim else {
        let fields = asked
            .fields
            .iter()
            .map(|field| PromptField {
                label: say(&field.label),
                masked: field.masked,
                required: field.required,
                initial: field.initial.clone(),
            })
            .collect();
        let error = refused.or_else(|| {
            retried.then(|| {
                let at = asked
                    .fields
                    .iter()
                    .position(|field| field.masked)
                    .unwrap_or(0);
                (at, retry.to_owned())
            })
        });
        return (PromptKind::Fields, fields, word("portal-connect"), error);
    };
    let (kind, fields) = match sim.step {
        SimUnlockStep::Pin => (
            PromptKind::SimPin {
                attempts_left: sim.attempts_left,
            },
            vec![PromptField {
                label: asked
                    .fields
                    .first()
                    .map(|field| say(&field.label))
                    .unwrap_or_default(),
                masked: true,
                required: true,
                initial: String::new(),
            }],
        ),
        // The question reads back the PUK and the new PIN; the sheet asks the PIN twice so a typo
        // does not lock the SIM again.
        SimUnlockStep::Puk => (
            PromptKind::SimPuk {
                attempts_left: sim.attempts_left,
            },
            vec![PromptField::default(); 2],
        ),
    };
    let error = refused.or_else(|| {
        (retried && sim.step == SimUnlockStep::Pin).then(|| (0, word("portal-sim-pin-wrong")))
    });
    (kind, fields, word("portal-sim-unlock"), error)
}

/// A network the person may join, which asks for its password next.
fn cannot_join(handle: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: CannotJoinDetails = serde_json::from_value(details).ok()?;
    let join = say(&asked.join);
    let dialog = Dialog {
        handle: handle.to_owned(),
        title: say(&asked.title),
        heading: asked.network.clone(),
        detail: say(&asked.body),
        body: Body::Choices(vec![join.clone()]),
        confirm_label: join,
        cancel_label: word("portal-cancel"),
        ..Dialog::default()
    };
    Some((dialog, Plan::Choose(vec![0])))
}

/// A device pairing, as the device's method asks it.
fn pairing(handle: &str, details: Value) -> Option<(Dialog, Plan)> {
    let asked: PairingDetails = serde_json::from_value(details).ok()?;
    let device = asked.device.clone();
    let service = asked
        .service_key
        .as_deref()
        .map(word)
        .or_else(|| asked.service.clone());
    let explain = |key: &str| words(key, &[("device", &device)]);
    let mut dialog = Dialog {
        handle: handle.to_owned(),
        heading: String::new(),
        cancel_label: word("bluetooth-prompt-reject"),
        ..Dialog::default()
    };
    let plan = match asked.method {
        PairingMethod::ConfirmPasskey => {
            dialog.title = words(
                "bluetooth-prompt-title-confirm-passkey",
                &[("device", &device)],
            );
            dialog.detail = explain("bluetooth-prompt-explain-confirm-passkey");
            dialog.code.clone_from(&asked.code);
            dialog.confirm_label = word("bluetooth-prompt-accept-match");
            Plan::Agree
        }
        PairingMethod::ConfirmPairing => {
            dialog.title = words(
                "bluetooth-prompt-title-confirm-pairing",
                &[("device", &device)],
            );
            dialog.detail = explain("bluetooth-prompt-explain-confirm-pairing");
            dialog.confirm_label = word("bluetooth-prompt-accept-pair");
            Plan::Agree
        }
        PairingMethod::AuthorizeService => {
            dialog.title = words(
                "bluetooth-prompt-title-authorize-service",
                &[("device", &device)],
            );
            let mut detail = explain("bluetooth-prompt-explain-authorize-service");
            if let Some(service) = &service {
                detail.push('\n');
                detail.push_str(&words("bluetooth-prompt-service", &[("service", service)]));
            }
            dialog.detail = detail;
            dialog.confirm_label = word("bluetooth-prompt-accept-allow");
            Plan::Agree
        }
        PairingMethod::EnterPasskey | PairingMethod::EnterPin => {
            let (title, masked) = if asked.method == PairingMethod::EnterPasskey {
                ("bluetooth-prompt-title-request-passkey", false)
            } else {
                ("bluetooth-prompt-title-request-pin", true)
            };
            dialog.title = words(title, &[("device", &device)]);
            dialog.detail = explain("bluetooth-prompt-explain-request");
            dialog.body = Body::Entry {
                masked,
                required: true,
                choices: Vec::new(),
            };
            dialog.confirm_label = word("bluetooth-prompt-accept-pair");
            Plan::Entry
        }
        PairingMethod::DisplayPasskey | PairingMethod::DisplayPin => {
            dialog.title = words("bluetooth-prompt-title-display", &[("device", &device)]);
            let mut detail = explain("bluetooth-prompt-explain-display");
            if let Some(entered) = asked.entered {
                detail.push('\n');
                detail.push_str(&words(
                    "bluetooth-prompt-digits-entered",
                    &[("entered", &entered.to_string())],
                ));
            }
            dialog.detail = detail;
            dialog.code.clone_from(&asked.code);
            // A code on show has nothing to agree to: the sheet says it and offers only the press
            // that stops the pairing.
            dialog.body = Body::Answers {
                choices: Vec::new(),
                suggested: 0,
                remember: None,
            };
            dialog.cancel_label = word("bluetooth-prompt-cancel");
            Plan::Nothing
        }
    };
    Some((dialog, plan))
}

/// The chooser's request for the portal's question `question` .
fn request(question: &Question, asked: &FileChooserQuestion) -> FileRequest {
    let home = tilo_paths::home();
    let start = home.clone().unwrap_or_else(|| PathBuf::from("/"));
    FileRequest {
        handle: question.handle.clone(),
        title: asked.title.clone(),
        app_name: question.app_id.as_deref().map(claimant),
        app_id: question.app_id.clone(),
        mode: if asked.directory {
            FileMode::Folder
        } else if asked.save {
            FileMode::Save
        } else {
            FileMode::Open
        },
        multiple: asked.multiple,
        accept_label: asked.accept_label.clone(),
        filters: asked
            .filters
            .iter()
            .map(|filter| FileType {
                label: filter.label.clone(),
                extensions: filter.extensions.clone(),
            })
            .collect(),
        initial_dir: start.clone(),
        roots: vec![start],
        home,
        suggested_name: asked.current_name.clone(),
        recent: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sheet `kind` composes from `details` .
    fn sheet(kind: &str, details: Value) -> Composed {
        compose(&Question {
            handle: "asked".into(),
            kind: kind.into(),
            app_id: None,
            label: "Notes".into(),
            details,
        })
        .expect("the session composes this question")
    }

    /// An app asking to take another app's action names both apps, the step and what it would
    /// run with, and answers on three buttons: deny, always and once; a step confirmed each time
    /// offers no "always".
    #[test]
    fn an_apps_change_of_another_app_names_both_apps() {
        let asked = |may_allow_agent| {
            sheet(
                crate::run_questions::KIND,
                serde_json::to_value(crate::run_questions::RunQuestion {
                    agent: "so.tilo.files".into(),
                    target: Some("so.tilo.notes".into()),
                    action: "Delete finished tasks…".into(),
                    resource: "Tasks".into(),
                    fields: vec![("Title".to_owned(), "Morning: 1 done".to_owned())],
                    risk: "low".into(),
                    may_allow_agent,
                })
                .unwrap(),
            )
        };
        let shown = asked(true);
        let files = crate::app_names::app_name("so.tilo.files");
        let notes = crate::app_names::app_name("so.tilo.notes");
        assert_eq!(
            shown.dialog.title,
            words("app-question-title", &[("app", &files), ("target", &notes)])
        );
        assert!(shown.dialog.heading.is_empty(), "the title names the app");
        assert_eq!(shown.dialog.detail, "Delete finished tasks");
        assert_eq!(
            shown.dialog.facts,
            vec![("Title".to_owned(), "Morning: 1 done".to_owned())]
        );
        assert!(
            matches!(shown.dialog.body, Body::Review),
            "no list beside the buttons"
        );
        assert_eq!(shown.dialog.confirm_label, word("portal-allow-once"));
        assert_eq!(shown.dialog.cancel_label, word("portal-deny"));
        assert_eq!(
            shown.dialog.alternate_label,
            Some(word("portal-allow-always"))
        );
        assert!(matches!(&shown.plan, Plan::Choose(choices) if choices.len() == 2));
        assert_eq!(asked(false).dialog.alternate_label, None);
    }

    fn agent_question() -> Composed {
        sheet(
            crate::network::AGENT_QUESTION,
            serde_json::to_value(tilo_sheets::portal::AgentQuestion {
                agent: "notes-agent".into(),
                action: "Delete three notes".into(),
                resource: "the recipes notebook".into(),
                risk: "destructive".into(),
                workspace: "Home".into(),
                ..tilo_sheets::portal::AgentQuestion::default()
            })
            .unwrap(),
        )
    }

    #[test]
    fn an_agents_question_shows_its_agent_action_and_risk() {
        let shown = agent_question();
        assert!(shown.dialog.title.contains("notes-agent"));
        assert!(shown.dialog.title.contains("delete three notes"));
        assert!(shown.dialog.detail.contains("the recipes notebook"));
        assert!(shown.dialog.detail.contains("Home"));
        assert!(!shown.dialog.detail.contains("network-agent"));
        assert!(shown.dialog.code.is_none());
        assert!(matches!(shown.plan, Plan::Confirm));
    }

    #[test]
    fn a_question_whose_risk_the_host_does_not_name_shows_no_risk() {
        let shown = sheet(
            crate::network::AGENT_QUESTION,
            serde_json::to_value(tilo_sheets::portal::AgentQuestion {
                agent: "notes-agent".into(),
                action: "Read a note".into(),
                resource: "the recipes notebook".into(),
                risk: String::new(),
                kind: "read".into(),
                workspace: "Home".into(),
                ..tilo_sheets::portal::AgentQuestion::default()
            })
            .unwrap(),
        );
        assert!(!shown.dialog.detail.contains("network-agent"));
        assert_eq!(shown.dialog.detail.lines().count(), 1);
    }

    #[test]
    fn a_boot_screens_code_is_shown_beside_the_one_the_person_compares() {
        let shown = sheet(
            crate::network::DEVICE_UNLOCK,
            serde_json::to_value(crate::network::UnlockQuestion {
                approval_code: "417 208".into(),
                device: "The phone".into(),
                slot: "abcd:0".into(),
            })
            .unwrap(),
        );
        assert_eq!(shown.dialog.code.as_deref(), Some("417 208"));
        assert!(shown.dialog.title.contains("The phone"));
        assert!(shown.dialog.detail.contains("abcd:0"));
        assert!(!shown.dialog.detail.contains("network-device"));
        assert!(matches!(shown.plan, Plan::Confirm));
    }

    /// A step an agent takes inside a conversation asks on the system's
    /// sheet: the draft the person reviews, row by row, and one confirm that
    /// answers it (`shell.md` §1, "Trust stays system-drawn"; §8.1).
    #[test]
    fn an_agents_step_in_a_conversation_asks_on_the_system_sheet() {
        let draft = tilo_actions::Draft {
            action_label: "Retitle".into(),
            resource_label: "Groceries".into(),
            fields: vec![tilo_actions::draft::DraftField {
                path: vec!["title".into()],
                label: "Title".into(),
                value: tilo_actions::draft::DraftValue::Text {
                    text: "Groceries".into(),
                },
                editable: true,
                multiline: false,
                shows: None,
                role: None,
            }],
            ..tilo_actions::Draft::default()
        };
        let shown = sheet(
            crate::turn_questions::KIND,
            serde_json::to_value(crate::turn_questions::TurnDetails {
                resource_label: "Groceries".into(),
                action_label: "Retitle".into(),
                draft,
            })
            .unwrap(),
        );
        // The words are the session's own catalogue's (`turn-question-*`);
        // a unit test has none loaded, so it reads the key standing for them,
        // and the data that fills them: what the step acts on, by its name.
        assert!(
            shown.dialog.title.contains("turn-question-title")
                || shown.dialog.title.contains("Retitle"),
            "{:?}",
            shown.dialog
        );
        assert!(
            shown.dialog.detail.contains("turn-question-detail")
                || shown.dialog.detail.contains("Groceries"),
            "{:?}",
            shown.dialog
        );
        assert_eq!(shown.dialog.heading, "Groceries", "{:?}", shown.dialog);
        assert!(matches!(shown.plan, Plan::Confirm));
        let Body::Draft { rows, choices } = &shown.dialog.body else {
            panic!("the sheet shows the step's draft: {:?}", shown.dialog.body);
        };
        assert_eq!(rows.len(), 1, "one row per field: {rows:?}");
        assert_eq!(rows[0].label, "Title");
        assert!(rows[0].editable, "the person may change it");
        assert!(choices.is_empty(), "one confirmation answers it");
    }

    /// A step whose resource label is a control path rather than a title — an
    /// editor's "Go to" on a sidebar row, as a live run showed — names what it
    /// acts on by a person's words, never the hundred-character id: the sheet
    /// must not print the address of a control.
    #[test]
    fn a_control_path_step_names_its_resource_by_title_not_by_its_id() {
        let draft = tilo_actions::Draft {
            action_label: "Go to".into(),
            resource_label: "Inbox".into(),
            fields: vec![tilo_actions::draft::DraftField {
                path: vec!["entry".into()],
                label: "Entry".into(),
                value: tilo_actions::draft::DraftValue::Text {
                    text: "is:inbox".into(),
                },
                editable: false,
                multiline: false,
                shows: None,
                role: None,
            }],
            ..tilo_actions::Draft::default()
        };
        let shown = sheet(
            crate::turn_questions::KIND,
            serde_json::to_value(crate::turn_questions::TurnDetails {
                // The title the broker resolved for the control's resource.
                resource_label: "Inbox".into(),
                action_label: "Go to".into(),
                draft: draft.clone(),
            })
            .unwrap(),
        );
        for text in [
            &shown.dialog.title,
            &shown.dialog.detail,
            &shown.dialog.heading,
        ] {
            assert!(
                !text.contains('~') && !text.contains("i6ll2n4j"),
                "the sheet shows an internal control path: {text:?}"
            );
        }
        assert_eq!(shown.dialog.heading, "Inbox", "{:?}", shown.dialog);

        // An unresolved label is an address: the sheet drops it and names the
        // step by what it does, rather than printing a path a person cannot
        // read.
        let path = "i6ll2n4j~i6ll2n4j.108~tj4z2obq.513~s7yqm7cb.for0/root-qhf2pyze/if2201f91-35~s7yqm7cb~row0~s7yqm7cb~row0.for0/root-qhf2pyze/if2201f91-35#select";
        let raw = sheet(
            crate::turn_questions::KIND,
            serde_json::to_value(crate::turn_questions::TurnDetails {
                resource_label: path.into(),
                action_label: "Go to".into(),
                draft,
            })
            .unwrap(),
        );
        for text in [&raw.dialog.title, &raw.dialog.detail, &raw.dialog.heading] {
            assert!(
                !text.contains('~') && !text.contains("i6ll2n4j"),
                "the sheet prints the control's own path: {text:?}"
            );
        }
        assert_eq!(
            raw.dialog.heading, "",
            "an address is no heading: {:?}",
            raw.dialog
        );
        assert!(
            raw.dialog.detail.contains("turn-question-detail-here")
                || raw.dialog.detail.contains("Go to"),
            "the step is still named by what it does: {:?}",
            raw.dialog
        );
    }
}

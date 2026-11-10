//! The review of the page in front: what its forms hold now, who set each
//! answer, and what was done in the tab, in order, for the person at this
//! window.
//!
//! The page is a full-bleed surface, so its review is a secondary screen
//! shown in the page's place and dismissed back to it, as the Library is
//! (HIG §1e). It opens through the page's `review-page` action, which the
//! Page menu, a key, the palette and an agent all invoke, and `show-page`
//! closes it. Choosing a row is the page's `show-page-field` action: the
//! page again, with that field in view.
//!
//! Nothing it shows is its own. It asks the engine for the page's forms, its
//! fields and the tab's record, the reads `read-page-forms`,
//! `read-page-fields` and `read-page-record` make, and draws what they
//! answered. It asks again when any page action answered, since the tab's
//! record may have moved; when the page shows another document; and when the
//! engine says the page changed a field or a form of itself or a person
//! typed into one ([`super::page_events`]). Only for an engine that does not
//! say so is the page read again once a second. Fields read from a document
//! the page has left are not drawn. What is kept here is the last answer: a
//! cache of the page, never state.

use std::cell::Cell;
use std::rc::Rc;

use ui::i18n::ArgValue;
use ui::icons::Icon;
use ui::tilo_actions::{ActionBinding, ResourceSpec};
use ui::tilo_resource::Resource as _;
use ui::tokens::TextRole;
use ui::webview::{FrameId, PageActor, PageFailure, PageReply, PageRequest, WebViewCommand};
use ui::widget::{
    Button, ChipStyle, Element, EmptyState, ListRow, Row, Section, SectionHeader, Spacer, Text,
    Tone, Widget,
};
use ui::window_toolbar::{LocationSegment, ToolbarAction, ToolbarLocation};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::BrowserWidget;
use super::window_bar::BarCommand;
use crate::page_review::{Fields, Form, Holds, Question, Record, SetBy, Step};
use crate::resource::{Browser, CATALOG};

/// How often the page is read again while its review shows and nothing said
/// it changed, for an engine that does not say when the page changes itself
/// ([`ui::webview::WebViewEngine::tells_page_events`]): the page's own
/// script may change a field at any time.
const REREAD_EVERY: std::time::Duration = std::time::Duration::from_secs(1);
/// The engine's answers to one round of reads never stay pending longer.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
/// How many of the tab's actions the review lists: the newest.
const STEP_LIMIT: usize = 100;
/// What the window bar's location commits for the page under its review.
const PAGE_PLACE: &str = "browser:page";
/// What it commits for the review itself.
const REVIEW_PLACE: &str = "browser:page-review";

fn t(key: &str) -> String {
    CATALOG.t(key)
}

/// What the review draws of the page's fields.
#[derive(Debug, Default, PartialEq)]
enum Shown {
    /// The page has not answered yet, or left the document it answered
    /// from: nothing is claimed about it.
    #[default]
    Unread,
    /// The fields as the page last answered.
    Read(Fields),
    /// The page did not answer.
    Unreadable,
}

/// One round of reads that is with the engine: the forms, the fields and
/// the record, each until its answer came.
struct Round {
    forms: Option<u32>,
    fields: Option<u32>,
    record: Option<u32>,
    /// What the forms read answered, which the fields are named by.
    asked: Option<serde_json::Value>,
    /// The address of the document the round was asked of.
    url: String,
    deadline: ui::time::Instant,
}

/// The review of this window's page: whether it shows, and what the engine
/// last answered about the page.
#[derive(Default)]
pub(super) struct PageReview {
    open: bool,
    shown: Shown,
    /// The address of the document `shown` was read from.
    url: String,
    record: Record,
    round: Option<Round>,
    /// Something said the page or its record changed since the last round.
    moved: bool,
    /// When the page is read again if nothing says so sooner.
    due: Option<ui::time::Instant>,
    /// A screenshot staged what it shows: it is not read from the engine.
    staged: bool,
    /// How far the review is scrolled, kept while its rows are rebuilt.
    scroll: Rc<Cell<(f32, f32)>>,
}

impl BrowserWidget {
    /// Whether the review stands in the page's place now.
    pub(super) fn page_review_shown(&self) -> bool {
        self.page_review.open
            && !self.show_settings
            && !self.show_library
            && !self.engine_failed
            && self.load_error.is_none()
    }

    /// Show the review of the page in front.
    pub(super) fn open_page_review(&mut self) -> Result<(), String> {
        if !super::is_supported_web_url(&self.current_uri) {
            return Err("A review needs a web page".into());
        }
        self.show_settings = false;
        self.show_library = false;
        if !self.page_review.open {
            self.page_review.open = true;
            self.page_review.moved = true;
            self.page_review.scroll.set((0.0, 0.0));
        }
        self.structure_dirty = true;
        Ok(())
    }

    /// Show the page again. What was read of it is let go: the next review
    /// reads the page as it is then.
    pub(super) fn close_page_review(&mut self) {
        if std::mem::take(&mut self.page_review.open) {
            self.page_review.shown = Shown::Unread;
            self.page_review.due = None;
            self.structure_dirty = true;
        }
    }

    /// Show the page again with the field `element` in view: a scroll of
    /// the page, which takes its turn in the tab like any other action.
    pub(super) fn show_page_field(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        actor: Option<PageActor>,
        element: String,
        frame: Option<FrameId>,
    ) -> Result<(), String> {
        self.start_page_operation(
            invocation,
            actor,
            PageRequest::Scroll {
                element: None,
                to: None,
                by: None,
                into_view: Some(element),
                frame,
            },
        )?;
        self.show_settings = false;
        self.show_library = false;
        self.close_page_review();
        self.structure_dirty = true;
        Ok(())
    }

    /// The resource and action that show the review, or the page again
    /// while the review shows: what its menu row and its key run.
    pub(super) fn review_command(&self) -> (&'static str, &'static str) {
        if self.page_review_shown() {
            (Browser::KIND, "show-page")
        } else {
            ("browser.page", "review-page")
        }
    }

    /// Ctrl+Alt+E (Cmd+Option+E) shows the page's review, or the page
    /// again; Escape leaves the review for the page.
    pub(super) fn review_shortcut(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        if key.state != ElementState::Pressed {
            return false;
        }
        let modifiers = modifiers.state();
        let toggles = ui::platform::PlatformConventions::default().command_modifier_down(modifiers)
            && modifiers.alt_key()
            && matches!(&key.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("e"));
        let leaves = self.page_review_shown() && key.logical_key == Key::Named(NamedKey::Escape);
        if !toggles && !leaves {
            return false;
        }
        let (resource, action) = self.review_command();
        self.queue_unit_operation(resource, action);
        true
    }

    /// The review's entry in the window bar's Page menu: on while it shows,
    /// when it shows the page again.
    pub(super) fn review_bar_entry(&self) -> ToolbarAction {
        let (resource, action) = self.review_command();
        self.bar_entries
            .entry(
                "list-checks",
                t("browser-review-open"),
                BarCommand::Unit(resource, action),
            )
            .on(self.page_review_shown())
    }

    /// Where the window bar puts the review: the page's site, then the
    /// review. The site commits the page again.
    pub(super) fn review_location(&self) -> ToolbarLocation {
        let title = t("browser-review-title");
        ToolbarLocation {
            label: title.clone(),
            name: title.clone(),
            segments: vec![
                LocationSegment {
                    label: super::window_bar::site_label(&self.current_uri),
                    value: PAGE_PLACE.to_owned(),
                },
                LocationSegment {
                    label: title,
                    value: REVIEW_PLACE.to_owned(),
                },
            ],
            ..ToolbarLocation::default()
        }
    }

    /// Go to the place of [`Self::review_location`] the person chose.
    pub(super) fn commit_review_place(&mut self, place: &str) {
        if place == PAGE_PLACE {
            self.queue_unit_operation(Browser::KIND, "show-page");
        }
    }

    /// Whether `request_id` is a read of the page the review sent.
    pub(super) fn reads_page_review(&self, request_id: u32) -> bool {
        self.page_review.round.as_ref().is_some_and(|round| {
            [round.forms, round.fields, round.record].contains(&Some(request_id))
        })
    }

    /// Something said the page or the tab's record changed: a page request
    /// that is not the review's answered, the engine told of a change or of
    /// what a person typed, or the page finished loading. The review reads
    /// again.
    pub(super) fn page_review_moved(&mut self) {
        if self.page_review.open {
            self.note_review_moved();
        }
    }

    /// The review is due a read. With an engine that tells page events
    /// nothing reads on a timer, so the wait for a page that is loading
    /// counts from when the read fell due.
    fn note_review_moved(&mut self) {
        if !std::mem::replace(&mut self.page_review.moved, true) && self.engine.tells_page_events()
        {
            self.page_review.due = Some(ui::time::now());
        }
    }

    /// The engine answered one of the review's reads.
    pub(super) fn accept_page_review(
        &mut self,
        request_id: u32,
        result: Result<PageReply, PageFailure>,
    ) {
        let loading = self.page_loading();
        let review = &mut self.page_review;
        let Some(round) = review.round.as_mut() else {
            return;
        };
        let data = result.ok().and_then(|reply| reply.data);
        let mut changed = false;
        if round.forms == Some(request_id) {
            round.forms = None;
            round.asked = data;
        } else if round.fields == Some(request_id) {
            round.fields = None;
            let shown = match &data {
                Some(data) => Shown::Read(Fields::read(round.asked.as_ref(), data)),
                // A page on its way to another document is not one that
                // cannot be read: it is read once it has loaded.
                None if loading => Shown::Unread,
                None => Shown::Unreadable,
            };
            if review.open && (shown != review.shown || review.url != round.url) {
                review.shown = shown;
                review.url.clone_from(&round.url);
                changed = true;
            }
        } else if round.record == Some(request_id) {
            round.record = None;
            if let Some(data) = &data {
                let (added, more) = review.record.take(data);
                changed |= added;
                // The record goes on past this answer: it is read on.
                review.moved |= more;
            }
        }
        if round.forms.is_none() && round.fields.is_none() && round.record.is_none() {
            review.round = None;
        }
        self.structure_dirty |= changed && review.open;
    }

    /// Read the page for its review when something says it changed and when
    /// it shows another document; once in a while otherwise only for an
    /// engine that does not say when the page changes itself.
    pub(super) fn sync_page_review(&mut self) {
        if !self.page_review_shown() || self.page_review.staged {
            return;
        }
        let now = ui::time::now();
        if self
            .page_review
            .round
            .as_ref()
            .is_some_and(|round| now >= round.deadline)
        {
            self.page_review.round = None;
            self.structure_dirty |= self.page_review.shown != Shown::Unreadable;
            self.page_review.shown = Shown::Unreadable;
        }
        // Fields read from a document the page has left are not the page's.
        if matches!(self.page_review.shown, Shown::Read(_))
            && self.page_review.url != self.current_uri
        {
            self.page_review.shown = Shown::Unread;
            self.note_review_moved();
            self.structure_dirty = true;
        }
        if self.page_review.round.is_some() {
            return;
        }
        let due = self.page_review.due;
        // A page that is loading is read once it has loaded, or as it stands
        // when it loads for too long.
        if self.page_loading() && due.is_none_or(|at| now < at + super::PAGE_LOAD_WAIT) {
            return;
        }
        // An engine that tells page events says when the page changed
        // itself; any other is read again once in a while.
        let polls = !self.engine.tells_page_events();
        if !self.page_review.moved && (!polls || due.is_some_and(|at| now < at)) {
            return;
        }
        self.page_review.moved = false;
        self.page_review.due = Some(now + REREAD_EVERY);
        let since = self.page_review.record.since();
        let forms = self.ask_page_for_review(PageRequest::Forms { frame: None });
        let fields = self.ask_page_for_review(PageRequest::Fields { frame: None });
        let record = self.ask_page_for_review(PageRequest::Ledger { since });
        self.page_review.round = Some(Round {
            forms: Some(forms),
            fields: Some(fields),
            record: Some(record),
            asked: None,
            url: self.current_uri.clone(),
            deadline: now + READ_TIMEOUT,
        });
    }

    /// When the review next needs the window awake: for the answers it
    /// waits on, or to read the page again.
    pub(super) fn page_review_deadline(&self) -> Option<ui::time::Instant> {
        if !self.page_review_shown() || self.page_review.staged {
            return None;
        }
        // Nothing is read on a timer for an engine that tells page events:
        // the window is woken by what the engine tells.
        let polls = !self.engine.tells_page_events();
        match &self.page_review.round {
            Some(round) => Some(round.deadline),
            None if !polls && !self.page_review.moved => None,
            None if self.page_loading() => {
                self.page_review.due.map(|at| at + super::PAGE_LOAD_WAIT)
            }
            None if polls => self.page_review.due,
            None => None,
        }
    }

    /// Send the engine one read for the review, as the tab's one unnamed
    /// actor: the person at this window. A read takes no place in the tab's
    /// record and tells no other actor anything.
    fn ask_page_for_review(&mut self, request: PageRequest) -> u32 {
        self.page_request_seq = self.page_request_seq % i32::MAX.unsigned_abs() + 1;
        self.engine.send(WebViewCommand::Page {
            request_id: self.page_request_seq,
            actor: None,
            request,
        });
        self.page_request_seq
    }

    /// The review's own header outside a Tilo session: its title and Done.
    /// While the desktop's bar hosts the window, the bar's location names
    /// the review after the page's site, which shows the page again.
    fn review_header(&self) -> Row {
        Row::new()
            .no_wrap()
            .push(Box::new(
                Text::new(&t("browser-review-title"))
                    .with_role(TextRole::LargeTitle)
                    .with_heading_level(1),
            ))
            .push(Box::new(Spacer::new()))
            .push(Box::new(self.bind_button(
                Button::new("browser.review.close", &t("browser-review-done")),
                Browser::KIND,
                "show-page",
            )))
    }

    /// The review in the page's place: each form with its questions, then
    /// what was done in the tab.
    pub(super) fn build_review_composition(
        &self,
        notices: &[crate::notice::Notice],
    ) -> Box<dyn Widget> {
        let review = &self.page_review;
        let mut page = ui::widget::Page::new()
            .with_persisted_offset(Rc::clone(&review.scroll))
            .with_semantic_key("browser.review");
        for notice in notices {
            page = page.section(notice.widget(false));
        }
        if !ui::window_toolbar::is_hosted() {
            page = page.section(Box::new(self.review_header()));
        }
        let steps = !review.record.steps.is_empty();
        match &review.shown {
            Shown::Unread => {}
            Shown::Unreadable => {
                let state = EmptyState::error(
                    t("browser-review-unreadable"),
                    t("browser-review-unreadable-hint"),
                )
                .with_icon(Icon::ListChecks)
                .with_id("browser.review.unreadable");
                page = page.section(Box::new(if steps { state.inline() } else { state }));
            }
            Shown::Read(fields) if fields.forms.is_empty() => {
                let state = EmptyState::new(t("browser-review-empty"))
                    .with_icon(Icon::ListChecks)
                    .with_id("browser.review.empty");
                page = page.section(Box::new(if steps { state.inline() } else { state }));
            }
            Shown::Read(fields) => {
                // Each row is bound to the page's own action for it.
                let actions = self.resource_by_id("browser.page");
                for form in &fields.forms {
                    page = page.section(Box::new(form_section(form, actions.as_ref())));
                }
            }
        }
        if steps {
            let secrets = match &review.shown {
                Shown::Read(fields) => fields.secrets(),
                Shown::Unread | Shown::Unreadable => Vec::new(),
            };
            page = page.section(Box::new(record_section(&review.record, &secrets)));
        }
        Box::new(page)
    }

    /// Screenshot hook: the review of a visa application two agents and a
    /// person filled, read from the engine's own answer shapes.
    pub(super) fn stage_page_review(&mut self) {
        let now = u64::try_from(ui::clock::local_now().timestamp_millis()).unwrap_or_default();
        let (forms, fields, record) = shot_page(now);
        // The chrome names the page the review is of.
        self.current_uri = "https://visas.example.org/apply".into();
        self.initial_url.clone_from(&self.current_uri);
        self.address.set(self.current_uri.clone());
        self.title = "Apply for a visitor visa".into();
        self.show_settings = false;
        self.show_library = false;
        self.page_review.open = true;
        self.page_review.staged = true;
        self.page_review.shown = Shown::Read(Fields::read(Some(&forms), &fields));
        self.page_review.record = Record::default();
        self.page_review.record.take(&record);
    }
}

/// `key` with `name` interpolated.
fn named(key: &str, name: &str) -> String {
    CATALOG.t_args(key, &[("name", ArgValue::Text(name.to_owned()))])
}

/// `key` said of `count` things.
fn counted(key: &str, count: usize) -> String {
    CATALOG.t_args(
        key,
        &[("count", ArgValue::Number(ui::num::f64_of_usize(count)))],
    )
}

/// The name an actor is shown under: an agent by its own name, anyone else
/// by the name the action door admitted them under. `None` for the tab's
/// one unnamed actor, the person at this window.
fn actor_name(actor: &str) -> Option<String> {
    let actor = actor.trim();
    if actor.is_empty() {
        return None;
    }
    Some(ui::history_source::agent_principal_name(actor).unwrap_or_else(|| actor.to_owned()))
}

/// What a question holds now, in words. A secret is said as set or not set.
fn holds_text(holds: &Holds) -> String {
    match holds {
        Holds::Nothing => t("browser-review-no-answer"),
        Holds::Text(held) => held.clone(),
        Holds::Yes => t("browser-review-yes"),
        Holds::No => t("browser-review-no"),
        Holds::Secret { set: true } => t("browser-review-secret-set"),
        Holds::Secret { set: false } => t("browser-review-secret-not-set"),
    }
}

/// Who set a question's answer, in words; nothing where the page did not
/// say.
fn set_by_text(set: &SetBy) -> Option<String> {
    Some(match set {
        SetBy::Action { actor, .. } => match actor_name(actor) {
            Some(name) => named("browser-review-set-by", &name),
            None => t("browser-review-set-by-you"),
        },
        SetBy::Input => t("browser-review-set-typed"),
        SetBy::Page => t("browser-review-set-by-page"),
        SetBy::NoOne => t("browser-review-set-by-no-one"),
        SetBy::Unknown => return None,
    })
}

/// One form: its name, then a row per question.
fn form_section(form: &Form, page: Option<&ResourceSpec>) -> Section {
    let label = if !form.label.is_empty() {
        form.label.clone()
    } else if form.id.is_empty() {
        t("browser-review-form-other")
    } else {
        t("browser-review-form-untitled")
    };
    let place = if form.id.is_empty() {
        "other"
    } else {
        &form.id
    };
    let rows = form
        .questions
        .iter()
        .enumerate()
        .map(|(index, question)| question_row(place, index, question, page))
        .collect();
    Section::new(rows).with_header(SectionHeader::heading(label).count(form.questions.len()))
}

/// One question: what it asks, what it holds now, who set that, and what is
/// wrong with it. Choosing it shows its field on the page.
fn question_row(
    form: &str,
    index: usize,
    question: &Question,
    page: Option<&ResourceSpec>,
) -> Element {
    let name = if question.name.trim().is_empty() {
        t("browser-review-question-unnamed")
    } else {
        question.name.clone()
    };
    let mut row = ListRow::new(format!("browser.review.{form}.{index}"), name)
        .subtitle(holds_text(&question.holds));
    if let Some(who) = set_by_text(&question.set) {
        row = row.secondary(who);
    }
    let empty = question.required_and_empty();
    let marks: Vec<(String, ChipStyle)> = [
        (question.changed, "browser-review-changed", Tone::Warning),
        (empty, "browser-review-required", Tone::Danger),
        (question.invalid, "browser-review-invalid", Tone::Danger),
    ]
    .into_iter()
    .filter(|(marked, ..)| *marked)
    .map(|(_, key, tone)| (t(key), ChipStyle::Tinted(tone)))
    .collect();
    let tags: Vec<(&str, ChipStyle)> = marks
        .iter()
        .map(|(label, style)| (label.as_str(), *style))
        .collect();
    row = row.tags(&tags);
    if empty || question.invalid {
        row = row.error();
    } else if question.changed {
        row = row.warning();
    }
    // What the page itself says is wrong, in the page's words.
    if let Some(problem) = &question.problem {
        row = row.note(problem.clone());
    }
    if let (Some(element), Some(page)) = (&question.element, page) {
        let mut arguments = serde_json::Map::new();
        arguments.insert("element".into(), element.clone().into());
        if let Ok(binding) = ActionBinding::static_json_for(page, "show-page-field", arguments) {
            row = row.on_action(binding).disclosure();
        }
    }
    Box::new(row)
}

/// What was done in the tab, oldest first: the newest [`STEP_LIMIT`]
/// actions, and how many came before them.
fn record_section(record: &Record, secrets: &[&str]) -> Section {
    let earlier = record.steps.len().saturating_sub(STEP_LIMIT);
    let mut rows: Vec<Element> = Vec::new();
    if earlier > 0 {
        rows.push(Box::new(
            Text::new(&counted("browser-review-earlier", earlier)).with_role(TextRole::Caption),
        ));
    }
    rows.extend(
        record
            .steps
            .iter()
            .skip(earlier)
            .map(|step| step_row(step, secrets)),
    );
    Section::new(rows)
        .with_header(SectionHeader::heading(t("browser-review-record")).count(record.steps.len()))
}

/// One action of the tab's record: what was done, what it wrote, who did it
/// and when.
fn step_row(step: &Step, secrets: &[&str]) -> Element {
    let mut row = ListRow::new(
        format!("browser.review.step.{}", step.seq),
        step_title(step),
    );
    // The record holds no secret's text. What was written into a field that
    // holds one is not shown either way.
    let secret = step
        .target
        .as_deref()
        .is_some_and(|target| secrets.contains(&target));
    if step.secret {
        // The record says the field was set, and that is what is shown.
        row = row.subtitle(t("browser-review-wrote-secret"));
    } else if !secret && !step.wrote.is_empty() {
        row = row.subtitle(step.wrote.join(", "));
    }
    row = row.secondary(if step.typed {
        // Typed straight into the page, at this window or through one that
        // shares it: no actor's action.
        t("browser-review-by-typing")
    } else {
        match actor_name(&step.actor) {
            Some(name) => named("browser-review-by", &name),
            None => t("browser-review-by-you"),
        }
    });
    if let Some(when) = step.time_ms.and_then(time_of) {
        row = row.meta(when);
    }
    if !step.ok {
        let failed = t("browser-review-failed");
        row = row
            .tags(&[(failed.as_str(), ChipStyle::Tinted(Tone::Danger))])
            .error();
    }
    Box::new(row)
}

/// What an action did, in words: the engine's name for it, and the element
/// it acted on by its own name.
fn step_title(step: &Step) -> String {
    let on = |key: &str, target: &str| {
        CATALOG.t_args(key, &[("target", ArgValue::Text(target.to_owned()))])
    };
    let target = step
        .target
        .as_deref()
        .filter(|target| !target.trim().is_empty());
    if step.typed {
        return match target {
            Some(target) => on("browser-review-did-type", target),
            None => t("browser-review-did-type-page"),
        };
    }
    match (step.action.as_str(), target) {
        ("click", Some(target)) => on("browser-review-did-click", target),
        ("fill", Some(target)) => on("browser-review-did-fill", target),
        ("select", Some(target)) => on("browser-review-did-select", target),
        ("fill_files", Some(target)) => on("browser-review-did-files", target),
        ("press", Some(target)) => on("browser-review-did-press", target),
        ("press", None) => t("browser-review-did-key"),
        ("hover", Some(target)) => on("browser-review-did-hover", target),
        ("drag", Some(target)) => on("browser-review-did-drag", target),
        ("hover" | "drag" | "path" | "pointer", _) => t("browser-review-did-pointer"),
        ("submit", _) => t("browser-review-did-submit"),
        ("scroll", _) => t("browser-review-did-scroll"),
        ("reader", _) => t("browser-review-did-reader"),
        ("fill_form", _) => counted("browser-review-did-form", step.wrote.len()),
        _ => t("browser-review-did-other"),
    }
}

/// The time of day an action ended, as this device shows times.
fn time_of(time_ms: u64) -> Option<String> {
    let when = chrono::DateTime::from_timestamp_millis(i64::try_from(time_ms).ok()?)?
        .with_timezone(&chrono::Local);
    Some(ui::clock::time_of_day(&when))
}

/// One field as the engine's fields read lists it for a screenshot: the
/// field of `question` in the one form, with whatever `more` says of it.
fn shot_field(
    element: &str,
    role: &str,
    question: &str,
    value: &str,
    more: serde_json::Value,
) -> serde_json::Value {
    let mut field = serde_json::json!({"element": element, "role": role, "name": question,
        "value": value, "required": true, "form": "form1", "question": question});
    if let (Some(field), serde_json::Value::Object(more)) = (field.as_object_mut(), more) {
        field.extend(more);
    }
    field
}

/// The page a screenshot reviews: a visa application as the engine's forms,
/// fields and record reads answer for it, its actions ending shortly before
/// `now` (milliseconds since the Unix epoch).
#[allow(
    clippy::too_many_lines,
    reason = "one page's answers stay together, in the shapes the engine gives them"
)]
fn shot_page(now: u64) -> (serde_json::Value, serde_json::Value, serde_json::Value) {
    use serde_json::json;
    let ago = |seconds: u64| now.saturating_sub(seconds * 1000);
    let by = |actor: &str, seq: u64| json!({"set": {"how": "action", "by": actor, "seq": seq}});
    let refused = "Refused a visa before?";
    let forms = json!({"forms": [{
        "form": "form1", "label": "Apply for a visitor visa", "method": "post",
        "action": "/apply", "signature": "5d0c7a1e", "submit": ["Send application"],
        "fields": [
            {"name": "Given names", "kind": "text", "required": true,
                "value": "Maria Elena", "element": "d1e4"},
            {"name": "Family name", "kind": "text", "required": true,
                "value": "Okonkwo-Reyes", "element": "d1e5"},
            {"name": "Nationality", "kind": "choice", "required": true,
                "choices": ["Kenya", "Portugal", "Other"], "value": ["Portugal"],
                "element": "d1e6"},
            {"name": "Passport number", "kind": "text", "required": true,
                "value": "P7K290XB4", "element": "d1e7"},
            {"name": "Passport expiry", "kind": "date", "required": true,
                "value": "2025-03-14", "element": "d1e8"},
            {"name": refused, "kind": "choice", "required": true,
                "choices": ["Yes", "No"], "value": ["No"], "elements": ["d1e10", "d1e11"]},
            {"name": "Arrival", "kind": "date", "required": true,
                "value": "2027-02-10", "element": "d1e12"},
            {"name": "Email", "kind": "email", "required": true,
                "value": "maria.okonkwo@example.org", "element": "d1e13"},
            {"name": "Phone", "kind": "tel", "required": true, "element": "d1e14"},
            {"name": "Portal password", "kind": "secret", "required": true,
                "value": "<16 characters hidden>", "element": "d1e15"},
            {"name": "I declare these answers are true", "kind": "flag",
                "required": true, "value": true, "element": "d1e16"},
        ],
    }]});
    let fields = json!({"fields": [
        shot_field("d1e4", "textbox", "Given names", "Maria Elena", by("agent:intake", 1)),
        shot_field("d1e5", "textbox", "Family name", "Okonkwo-Reyes", by("agent:intake", 1)),
        shot_field("d1e6", "combobox", "Nationality", "Portugal", by("agent:intake", 1)),
        shot_field("d1e7", "textbox", "Passport number", "P7K290XB4", by("agent:documents", 2)),
        shot_field("d1e8", "textbox", "Passport expiry", "2025-03-14", json!({
            "set": {"how": "action", "by": "agent:documents", "seq": 2},
            "invalid": true,
            "problem": "The passport must be valid for six months after arrival.",
        })),
        // The two radios answer one question, each by its own label.
        shot_field("d1e10", "radio", "Yes", "unchecked", json!({
            "question": refused, "answer": "Yes",
        })),
        shot_field("d1e11", "radio", "No", "checked", json!({
            "question": refused, "answer": "No",
            "set": {"how": "action", "by": "agent:documents", "seq": 2},
        })),
        shot_field("d1e12", "textbox", "Arrival", "2027-02-10", json!({
            "set": {"how": "action", "by": "agent:intake", "seq": 3, "changed": true},
        })),
        shot_field("d1e13", "textbox", "Email", "maria.okonkwo@example.org", json!({
            "set": {"how": "input"},
        })),
        shot_field("d1e14", "textbox", "Phone", "", json!({})),
        shot_field("d1e15", "textbox", "Portal password", "<16 characters hidden>", by("", 4)),
        shot_field("d1e16", "checkbox", "I declare these answers are true", "checked", json!({
            "set": {"how": "page"},
        })),
    ]});
    let record = json!({"entries": [
        {"seq": 1, "time_ms": ago(260), "actor": "agent:intake", "action": "fill_form",
            "ok": true, "answers": [
                {"name": "Given names", "value": "Maria Elena"},
                {"name": "Family name", "value": "Okonkwo-Reyes"},
                {"name": "Nationality", "value": "Portugal"},
            ]},
        {"seq": 2, "time_ms": ago(205), "actor": "agent:documents", "action": "fill_form",
            "ok": true, "answers": [
                {"name": "Passport number", "value": "P7K290XB4"},
                {"name": "Passport expiry", "value": "2025-03-14"},
                {"name": refused, "value": "No"},
            ]},
        {"seq": 3, "time_ms": ago(150), "actor": "agent:intake", "action": "fill",
            "ok": true, "target": {"role": "textbox", "name": "Arrival"}, "text": "2027-02-03"},
        {"seq": 4, "time_ms": ago(95), "actor": "", "action": "fill", "ok": true,
            "target": {"role": "textbox", "name": "Portal password"}},
        {"seq": 5, "time_ms": ago(40), "actor": "agent:documents", "action": "click",
            "ok": false, "target": {"role": "button", "name": "Send application"}},
    ], "next": 5, "more": false, "dropped": 0, "waiting": 0});
    (forms, fields, record)
}

#[cfg(test)]
mod tests;

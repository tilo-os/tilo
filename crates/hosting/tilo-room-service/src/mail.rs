//! The mail the persistent room's door sends about its wait list: the link
//! that confirms an address, the invitation a member sends there, and word
//! that an address left the list.
//!
//! The words are in `mail/` beside this crate's sources, one plain-text and
//! one HTML file per mail and language, and `mail/layout.html` around the
//! HTML ones. `TILO_ROOMS_MAIL_TEMPLATES` names a directory of the same
//! shape to use instead (the shared mail repository's generated coffee set);
//! a configured file that is missing or malformed stops the service, never
//! falling back to the built-in words. A plain-text file's first line is the
//! subject (after an optional `Subject: `). `{{workspace}}`, `{{email}}`,
//! `{{link}}`, `{{remove_link}}` and `{{door}}` are filled in, HTML-escaped
//! in the HTML; a layout fills `{{body}}`, and may name the language with
//! `{{language}}`. Only those placeholders are allowed where they belong,
//! each is filled once, and a filled value is never searched for
//! placeholders again: a name or address that spells `{{link}}` stays text.
//! The `mail-preview <dir>` command of `tilo-room-service` writes every mail
//! as it is sent.
//!
//! A [`DoorMail`] hands what it sends to the outbox it opened, which
//! sends through a [`Mailer`] — [`Resend`] over its HTTPS API, or a fake in
//! tests — with a bounded retry and one idempotency key per mail. Nothing
//! here logs an address, a link or a secret.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use tilo_relay_protocol::room_operator::WaitListMailbox;

use crate::config::MailConfig;
use crate::outbox::{Limits, Outbox};

/// The mails the door sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The link that confirms an address left at the door.
    Confirm,
    /// A member's invitation, to a confirmed address.
    Invite,
    /// The address left the list.
    Removed,
}

impl Kind {
    pub const ALL: [Self; 3] = [Self::Confirm, Self::Invite, Self::Removed];

    const fn name(self) -> &'static str {
        match self {
            Self::Confirm => "confirm",
            Self::Invite => "invite",
            Self::Removed => "removed",
        }
    }

    /// What a log line calls this mail, so it names no address.
    pub(crate) const fn describe(self) -> &'static str {
        match self {
            Self::Confirm => "a confirmation link",
            Self::Invite => "an invitation",
            Self::Removed => "word of a removal",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.describe())
    }
}

/// The languages the mails are in ([`tilo_relay_protocol::rooms::mail_language`]).
pub const LANGUAGES: [&str; 2] = ["en", "de"];

/// The most any one template file may be.
const MAX_TEMPLATE_BYTES: usize = 256 * 1024;

/// The most a subject may be, as RFC 5322 bounds a header line.
const MAX_SUBJECT_BYTES: usize = 998;

/// The most a link the door fills in may be: an invitation carries its
/// certificate chain ([`tilo_relay_protocol::rooms::MAX_INVITATION_LINK`])
/// under the web entry's prefix.
const MAX_LINK_BYTES: usize = tilo_relay_protocol::rooms::MAX_INVITATION_LINK + 4096;

/// What a mail body may fill.
const BODY_PLACEHOLDERS: [&str; 5] = ["workspace", "email", "link", "remove_link", "door"];

/// What a layout may fill: the body it wraps, and the language it is in.
const LAYOUT_PLACEHOLDERS: [&str; 2] = ["body", "language"];

const LAYOUT: &str = include_str!("../mail/layout.html");

/// One mail's plain text and HTML, as a configured directory or the built-in
/// `mail/` holds them.
#[derive(Clone, Debug)]
struct Body {
    text: String,
    html: String,
}

/// One language's three mails.
#[derive(Clone, Debug)]
struct Language {
    confirm: Body,
    invite: Body,
    removed: Body,
}

/// The words of every mail the door sends, in every language, checked when
/// they are loaded.
#[derive(Clone, Debug)]
pub struct Templates {
    layout: String,
    en: Language,
    de: Language,
}

impl Templates {
    /// The templates built into the service, for a door that configures no
    /// directory (`TILO_ROOMS_MAIL_TEMPLATES`).
    pub fn baked() -> Self {
        Self {
            layout: LAYOUT.to_owned(),
            en: baked_language("en"),
            de: baked_language("de"),
        }
    }

    /// The templates in `dir`: `layout.html` and, for every language,
    /// `<language>/<mail>.txt` and `.html`. Every one must be there and be a
    /// template this mail fills; a door that cannot load them sends nothing.
    pub fn load(dir: &Path) -> Result<Self> {
        ensure!(
            dir.is_dir(),
            "{} is not a directory of mail templates",
            dir.display()
        );
        let layout = load_file(&dir.join("layout.html"), &LAYOUT_PLACEHOLDERS, true)?;
        Ok(Self {
            layout,
            en: load_language(dir, "en")?,
            de: load_language(dir, "de")?,
        })
    }

    fn body(&self, kind: Kind, language: &str) -> &Body {
        let language = if language == "de" { &self.de } else { &self.en };
        match kind {
            Kind::Confirm => &language.confirm,
            Kind::Invite => &language.invite,
            Kind::Removed => &language.removed,
        }
    }

    /// `kind` in `language` to `to`, filled with `values`.
    pub fn render(&self, kind: Kind, language: &str, to: &str, values: &Values<'_>) -> Mail {
        let language = if LANGUAGES.contains(&language) {
            language
        } else {
            LANGUAGES[0]
        };
        let body = self.body(kind, language);
        let text = filled(&body.text, &Fill::Values(values), str::to_owned);
        let (subject, body_text) = text.split_once('\n').unwrap_or((&text, ""));
        let subject = subject
            .strip_prefix("Subject:")
            .unwrap_or(subject)
            .trim()
            .to_owned();
        let html_body = filled(&body.html, &Fill::Values(values), escaped);
        let html = filled(
            &self.layout,
            &Fill::Frame {
                body: html_body.trim_end(),
                language,
            },
            str::to_owned,
        );
        Mail {
            to: to.to_owned(),
            subject,
            text: body_text.trim_start_matches('\n').to_owned(),
            html,
        }
    }
}

/// One mail in the built-in `mail/` directory.
fn baked_body(kind: Kind, language: &str) -> Body {
    macro_rules! files {
        ($language:literal, $name:literal) => {
            (
                include_str!(concat!("../mail/", $language, "/", $name, ".txt")),
                include_str!(concat!("../mail/", $language, "/", $name, ".html")),
            )
        };
    }
    let (text, html) = match (language, kind) {
        ("de", Kind::Confirm) => files!("de", "confirm"),
        ("de", Kind::Invite) => files!("de", "invite"),
        ("de", Kind::Removed) => files!("de", "removed"),
        (_, Kind::Confirm) => files!("en", "confirm"),
        (_, Kind::Invite) => files!("en", "invite"),
        (_, Kind::Removed) => files!("en", "removed"),
    };
    Body {
        text: text.to_owned(),
        html: html.to_owned(),
    }
}

fn baked_language(language: &str) -> Language {
    Language {
        confirm: baked_body(Kind::Confirm, language),
        invite: baked_body(Kind::Invite, language),
        removed: baked_body(Kind::Removed, language),
    }
}

fn load_language(dir: &Path, language: &str) -> Result<Language> {
    let body = |kind: Kind| -> Result<Body> {
        let stem = dir.join(language).join(kind.name());
        Ok(Body {
            text: load_file(&stem.with_extension("txt"), &BODY_PLACEHOLDERS, false)?,
            html: load_file(&stem.with_extension("html"), &BODY_PLACEHOLDERS, false)?,
        })
    };
    Ok(Language {
        confirm: body(Kind::Confirm)?,
        invite: body(Kind::Invite)?,
        removed: body(Kind::Removed)?,
    })
}

fn load_file(path: &Path, allowed: &[&str], frame: bool) -> Result<String> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    check_template(path, &text, allowed, frame)?;
    Ok(text)
}

/// Whether `template` is one this service can fill, with `allowed` the
/// placeholders its place (`frame`: a layout) may name: nothing else is
/// filled, and a `{{` that never closes is a malformed template rather than
/// text, so a mail never goes out holding a placeholder.
fn check_template(path: &Path, template: &str, allowed: &[&str], frame: bool) -> Result<()> {
    ensure!(
        !template.trim().is_empty(),
        "{}: the mail template is empty",
        path.display()
    );
    ensure!(
        template.len() <= MAX_TEMPLATE_BYTES,
        "{}: a mail template is at most {MAX_TEMPLATE_BYTES} bytes",
        path.display()
    );
    ensure!(
        template
            .chars()
            .all(|c| !c.is_control() || matches!(c, '\n' | '\t')),
        "{}: a mail template keeps no carriage return or other control character",
        path.display()
    );
    let names =
        placeholders(template).map_err(|error| anyhow::anyhow!("{}: {error}", path.display()))?;
    for name in &names {
        ensure!(
            allowed.contains(name),
            "{}: {{{{{name}}}}} is not a placeholder {} fills; only {}",
            path.display(),
            if frame { "a layout" } else { "a mail" },
            allowed.join(", ")
        );
    }
    if frame {
        ensure!(
            names.iter().filter(|name| **name == "body").count() == 1,
            "{}: a layout fills {{{{body}}}} exactly once",
            path.display()
        );
    } else {
        let subject = template.lines().next().unwrap_or_default();
        ensure!(
            !subject.trim().is_empty() && subject.len() <= MAX_SUBJECT_BYTES,
            "{}: the first line is the subject, of at most {MAX_SUBJECT_BYTES} bytes",
            path.display()
        );
    }
    Ok(())
}

/// Every `{{name}}` in `template`, in order; a `{{` with no `}}` after it is
/// an error, since a template that would mail a literal placeholder is
/// malformed.
fn placeholders(template: &str) -> std::result::Result<Vec<&str>, &'static str> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = rest.get(start.saturating_add(2)..).unwrap_or_default();
        let Some(end) = after.find("}}") else {
            return Err("a {{ placeholder is never closed");
        };
        names.push(after.get(..end).unwrap_or_default());
        rest = after.get(end.saturating_add(2)..).unwrap_or_default();
    }
    Ok(names)
}

/// What a mail's placeholders are filled with.
#[derive(Clone, Debug, Default)]
pub struct Values<'a> {
    pub workspace: &'a str,
    pub email: &'a str,
    /// The confirmation or invitation link.
    pub link: &'a str,
    /// The link that takes the address off the list.
    pub remove_link: &'a str,
    /// The door itself.
    pub door: &'a str,
}

impl Values<'_> {
    fn get(&self, name: &str) -> Option<&str> {
        match name {
            "workspace" => Some(self.workspace),
            "email" => Some(self.email),
            "link" => Some(self.link),
            "remove_link" => Some(self.remove_link),
            "door" => Some(self.door),
            _ => None,
        }
    }
}

/// What one fill of a template takes its values from.
enum Fill<'a> {
    /// A mail's values.
    Values(&'a Values<'a>),
    /// The body a layout wraps, and the language the mail is in.
    Frame { body: &'a str, language: &'a str },
}

impl Fill<'_> {
    fn get(&self, name: &str) -> Option<&str> {
        match self {
            Self::Values(values) => values.get(name),
            Self::Frame { body, language } => match name {
                "body" => Some(body),
                "language" => Some(language),
                _ => None,
            },
        }
    }
}

/// One mail to send.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub text: String,
    pub html: String,
}

impl Mail {
    /// Whether this is a mail the door may hand to a mail service: an
    /// address it accepts, a subject of one header line without control
    /// characters, and a body.
    pub(crate) fn is_sendable(&self) -> bool {
        tilo_relay_protocol::rooms::contact_address(&self.to).is_ok()
            && !self.subject.is_empty()
            && self.subject.len() <= MAX_SUBJECT_BYTES
            && !self.subject.chars().any(char::is_control)
            && !self.text.trim().is_empty()
            && !self.html.trim().is_empty()
    }

    /// How much room this mail takes in an outbox, near enough for a bound.
    pub(crate) fn weight(&self) -> usize {
        self.to
            .len()
            .saturating_add(self.subject.len())
            .saturating_add(self.text.len())
            .saturating_add(self.html.len())
    }
}

impl fmt::Debug for Mail {
    /// Without the address or the body, whose links are secrets: what a log
    /// line may say.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mail")
            .field("subject", &self.subject)
            .finish_non_exhaustive()
    }
}

fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// `template` with each placeholder filled once, left to right: a value that
/// itself spells a placeholder is text, never another placeholder.
fn filled(template: &str, fill: &Fill<'_>, escape: fn(&str) -> String) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(rest.get(..start).unwrap_or_default());
        let after = rest.get(start.saturating_add(2)..).unwrap_or_default();
        let Some(end) = after.find("}}") else {
            out.push_str(rest.get(start..).unwrap_or_default());
            return out;
        };
        let name = after.get(..end).unwrap_or_default();
        match fill.get(name) {
            Some(value) => out.push_str(&escape(value)),
            // Checked when the templates are loaded; a name that is not
            // filled stays as it is rather than disappearing.
            None => out.push_str(
                rest.get(start..start.saturating_add(end).saturating_add(4))
                    .unwrap_or_default(),
            ),
        }
        rest = after.get(end.saturating_add(2)..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

/// Whether `url` is a page mail may link to: `https` with a host, or, in the
/// local review mode (`TILO_ROOMS_LOCAL`) the door was started in, a plain
/// `http` loopback address, which `TILO_ROOMS_WEB_ENTRY` accepts there too.
/// Nothing that could run as another kind of URL, carry credentials or break
/// out of the attribute.
fn safe_url(url: &str, local: bool) -> bool {
    if url.len() > MAX_LINK_BYTES
        || url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>'))
    {
        return false;
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    if scheme.eq_ignore_ascii_case("https") {
        return true;
    }
    local && scheme.eq_ignore_ascii_case("http") && loopback(authority)
}

fn loopback(authority: &str) -> bool {
    let host = match authority.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

/// Sends a mail; why not, without the address or the body.
pub trait Mailer: Send + Sync + 'static {
    /// Send `mail` with `id` as the mail service's idempotency key, so a
    /// repeated attempt after an uncertain answer is one mail.
    fn send(&self, id: &str, mail: Mail) -> BoxFuture<'static, std::result::Result<(), SendError>>;
}

/// Why a mail was not sent: whether trying again may deliver it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendError {
    /// Not now: the service asked for time, was busy or could not be
    /// reached. `after` is the wait it asked for, if it named one.
    Retry {
        after: Option<Duration>,
        reason: String,
    },
    /// Never: the service refused this mail, and another attempt would be
    /// refused alike.
    Permanent(String),
}

/// Mail through Resend's HTTP API (`POST /emails`).
pub struct Resend {
    client: tilo_net::Client,
    endpoint: String,
    from: String,
    key: Arc<str>,
}

impl Resend {
    /// The sender `config` describes, its API key read from its file.
    pub fn new(config: &MailConfig) -> Result<Self> {
        let key = std::fs::read_to_string(&config.resend_key_file)
            .with_context(|| format!("read {}", config.resend_key_file.display()))?;
        let key = key.trim();
        ensure!(
            !key.is_empty() && !key.chars().any(char::is_whitespace),
            "{} holds no API key",
            config.resend_key_file.display()
        );
        let client = tilo_net::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirects(tilo_net::Redirects::None)
            .build()
            .context("the mail client")?;
        Ok(Self {
            client,
            endpoint: config.resend_url.clone(),
            from: config.from.clone(),
            key: key.into(),
        })
    }
}

impl Mailer for Resend {
    fn send(&self, id: &str, mail: Mail) -> BoxFuture<'static, std::result::Result<(), SendError>> {
        let request = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&*self.key)
            .header("idempotency-key", id)
            .json(&serde_json::json!({
                "from": self.from,
                "to": [mail.to],
                "subject": mail.subject,
                "text": mail.text,
                "html": mail.html,
            }));
        Box::pin(async move {
            let response = match request.send().await {
                Ok(response) => response,
                Err(error) => return Err(transport_error(&error)),
            };
            let status = response.status();
            if status.is_success() {
                return Ok(());
            }
            // Not the body: it may repeat the address.
            let reason = format!("the mail service answered {status}");
            let asked = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            if waits(status.as_u16()) {
                Err(SendError::Retry {
                    after: asked,
                    reason,
                })
            } else {
                Err(SendError::Permanent(reason))
            }
        })
    }
}

/// Whether the service refusing this way may let the same mail through
/// later: a request timeout, a conflict (the same key in flight or already
/// used), too many requests, or its own failure. Every other refusal is the
/// mail itself, which another attempt would be refused for again.
fn waits(status: u16) -> bool {
    matches!(status, 408 | 409 | 425 | 429 | 500..=599)
}

fn transport_error(error: &tilo_net::NetError) -> SendError {
    use tilo_net::NetError;
    // A peer that broke the protocol may be a proxy in the way; the attempt
    // is bounded either way. A request or URL this service built wrongly is
    // not worth repeating.
    let retry = matches!(
        error,
        NetError::Timeout
            | NetError::Connect { .. }
            | NetError::Closed
            | NetError::Body(_)
            | NetError::Protocol(_)
    );
    // `NetError`'s text names a host, never an address or a link.
    if retry {
        SendError::Retry {
            after: None,
            reason: format!("the mail service could not be reached: {error}"),
        }
    } else {
        SendError::Permanent(format!("the mail request was refused: {error}"))
    }
}

/// The door's mail: what it fills in, and where what it sends is kept.
#[derive(Clone)]
pub struct DoorMail {
    /// The door's page (`TILO_ROOMS_WEB_ENTRY`), which the links open.
    door: String,
    workspace: String,
    templates: Templates,
    outbox: Arc<Outbox>,
    /// The door runs in the local review mode (`TILO_ROOMS_LOCAL`), where
    /// its page may be a plain `http` loopback address.
    local: bool,
}

impl DoorMail {
    /// A door that keeps its mail only while the process runs, with the
    /// built-in words: for tests. A persistent room's door is
    /// [`Self::durable`], so a restart delays its mail rather than losing
    /// it.
    #[cfg(test)]
    pub fn new(mailer: Arc<dyn Mailer>, door: &str, workspace: &str) -> Self {
        Self::assembled(
            door,
            workspace,
            Templates::baked(),
            Outbox::in_memory(mailer, Limits::DEFAULT),
            true,
        )
    }

    /// The door of a persistent room: `templates` as configured or built in,
    /// and every mail kept in `dir` until it is sent, across restarts.
    pub fn durable(
        mailer: Arc<dyn Mailer>,
        door: &str,
        workspace: &str,
        templates: Templates,
        local: bool,
        dir: &Path,
    ) -> Result<Self> {
        ensure!(
            door.ends_with('/') && safe_url(door, local),
            "the door's page is an https URL ending in / (a local http loopback one in the local mode)"
        );
        let outbox = Outbox::durable(dir, mailer, Limits::DEFAULT)
            .with_context(|| format!("open the mail outbox in {}", dir.display()))?;
        Ok(Self::assembled(door, workspace, templates, outbox, local))
    }

    fn assembled(
        door: &str,
        workspace: &str,
        templates: Templates,
        outbox: Outbox,
        local: bool,
    ) -> Self {
        let outbox = Arc::new(outbox);
        Self {
            door: door.to_owned(),
            workspace: workspace.to_owned(),
            templates,
            outbox,
            local,
        }
    }

    fn link(&self, parameter: &str, secret: &str) -> String {
        format!("{}?{parameter}={secret}", self.door)
    }

    /// Whether `link` is an invitation on this door, which is all a member
    /// may have the door mail.
    pub fn on_door(&self, link: &str) -> bool {
        tilo_relay_protocol::rooms::invitation_on_door(&self.door, link)
    }

    fn mail(&self, kind: Kind, to: &WaitListMailbox, link: &str, removal: &str) -> Option<Mail> {
        let remove = self.link("wait-list-remove", removal);
        let remove_link = if removal.is_empty() {
            ""
        } else {
            remove.as_str()
        };
        // The room checks an invitation link, and the door's own links are
        // built from its page; this is the last look before a link is mailed
        // to someone who will click it.
        if !safe_url(&self.door, self.local)
            || (!link.is_empty() && !safe_url(link, self.local))
            || (!remove_link.is_empty() && !safe_url(remove_link, self.local))
        {
            return None;
        }
        let values = Values {
            workspace: &self.workspace,
            email: &to.contact,
            link,
            remove_link,
            door: &self.door,
        };
        Some(
            self.templates
                .render(kind, &to.language, &to.contact, &values),
        )
    }

    /// The confirmation mail: `confirm` and `removal` are the secrets (hex)
    /// its two links carry.
    pub fn confirm(&self, to: &WaitListMailbox, confirm: &str, removal: &str) -> Option<Mail> {
        self.mail(
            Kind::Confirm,
            to,
            &self.link("wait-list-confirm", confirm),
            removal,
        )
    }

    /// A member's invitation `link`, with the secret (hex) that takes the
    /// address off the list.
    pub fn invite(&self, to: &WaitListMailbox, link: &str, removal: &str) -> Option<Mail> {
        self.mail(Kind::Invite, to, link, removal)
    }

    /// Word that the address left the list.
    pub fn removed(&self, to: &WaitListMailbox) -> Option<Mail> {
        self.mail(Kind::Removed, to, "", "")
    }

    pub(crate) fn start(&self) {
        self.outbox.start();
    }

    pub(crate) fn set_eligibility(&self, check: crate::outbox::Eligibility) {
        self.outbox.set_eligibility(check);
    }

    pub(crate) fn confirmation_kept(&self, contact: &str) -> bool {
        self.outbox.confirmation_kept(contact)
    }

    pub(crate) fn cancel(&self, removal: &str, contact_sha256: Option<&str>) {
        self.outbox.cancel(removal, contact_sha256);
    }

    pub(crate) fn send_for(&self, kind: Kind, mail: Option<Mail>, removal: &str) -> bool {
        let Some(mail) = mail else {
            return false;
        };
        let contact = mail.to.clone();
        if kind == Kind::Confirm && !self.outbox.confirmation_receipt(&contact, false) {
            return false;
        }
        let kept = self
            .outbox
            .enqueue_for(kind, mail, Some(removal.to_owned()));
        if !kept {
            return false;
        }
        if kind == Kind::Confirm && !self.outbox.confirmation_receipt(&contact, true) {
            return false;
        }
        self.outbox.start();
        true
    }

    /// Keep `mail` (when the door built one) to be sent, and say whether it
    /// is kept for at least one attempt. A mail the door will not build, or
    /// an outbox with no room, is logged without the address.
    pub fn send(&self, kind: Kind, mail: Option<Mail>) -> bool {
        let Some(mail) = mail else {
            eprintln!(
                "tilo-room-service: {kind} was not mailed: the door's links are not plain https URLs"
            );
            return false;
        };
        self.outbox.start();
        self.outbox.enqueue(kind, mail)
    }
}

/// `tilo-room-service mail-preview <dir>`: every mail in every language,
/// with sample values, written to `dir` as `<language>-<mail>.txt` (subject
/// first) and `.html`. The paths written.
pub fn preview(dir: &Path, templates: &Templates) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let door = "https://tilo.example/";
    let link = |name: &str| format!("{door}?{name}=0123456789abcdef");
    let (confirm, remove) = (link("wait-list-confirm"), link("wait-list-remove"));
    let invite = format!("{door}#/desktop/room/example?invite=AAECAwQF");
    let mut written = Vec::new();
    for language in LANGUAGES {
        for kind in Kind::ALL {
            let values = Values {
                workspace: "Lounge",
                email: "ada@example.org",
                link: match kind {
                    Kind::Confirm => &confirm,
                    Kind::Invite => &invite,
                    Kind::Removed => "",
                },
                remove_link: &remove,
                door,
            };
            let mail = templates.render(kind, language, values.email, &values);
            let stem = dir.join(format!("{language}-{}", kind.name()));
            let text = stem.with_extension("txt");
            std::fs::write(&text, format!("Subject: {}\n\n{}", mail.subject, mail.text))
                .with_context(|| format!("write {}", text.display()))?;
            let html = stem.with_extension("html");
            std::fs::write(&html, &mail.html)
                .with_context(|| format!("write {}", html.display()))?;
            written.extend([text, html]);
        }
    }
    Ok(written)
}

/// A mailer that keeps what it is asked to send, for tests.
#[cfg(test)]
#[derive(Default)]
pub struct FakeMailer {
    pub sent: std::sync::Mutex<Vec<Mail>>,
}

#[cfg(test)]
impl Mailer for FakeMailer {
    fn send(
        &self,
        _id: &str,
        mail: Mail,
    ) -> BoxFuture<'static, std::result::Result<(), SendError>> {
        if let Ok(mut sent) = self.sent.lock() {
            sent.push(mail);
        }
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_acceptance_and_queue_failure_stay_honest_across_restart() {
        let root = tempfile::tempdir().unwrap();
        let mailer = Arc::new(FakeMailer::default());
        let limits = Limits {
            max_items: 1,
            ..Limits::DEFAULT
        };
        let open = || {
            DoorMail::assembled(
                "https://tilo.example/",
                "Lounge",
                Templates::baked(),
                Outbox::durable(root.path(), mailer.clone(), limits).unwrap(),
                false,
            )
        };
        let door = open();
        let to = WaitListMailbox {
            contact: "Ada@example.org".into(),
            language: "en".into(),
        };
        assert!(door.send_for(
            Kind::Confirm,
            door.confirm(&to, &"ab".repeat(32), &"cd".repeat(32)),
            &"ef".repeat(32)
        ));
        assert!(door.confirmation_kept("ada@example.org"));
        drop(door);
        let restarted = open();
        assert!(restarted.confirmation_kept("ADA@example.org"));
        assert!(!restarted.send_for(
            Kind::Confirm,
            restarted.confirm(&to, &"12".repeat(32), &"34".repeat(32)),
            &"56".repeat(32)
        ));
        assert!(!restarted.confirmation_kept("ada@example.org"));
        drop(restarted);
        assert!(!open().confirmation_kept("ada@example.org"));
        assert!(mailer.sent.lock().unwrap().is_empty());
    }

    #[test]
    fn the_built_in_templates_are_templates_this_mail_fills() {
        let templates = Templates::baked();
        check_template(
            Path::new("layout.html"),
            &templates.layout,
            &LAYOUT_PLACEHOLDERS,
            true,
        )
        .unwrap();
        for (language, templates) in [("en", &templates.en), ("de", &templates.de)] {
            for (kind, body) in [
                (Kind::Confirm, &templates.confirm),
                (Kind::Invite, &templates.invite),
                (Kind::Removed, &templates.removed),
            ] {
                let stem = Path::new(language).join(kind.name());
                check_template(
                    &stem.with_extension("txt"),
                    &body.text,
                    &BODY_PLACEHOLDERS,
                    false,
                )
                .unwrap();
                check_template(
                    &stem.with_extension("html"),
                    &body.html,
                    &BODY_PLACEHOLDERS,
                    false,
                )
                .unwrap();
            }
        }
    }

    #[test]
    fn every_mail_fills_every_placeholder_in_every_language() {
        for language in LANGUAGES {
            for kind in Kind::ALL {
                let values = Values {
                    workspace: "Lounge",
                    email: "ada@example.org",
                    link: "https://tilo.example/?x=1",
                    remove_link: "https://tilo.example/?y=2",
                    door: "https://tilo.example/",
                };
                let mail = Templates::baked().render(kind, language, "ada@example.org", &values);
                assert!(!mail.subject.is_empty() && !mail.subject.contains("{{"));
                for body in [&mail.text, &mail.html] {
                    assert!(!body.contains("{{"), "{language} {kind:?}: {body}");
                }
                assert!(mail.html.starts_with("<!doctype html>"));
                assert!(mail.html.contains(&format!("lang=\"{language}\"")));
                if kind != Kind::Removed {
                    assert!(mail.text.contains("https://tilo.example/?y=2"));
                }
            }
        }
    }

    #[test]
    fn the_html_escapes_what_it_is_filled_with_and_the_text_does_not() {
        let values = Values {
            workspace: "<Ada & Bo>",
            ..Values::default()
        };
        let mail = Templates::baked().render(Kind::Removed, "en", "x@example.org", &values);
        assert!(mail.html.contains("&lt;Ada &amp; Bo&gt;"));
        assert!(!mail.html.contains("<Ada"));
        assert!(mail.text.contains("<Ada & Bo>"));
    }

    #[test]
    fn a_value_that_spells_a_placeholder_is_never_filled_again() {
        let values = Values {
            workspace: "{{link}} <{{door}}>",
            email: "ada@example.org",
            link: "https://tilo.example/?wait-list-confirm=aa",
            remove_link: "https://tilo.example/?wait-list-remove=bb",
            door: "https://tilo.example/",
        };
        let mail = Templates::baked().render(Kind::Confirm, "en", "ada@example.org", &values);
        // As text it stays as typed, in the HTML escaped, and neither takes
        // the link's place.
        assert!(mail.text.contains("{{link}} <{{door}}>"));
        assert!(mail.html.contains("{{link}} &lt;{{door}}&gt;"));
        assert_eq!(mail.html.matches("wait-list-confirm=aa").count(), 1);
        assert_eq!(mail.text.matches("wait-list-confirm=aa").count(), 1);
        assert!(mail.is_sendable());
    }

    #[test]
    fn a_mail_is_logged_without_its_address_or_links() {
        let door = DoorMail::new(
            Arc::new(FakeMailer::default()),
            "https://tilo.example/",
            "Lounge",
        );
        let to = WaitListMailbox {
            contact: "ada@example.org".into(),
            language: "de".into(),
        };
        let mail = door.confirm(&to, "c0ffee", "beef").unwrap();
        assert!(
            mail.text
                .contains("https://tilo.example/?wait-list-confirm=c0ffee")
        );
        assert!(
            mail.text
                .contains("https://tilo.example/?wait-list-remove=beef")
        );
        assert!(mail.subject.contains("Warteliste"));
        let logged = format!("{mail:?}");
        assert!(!logged.contains("ada@") && !logged.contains("c0ffee"));
    }

    #[test]
    fn a_member_has_only_invitations_on_this_door_mailed() {
        let door = DoorMail::new(
            Arc::new(FakeMailer::default()),
            "https://tilo.example/",
            "Lounge",
        );
        assert!(door.on_door("https://tilo.example/?invite=AAEC"));
        assert!(door.on_door("https://tilo.example/#/desktop/room/x?invite=AAEC"));
        // An invitation with its certificate chain is several KiB long.
        let long = format!(
            "https://tilo.example/#/@host/w/x?invite={}",
            "A".repeat(6000)
        );
        assert!(door.on_door(&long));
        for language in LANGUAGES {
            let to = WaitListMailbox {
                contact: "ada@example.org".into(),
                language: language.into(),
            };
            let mail = door.invite(&to, &long, &"ab".repeat(32)).unwrap();
            assert!(mail.text.contains(&long));
            assert!(mail.html.contains(&long));
            assert!(door.send_for(Kind::Invite, Some(mail), &"cd".repeat(32)));
        }
        for link in [
            "https://evil.example/?invite=AAEC",
            "https://tilo.example.evil/?invite=AAEC",
            "https://tilo.example/?other=1",
            "https://tilo.example/?invite=AA EC",
            &format!("https://tilo.example/?invite={}", "A".repeat(20_000)),
        ] {
            assert!(!door.on_door(link), "{link}");
        }
    }

    #[test]
    fn the_door_mails_only_plain_https_links_on_a_page_that_is_one() {
        assert!(safe_url("https://tilo.example/", false));
        assert!(safe_url(
            "https://tilo.example.invalid/?wait-list-confirm=aa",
            false
        ));
        assert!(safe_url(
            "https://tilo.example/#/desktop/room/x?invite=AAEC",
            false
        ));
        assert!(safe_url("http://localhost:8790/", true));
        assert!(safe_url("http://127.0.0.1:9/", true));
        assert!(safe_url("http://[::1]:9/", true));
        // A plain loopback page is the local review mode's, not a room's.
        assert!(!safe_url("http://localhost:8790/", false));
        assert!(!safe_url("http://127.0.0.1:9/", false));
        for bad in [
            "",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "//tilo.example/",
            "http://tilo.example/",
            "https:///",
            "https://user:pass@tilo.example/",
            "https://tilo.example/a b",
            "https://tilo.example/\r\n",
        ] {
            assert!(!safe_url(bad, true) && !safe_url(bad, false), "{bad:?}");
        }

        let door = DoorMail::new(
            Arc::new(FakeMailer::default()),
            "https://tilo.example/",
            "Lounge",
        );
        let to = WaitListMailbox {
            contact: "ada@example.org".into(),
            language: "en".into(),
        };
        assert!(
            door.invite(&to, "javascript:alert(1)", "aa").is_none(),
            "an invitation the room let through is never mailed"
        );
        assert!(
            door.invite(&to, "http://tilo.example/?invite=AA", "aa")
                .is_none()
        );
        assert!(
            door.invite(&to, "http://localhost:8790/?invite=AA", "aa")
                .is_some()
        );

        // A room's door, outside the local mode, mails no plain http link.
        let dir = tempfile::tempdir().unwrap();
        let room = DoorMail::durable(
            Arc::new(FakeMailer::default()),
            "https://tilo.example/",
            "Lounge",
            Templates::baked(),
            false,
            &dir.path().join("mail-outbox"),
        )
        .unwrap();
        assert!(
            room.invite(&to, "http://localhost:8790/?invite=AA", "aa")
                .is_none()
        );
        assert!(
            room.invite(&to, "https://tilo.example/?invite=AA", "aa")
                .is_some()
        );
    }

    #[test]
    fn a_subject_that_could_be_more_headers_is_never_sent() {
        let good = Mail {
            to: "ada@example.org".into(),
            subject: "You're invited to Lounge".into(),
            text: "hello".into(),
            html: "<p>hello</p>".into(),
        };
        assert!(good.is_sendable());
        for subject in [
            String::new(),
            "Hello\r\nBcc: evil@example.net".into(),
            "Hello\nBcc: evil@example.net".into(),
            "Hello\u{0}".into(),
            "x".repeat(MAX_SUBJECT_BYTES + 1),
        ] {
            let mail = Mail {
                subject,
                ..good.clone()
            };
            assert!(!mail.is_sendable());
        }
        let mail = Mail {
            to: "ada@example.org\r\nBcc: evil@example.net".into(),
            ..good
        };
        assert!(!mail.is_sendable());
    }

    /// A configured directory of templates in `dir`: the layout wraps a body
    /// and names no language, as the shared mail repository generates them.
    fn configured(dir: &Path, layout: &str, confirm: &str, subject: &str) {
        std::fs::write(dir.join("layout.html"), layout).unwrap();
        for language in LANGUAGES {
            let language = dir.join(language);
            std::fs::create_dir_all(&language).unwrap();
            std::fs::write(language.join("confirm.txt"), subject).unwrap();
            std::fs::write(language.join("confirm.html"), confirm).unwrap();
            for kind in [Kind::Invite, Kind::Removed] {
                std::fs::write(
                    language.join(format!("{}.txt", kind.name())),
                    "Subject: {{workspace}}\n\nHi from {{workspace}}.\n",
                )
                .unwrap();
                std::fs::write(
                    language.join(format!("{}.html", kind.name())),
                    "<p>Hi from {{workspace}}.</p>",
                )
                .unwrap();
            }
        }
    }

    #[test]
    fn a_configured_directory_is_used_and_a_missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        configured(
            dir.path(),
            "<!doctype html><html><body>{{body}}</body></html>",
            "<p>Confirm {{email}} for {{workspace}}: <a href=\"{{link}}\">here</a></p>",
            "Subject: Confirm for {{workspace}}\n\n{{email}}: {{link}}\n",
        );
        let templates = Templates::load(dir.path()).unwrap();
        let out = tempfile::tempdir().unwrap();
        let written = preview(out.path(), &templates).unwrap();
        assert_eq!(written.len(), LANGUAGES.len() * Kind::ALL.len() * 2);
        let mail = templates.render(
            Kind::Confirm,
            "en",
            "ada@example.org",
            &Values {
                workspace: "Lounge",
                email: "ada@example.org",
                link: "https://tilo.example/?wait-list-confirm=aa",
                remove_link: "https://tilo.example/?wait-list-remove=bb",
                door: "https://tilo.example/",
            },
        );
        assert!(mail.html.starts_with("<!doctype html>"));
        assert!(mail.html.contains("Confirm ada@example.org for Lounge"));
        assert!(!mail.html.contains("lang="));
        assert_eq!(mail.subject, "Confirm for Lounge");

        // Every file is required: none falls back to the built-in words.
        for missing in ["layout.html", "en/confirm.txt", "de/removed.html"] {
            let broken = tempfile::tempdir().unwrap();
            configured(
                broken.path(),
                "<!doctype html><html><body>{{body}}</body></html>",
                "<p>{{email}}</p>",
                "Subject: {{workspace}}\n\n{{link}}\n",
            );
            std::fs::remove_file(broken.path().join(missing)).unwrap();
            assert!(Templates::load(broken.path()).is_err(), "{missing}");
        }
    }

    #[test]
    fn a_malformed_template_is_an_error_rather_than_a_fallback() {
        let cases = [
            // A layout must wrap a body, once, and fills nothing else.
            (
                "<!doctype html><html><body></body></html>",
                "Subject: x\n\n{{link}}\n",
            ),
            (
                "<!doctype html><html><body>{{body}}{{body}}</body></html>",
                "Subject: x\n\n{{link}}\n",
            ),
            (
                "<!doctype html><html><body>{{body}}{{link}}</body></html>",
                "Subject: x\n\n{{link}}\n",
            ),
            // A mail fills no body and no unknown placeholder.
            (
                "<!doctype html><html><body>{{body}}</body></html>",
                "Subject: x\n\n{{body}}\n",
            ),
            (
                "<!doctype html><html><body>{{body}}</body></html>",
                "Subject: x\n\n{{secret}}\n",
            ),
            // A placeholder that never closes would be mailed as text.
            (
                "<!doctype html><html><body>{{body}}</body></html>",
                "Subject: x\n\n{{link\n",
            ),
            // A subject is the first line.
            (
                "<!doctype html><html><body>{{body}}</body></html>",
                "\n{{link}}\n",
            ),
            // Carriage returns could split a header.
            (
                "<!doctype html><html><body>{{body}}</body></html>",
                "Subject: x\r\n\r\n{{link}}\r\n",
            ),
        ];
        for (layout, confirm) in cases {
            let dir = tempfile::tempdir().unwrap();
            configured(dir.path(), layout, "<p>{{email}}</p>", confirm);
            assert!(
                Templates::load(dir.path()).is_err(),
                "{layout:?} {confirm:?}"
            );
        }
        // A directory that is not one is refused, not replaced by defaults.
        assert!(Templates::load(Path::new("/nonexistent/tilo-mail")).is_err());
    }

    #[test]
    fn the_preview_writes_every_mail_in_every_language() {
        let dir = tempfile::tempdir().unwrap();
        let written = preview(dir.path(), &Templates::baked()).unwrap();
        assert_eq!(written.len(), LANGUAGES.len() * Kind::ALL.len() * 2);
        assert!(written.iter().all(|path| path.is_file()));
    }

    #[test]
    fn the_mail_service_is_asked_again_only_when_it_may_let_the_mail_through() {
        for status in [408, 409, 425, 429, 500, 503, 599] {
            assert!(waits(status), "{status}");
        }
        for status in [400, 401, 403, 404, 413, 422] {
            assert!(!waits(status), "{status}");
        }
    }
}

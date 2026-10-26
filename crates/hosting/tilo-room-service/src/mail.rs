//! The mail the persistent room's door sends about its wait list: the link
//! that confirms an address, the invitation a member sends there, and word
//! that an address left the list.
//!
//! The words are in `mail/` beside this crate's sources, one plain-text and
//! one HTML file per mail and language, and `mail/layout.html` around the
//! HTML ones. A plain-text file's first line is the subject (`Subject: …`).
//! `{{workspace}}`, `{{email}}`, `{{link}}`, `{{remove_link}}` and `{{door}}`
//! are filled in, HTML-escaped in the HTML. The `mail-preview <dir>` command
//! of `tilo-room-service` writes every mail as it is sent.
//!
//! A [`Mailer`] sends them: [`Resend`] over its HTTPS API, or a fake in
//! tests. Nothing here logs an address, a link or a secret.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use futures::future::BoxFuture;
use tilo_relay_protocol::room_operator::WaitListMailbox;

use crate::config::MailConfig;

/// The mails the door sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
}

/// The languages the mails are in ([`tilo_relay_protocol::rooms::mail_language`]).
pub const LANGUAGES: [&str; 2] = ["en", "de"];

const LAYOUT: &str = include_str!("../mail/layout.html");

/// The plain text and HTML of `kind` in `language`.
fn template(kind: Kind, language: &str) -> (&'static str, &'static str) {
    macro_rules! files {
        ($language:literal, $name:literal) => {
            (
                include_str!(concat!("../mail/", $language, "/", $name, ".txt")),
                include_str!(concat!("../mail/", $language, "/", $name, ".html")),
            )
        };
    }
    match (language, kind) {
        ("de", Kind::Confirm) => files!("de", "confirm"),
        ("de", Kind::Invite) => files!("de", "invite"),
        ("de", Kind::Removed) => files!("de", "removed"),
        (_, Kind::Confirm) => files!("en", "confirm"),
        (_, Kind::Invite) => files!("en", "invite"),
        (_, Kind::Removed) => files!("en", "removed"),
    }
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

/// One mail to send.
#[derive(Clone, PartialEq, Eq)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub text: String,
    pub html: String,
}

impl fmt::Debug for Mail {
    /// Without the address or the body, whose links are secrets.
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

fn filled(template: &str, values: &Values<'_>, escape: fn(&str) -> String) -> String {
    [
        ("{{workspace}}", values.workspace),
        ("{{email}}", values.email),
        ("{{link}}", values.link),
        ("{{remove_link}}", values.remove_link),
        ("{{door}}", values.door),
    ]
    .iter()
    .fold(template.to_owned(), |text, (placeholder, value)| {
        text.replace(placeholder, &escape(value))
    })
}

/// `kind` in `language` to `to`, filled with `values`.
pub fn render(kind: Kind, language: &str, to: &str, values: &Values<'_>) -> Mail {
    let (text, html) = template(kind, language);
    let text = filled(text, values, str::to_owned);
    let (subject, body) = text.split_once('\n').unwrap_or((&text, ""));
    let subject = subject
        .strip_prefix("Subject:")
        .unwrap_or(subject)
        .trim()
        .to_owned();
    let language = if LANGUAGES.contains(&language) {
        language
    } else {
        LANGUAGES[0]
    };
    Mail {
        to: to.to_owned(),
        subject,
        text: body.trim_start_matches('\n').to_owned(),
        html: LAYOUT
            .replace("{{language}}", language)
            .replace("{{body}}", filled(html, values, escaped).trim_end()),
    }
}

/// Sends mail.
pub trait Mailer: Send + Sync + 'static {
    /// Send `mail`; why not, without the address.
    fn send(&self, mail: Mail) -> BoxFuture<'static, std::result::Result<(), String>>;
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
            .timeout(std::time::Duration::from_secs(20))
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
    fn send(&self, mail: Mail) -> BoxFuture<'static, std::result::Result<(), String>> {
        let request = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&*self.key)
            .json(&serde_json::json!({
                "from": self.from,
                "to": [mail.to],
                "subject": mail.subject,
                "text": mail.text,
                "html": mail.html,
            }));
        Box::pin(async move {
            let response = request.send().await.map_err(|error| error.to_string())?;
            let status = response.status();
            if status.is_success() {
                Ok(())
            } else {
                // Not the body: it may repeat the address.
                Err(format!("the mail service answered {status}"))
            }
        })
    }
}

/// The door's mail: what it fills in, and who sends it.
#[derive(Clone)]
pub struct DoorMail {
    mailer: Arc<dyn Mailer>,
    /// The door's page (`TILO_ROOMS_WEB_ENTRY`), which the links open.
    door: String,
    workspace: String,
}

impl DoorMail {
    pub fn new(mailer: Arc<dyn Mailer>, door: &str, workspace: &str) -> Self {
        Self {
            mailer,
            door: door.to_owned(),
            workspace: workspace.to_owned(),
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

    fn mail(&self, kind: Kind, to: &WaitListMailbox, link: &str, removal: &str) -> Mail {
        let remove_link = self.link("wait-list-remove", removal);
        let values = Values {
            workspace: &self.workspace,
            email: &to.contact,
            link,
            remove_link: if removal.is_empty() { "" } else { &remove_link },
            door: &self.door,
        };
        render(kind, &to.language, &to.contact, &values)
    }

    /// The confirmation mail: `confirm` and `removal` are the secrets (hex)
    /// its two links carry.
    pub fn confirm(&self, to: &WaitListMailbox, confirm: &str, removal: &str) -> Mail {
        self.mail(
            Kind::Confirm,
            to,
            &self.link("wait-list-confirm", confirm),
            removal,
        )
    }

    /// A member's invitation `link`, with the secret (hex) that takes the
    /// address off the list.
    pub fn invite(&self, to: &WaitListMailbox, link: &str, removal: &str) -> Mail {
        self.mail(Kind::Invite, to, link, removal)
    }

    /// Word that the address left the list.
    pub fn removed(&self, to: &WaitListMailbox) -> Mail {
        self.mail(Kind::Removed, to, "", "")
    }

    /// Send `mail` in the background; a failure is logged as `what` failed,
    /// without the address.
    pub fn send(&self, what: &'static str, mail: Mail) {
        let sending = self.mailer.send(mail);
        tokio::spawn(async move {
            if let Err(error) = sending.await {
                eprintln!("tilo-room-service: {what} was not mailed: {error}");
            }
        });
    }
}

/// `tilo-room-service mail-preview <dir>`: every mail in every language,
/// with sample values, written to `dir` as `<language>-<mail>.txt` (subject
/// first) and `.html`. The paths written.
pub fn preview(dir: &Path) -> Result<Vec<PathBuf>> {
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
            let mail = render(kind, language, values.email, &values);
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
    fn send(&self, mail: Mail) -> BoxFuture<'static, std::result::Result<(), String>> {
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
                let mail = render(kind, language, "ada@example.org", &values);
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
        let mail = render(Kind::Removed, "en", "x@example.org", &values);
        assert!(mail.html.contains("&lt;Ada &amp; Bo&gt;"));
        assert!(!mail.html.contains("<Ada"));
        assert!(mail.text.contains("<Ada & Bo>"));
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
        let mail = door.confirm(&to, "c0ffee", "beef");
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
    fn the_preview_writes_every_mail_in_every_language() {
        let dir = tempfile::tempdir().unwrap();
        let written = preview(dir.path()).unwrap();
        assert_eq!(written.len(), LANGUAGES.len() * Kind::ALL.len() * 2);
        assert!(written.iter().all(|path| path.is_file()));
    }
}

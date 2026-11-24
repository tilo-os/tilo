//! What each kind of question a program asks shows the person, as an asked record's facts, and
//! how their answer reaches the program again.
//!
//! The words are the person's, in their language, read from the catalogue the questions were
//! always worded from. What a program said itself (polkit's message, an app's reason, a
//! network's name) is shown as it said it, never as our words.

use serde_json::Value;
use tilo_action_policy::Lifetime;
use tilo_actions::remote::asked::{
    AskedBy, AskedFacts, Fact, FileType, FilesAsked, FilesMode, Take,
};
use tilo_bluetooth_agent::{Method as PairingMethod, PairingDetails};
use tilo_network_secret_agent::questions::{
    CANNOT_JOIN_KIND, CannotJoinDetails, FieldRule, KIND as NETWORK_KIND, SecretsDetails,
    SimUnlockStep, Text as Words,
};
use tilo_polkit_agent::questions::{AuthenticateDetails, Notice};
use tilo_portal::account::AccountQuestion;
use tilo_portal::app_chooser::AppChooserQuestion;
use tilo_portal::background::{ACTIVE, BACKGROUND, BackgroundQuestion, RUNNING};
use tilo_portal::consent::AccessQuestion;
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
use tilo_service::{Answer, Question};
use tilo_sheets::{word, words};
use zeroize::Zeroizing;

use crate::app_names::claimant;

/// What the person answered, as the dispatch hands it back with their approve: the choice, the
/// rows, the files, and what they typed, which lives in memory only.
#[derive(Default)]
pub(crate) struct Answered {
    pub choice: Option<usize>,
    pub chosen: Vec<usize>,
    pub files: Vec<String>,
    pub typed: Vec<Zeroizing<String>>,
}

/// A question as the person is asked it: what its record shows, what an answer to it means, and
/// the allowance it leaves when the person allows it beyond this session.
pub(crate) struct Raised {
    pub facts: AskedFacts,
    pub reply: Reply,
    pub grant: Option<Grant>,
}

/// What a program's question becomes for the person.
pub(crate) enum Put {
    /// A question they answer.
    Ask(Raised),
    /// Something they only read, which they may stop the program from: a code to type on another
    /// device.
    Shown(Shown),
}

/// What a notice shows for a question that asks nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Shown {
    pub from: String,
    pub title: String,
    pub body: String,
    /// What stopping the program is called.
    pub stop: String,
}

/// An allowance a question leaves once the person allows it beyond this session: what the notice
/// that offers to take it back names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Grant {
    pub app_id: String,
    pub app_label: String,
    /// What the notice words it as.
    pub capability: String,
    /// The device it is kept under, by [`collab_types::caps::DeviceCap::name`].
    pub device: String,
}

impl Grant {
    /// Whether the answer that picked `choice` leaves this allowance: one that outlasts the
    /// session.
    pub(crate) fn kept_by(choice: usize) -> bool {
        Lifetime::chosen(choice, tilo_clock::unix_now_ms()).is_some_and(Lifetime::is_durable)
    }

    /// What the notice says was allowed.
    pub(crate) fn words(&self) -> (String, String) {
        (
            word("portal-allowed-title"),
            words(
                "portal-allowed-detail",
                &[("app", &self.app_label), ("what", &what(&self.capability))],
            ),
        )
    }
}

/// What an approve answers the program with.
pub(crate) enum Reply {
    /// Approving is the answer: the question asks nothing else.
    Confirm,
    /// The picked choice, answered as the number it stands for.
    Choose(Vec<usize>),
    /// The picked rows, named as the question names them.
    Rows(Vec<String>),
    /// One picked row, named as the question names it.
    Pick(Vec<String>),
    /// The rows are shortcut bindings: each take records one chord.
    Bind(Vec<Binding>),
    /// A launcher's name (the first take, where it may be edited) and icon (the choice).
    Launcher { name: bool, icons: usize },
    /// Approving agrees, as choice 0.
    Agree,
    /// What was typed into the one take, agreeing with it.
    Entry,
    /// A fingerprint typed into the one take, which must be this certificate digest's.
    Fingerprint(String),
    /// A password for an identity: the choice picks it by its place in `order`, else the one
    /// asked for, which stands first.
    Password { order: Vec<usize> },
    /// A network's secrets, one take each; the second choice keeps them on this device.
    Secrets { remember: bool },
    /// A SIM's PUK and its new PIN typed twice, which must agree.
    Puk,
    /// The files or the place chosen.
    Files,
}

/// One row of a global-shortcut question.
pub(crate) struct Binding {
    id: String,
    /// The chord the app proposed.
    proposed: Option<String>,
}

/// Why an answer reached nothing: what the next asking of the same question says at its fields.
pub(crate) type Retry = String;

impl Reply {
    /// The program's answer from the person's.
    pub(crate) fn answer(&self, answered: Answered) -> Result<Answer, Retry> {
        let Answered {
            choice,
            chosen,
            files,
            mut typed,
        } = answered;
        let first = |typed: &mut Vec<Zeroizing<String>>| {
            if typed.is_empty() {
                Zeroizing::new(String::new())
            } else {
                typed.remove(0)
            }
        };
        Ok(match self {
            Self::Confirm => Answer::default(),
            Self::Agree => Answer {
                choice: Some(0),
                ..Answer::default()
            },
            Self::Choose(choices) => Answer {
                choice: Some(
                    *choice
                        .and_then(|picked| choices.get(picked))
                        .ok_or_else(|| word("agent-ask-pick"))?,
                ),
                ..Answer::default()
            },
            Self::Pick(names) => Answer {
                uris: vec![
                    choice
                        .and_then(|picked| names.get(picked))
                        .cloned()
                        .ok_or_else(|| word("agent-ask-pick"))?,
                ],
                ..Answer::default()
            },
            Self::Rows(names) => Answer {
                uris: chosen
                    .iter()
                    .filter_map(|row| names.get(*row).cloned())
                    .collect(),
                ..Answer::default()
            },
            Self::Bind(bindings) => bindings_answer(bindings, &typed),
            Self::Launcher { name, icons } => {
                let mut values = Vec::new();
                if *name {
                    let typed = first(&mut typed);
                    if !valid_name(&typed) {
                        return Err(word("portal-launcher-bad-name"));
                    }
                    values.push(typed);
                }
                Answer {
                    choice: (*icons > 0).then(|| choice.unwrap_or(0)),
                    values,
                    ..Answer::default()
                }
            }
            Self::Entry => {
                let typed = first(&mut typed);
                if typed.trim().is_empty() {
                    return Err(word("portal-secret-incomplete"));
                }
                Answer {
                    choice: Some(0),
                    values: vec![Zeroizing::new(typed.trim().to_owned())],
                    ..Answer::default()
                }
            }
            Self::Fingerprint(certificate) => {
                if !first(&mut typed).trim().eq_ignore_ascii_case(certificate) {
                    return Err(crate::i18n::t("airdrop-error-verification"));
                }
                Answer {
                    values: vec![Zeroizing::new(certificate.clone())],
                    ..Answer::default()
                }
            }
            Self::Password { order } => Answer {
                choice: order.get(choice.unwrap_or(0)).copied(),
                values: vec![first(&mut typed)],
                ..Answer::default()
            },
            Self::Secrets { remember } => Answer {
                remember: *remember && choice == Some(1),
                values: typed,
                ..Answer::default()
            },
            Self::Puk => {
                let [puk, pin, again] = <[Zeroizing<String>; 3]>::try_from(typed)
                    .map_err(|_| word("portal-secret-required"))?;
                if *pin != *again {
                    return Err(word("portal-sim-pins-differ"));
                }
                Answer {
                    values: vec![puk, pin],
                    ..Answer::default()
                }
            }
            // The picker hands back paths on this device; a program is answered with their URIs.
            Self::Files => Answer {
                uris: files
                    .into_iter()
                    .map(|file| match url::Url::from_file_path(&file) {
                        Ok(uri) => uri.to_string(),
                        Err(()) => file,
                    })
                    .collect(),
                ..Answer::default()
            },
        })
    }
}

/// Whether `name` may name a launcher.
fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.trim() == name
        && name.len() <= 255
        && !name.chars().any(char::is_control)
}

/// The bindings the chords `typed`, one per row, answer with.
fn bindings_answer(bindings: &[Binding], typed: &[Zeroizing<String>]) -> Answer {
    let chords: Vec<Option<&str>> = bindings
        .iter()
        .enumerate()
        .map(|(row, _)| {
            typed
                .get(row)
                .map(|chord| chord.as_str())
                .filter(|chord| !chord.is_empty())
        })
        .collect();
    if bindings
        .iter()
        .zip(&chords)
        .all(|(binding, chord)| binding.proposed.as_deref() == *chord)
    {
        return Answer {
            choice: Some(BIND_AS_PROPOSED),
            ..Answer::default()
        };
    }
    Answer {
        uris: bindings
            .iter()
            .zip(chords)
            .filter_map(|(binding, chord)| {
                let chord = chord?;
                Some(if binding.proposed.as_deref() == Some(chord) {
                    binding.id.clone()
                } else {
                    format!("{}={chord}", binding.id)
                })
            })
            .collect(),
        ..Answer::default()
    }
}

/// What `question` shows the person and what its answer means, `retry` saying why the last
/// answer to it reached nothing; `None` for a kind nobody asks a person.
pub(crate) fn put(question: &Question, retry: Option<Retry>) -> Option<Put> {
    if question.kind == tilo_bluetooth_agent::KIND
        && let Some(shown) = code_on_show(question.details.clone())
    {
        return Some(Put::Shown(shown));
    }
    let who = who(question);
    let details = question.details.clone();
    let by = AskedBy::Program {
        name: who.clone(),
        app_id: question.app_id.clone(),
    };
    let (mut facts, reply) = match question.kind.as_str() {
        tilo_portal::consent::ACCESS_QUESTION => access(&who, details)?,
        tilo_portal::account::ACCOUNT_QUESTION => account(&who, details)?,
        tilo_portal::email::EMAIL_QUESTION => email(&who, details)?,
        tilo_portal::background::BACKGROUND_QUESTION => background(&who, details)?,
        tilo_portal::wallpaper::WALLPAPER_QUESTION => wallpaper(&who, details)?,
        tilo_portal::print::PRINT_QUESTION => printing(&who, details)?,
        tilo_portal::screenshot::SCREENSHOT_QUESTION => screenshot(&who, details)?,
        tilo_portal::usb::USB_QUESTION => usb(&who, details)?,
        tilo_portal::app_chooser::APP_CHOOSER_QUESTION => app_chooser(&who, details)?,
        tilo_portal::dynamic_launcher::LAUNCHER_QUESTION => launcher(&who, details)?,
        tilo_portal::global_shortcuts::SHORTCUTS_QUESTION => shortcuts(&who, details)?,
        tilo_portal::screencast::SCREENCAST_QUESTION => share(&who, details, false)?,
        tilo_portal::remote_desktop::REMOTE_DESKTOP_QUESTION => share(&who, details, true)?,
        tilo_portal::file_chooser::FILE_CHOOSER_QUESTION => files(&who, details)?,
        tilo_polkit_agent::questions::KIND => authentication(details)?,
        NETWORK_KIND => network_secrets(details)?,
        CANNOT_JOIN_KIND => cannot_join(details)?,
        tilo_bluetooth_agent::KIND => pairing(details)?,
        crate::mcp_server::KIND => admitted(&who, details)?,
        crate::network::DEVICE_UNLOCK => device_unlock(details)?,
        crate::network::DEVICE_ENROL => device_enrol(details)?,
        crate::network_reach::KIND => reach(&who, details)?,
        crate::step_questions::KIND => step(details)?,
        crate::step_questions::AGENT_QUESTION => agent_words(details)?,
        #[cfg(target_os = "linux")]
        tilo_workspace_service::apps::nearby_send::VERIFY_QUESTION => receiver(details)?,
        _ => return None,
    };
    if matches!(facts.by, AskedBy::Program { .. }) || facts.by == AskedBy::default() {
        facts.by = by;
    }
    if retry.is_some() {
        facts.retry = retry;
    }
    Some(Put::Ask(Raised {
        facts,
        reply,
        grant: grant_of(question),
    }))
}

/// The allowance `question` leaves when the person may keep their answer.
fn grant_of(question: &Question) -> Option<Grant> {
    if question.kind != tilo_portal::consent::ACCESS_QUESTION {
        return None;
    }
    let asked: AccessQuestion = serde_json::from_value(question.details.clone()).ok()?;
    let device = question
        .details
        .get(tilo_portal::consent::DEVICE_DETAIL)
        .and_then(Value::as_str)?
        .to_owned();
    asked.may_remember.then(|| Grant {
        app_id: question.app_id.clone().unwrap_or_default(),
        app_label: who(question),
        capability: asked.capability.clone(),
        device,
    })
}

/// Who asks, as the person reads it: the app's own name where this device knows the app, else the
/// name it gave.
fn who(question: &Question) -> String {
    match &question.app_id {
        Some(app_id) => claimant(app_id),
        None => question.label.clone(),
    }
}

/// A question that asks to allow what `words` says, `detail` beneath it.
fn asking(words: String, detail: Option<String>) -> AskedFacts {
    AskedFacts {
        words,
        facts: detail.into_iter().map(line).collect(),
        ..AskedFacts::default()
    }
}

/// A line the question shows under its words, with no label of its own.
fn line(value: String) -> Fact {
    Fact {
        label: String::new(),
        value,
    }
}

fn fact(label: String, value: String) -> Fact {
    Fact { label, value }
}

/// A field the person types into.
fn take(label: String, secret: bool) -> Take {
    Take {
        label,
        secret,
        shown: !secret,
        initial: None,
        chord: false,
    }
}

type Kind = Option<(AskedFacts, Reply)>;

/// The durations a permission question offers, read from the one list every prompt offers
/// ([`Lifetime::offered`]), as its choices. A question of buttons has no time to pick, so it
/// leaves out the timed one; one that outlasts the session is offered only where the answer may
/// be kept: the portal's own allowance, never a grant of the workspace's.
fn durations(may_keep: bool) -> (Vec<String>, Reply) {
    let (labels, choices) = Lifetime::offered(tilo_clock::unix_now_ms())
        .into_iter()
        .enumerate()
        .filter(|(_, lifetime)| !matches!(lifetime, Lifetime::Until(_)))
        .filter(|(_, lifetime)| may_keep || !lifetime.is_durable())
        .map(|(choice, lifetime)| (word(&format!("portal-allow-{}", lifetime.key())), choice))
        .unzip();
    (labels, Reply::Choose(choices))
}

/// The access question: what `capability` asks for, in the person's words, or in the app's own
/// where it worded a generic access question itself.
fn access(who: &str, details: Value) -> Kind {
    let asked: AccessQuestion = serde_json::from_value(details).ok()?;
    let mut facts = if let Some(text) = &asked.text {
        let mut facts = asking(text.title.clone(), None);
        facts.facts = [&text.subtitle, &text.body]
            .into_iter()
            .filter(|said| !said.is_empty())
            .cloned()
            .map(line)
            .collect();
        facts
    } else {
        let what = what(&asked.capability);
        asking(
            words("portal-access-title", &[("app", who), ("what", &what)]),
            Some(
                asked
                    .reason
                    .clone()
                    .unwrap_or_else(|| word("portal-access-detail")),
            ),
        )
    };
    let (choices, reply) = durations(asked.may_remember);
    facts.choices = choices;
    Some((facts, reply))
}

/// What `capability` is, in the person's words.
fn what(capability: &str) -> String {
    match capability {
        "screenshot" | "screencast" | "remote-desktop" | "camera" | "microphone" | "speaker"
        | "location" | "print" | "usb" | "input-capture" | "global-shortcuts" | "notification"
        | "secret" | "host-shell" => word(&format!("portal-access-{capability}")),
        _ => words("portal-access-generic", &[("capability", capability)]),
    }
}

/// An app the person installed, before it first starts in a workspace: whether it may reach what
/// its manifest declares.
fn reach(who: &str, details: Value) -> Kind {
    let asked: crate::network_reach::ReachQuestion = serde_json::from_value(details).ok()?;
    let any = asked.hosts.iter().any(|host| host == "*");
    let mut facts = asking(
        words(
            "portal-access-title",
            &[("app", who), ("what", &word("portal-access-network"))],
        ),
        Some(word(if any {
            "portal-network-any"
        } else {
            "portal-network-hosts"
        })),
    );
    if !any {
        let site = word("portal-network-site");
        facts
            .facts
            .extend(asked.hosts.into_iter().map(|host| fact(site.clone(), host)));
    }
    Some((facts, Reply::Confirm))
}

/// A program of this login wants to work in a workspace as an agent.
fn admitted(who: &str, details: Value) -> Kind {
    let asked: crate::mcp_server::Admission = serde_json::from_value(details).ok()?;
    let mut facts = asking(
        words(
            "mcp-server-admission",
            &[("workspace", &asked.workspace), ("agent", &asked.agent)],
        ),
        Some(words(
            "mcp-server-admission-detail",
            &[("agent", &asked.agent)],
        )),
    );
    facts.by = AskedBy::Program {
        name: who.to_owned(),
        app_id: None,
    };
    let (choices, reply) = durations(true);
    facts.choices = choices;
    Some((facts, reply))
}

/// What an agent's run asks before a step its grants leave to the person, or an app before it
/// takes another app's action: the step, what it would do field by field, what it risks, and
/// whether to allow it once or every such step from now on.
fn step(details: Value) -> Kind {
    use tilo_action_policy::ActionKind;
    let asked: crate::step_questions::StepQuestion = serde_json::from_value(details).ok()?;
    // An app asking to take another app's action is named as the person knows both apps; a
    // label that asks for more ("File a note…") is the step itself, its fields below it.
    let (title, detail, always, by) = match &asked.target {
        Some(target) => {
            let key = match asked.kind {
                ActionKind::Read => "app-question-title-read",
                ActionKind::Undoable | ActionKind::Outside => "app-question-title-change",
            };
            let app = crate::app_names::app_name(&asked.agent);
            (
                words(
                    key,
                    &[
                        ("app", &app),
                        ("target", &crate::app_names::app_name(target)),
                    ],
                ),
                asked.action.trim_end_matches('…').to_owned(),
                word("portal-allow-always"),
                AskedBy::Program {
                    name: app,
                    app_id: Some(asked.agent.clone()),
                },
            )
        }
        None => (
            words(
                "step-question-title",
                &[("agent", &asked.agent), ("action", &asked.action)],
            ),
            words("step-question-detail", &[("resource", &asked.resource)]),
            words("step-question-allow-agent", &[("agent", &asked.agent)]),
            AskedBy::Agent(tilo_actions::remote::asked::Named {
                id: asked.agent.clone(),
                name: asked.agent.clone(),
            }),
        ),
    };
    let mut facts = asking(title, Some(detail));
    facts.facts.extend(
        asked
            .fields
            .into_iter()
            .map(|(label, value)| fact(label, value)),
    );
    if !matches!(asked.risk.as_str(), "" | "none" | "low") {
        facts.facts.push(line(words(
            "network-agent-question-risk",
            &[("risk", &risk_words(&asked.risk))],
        )));
    }
    facts.destructive = asked.risk == "destructive";
    facts.by = by;
    let mut offered = vec![Lifetime::Once];
    facts.choices = vec![word("portal-allow-once")];
    if asked.may_allow_agent {
        offered.push(Lifetime::UntilRevoked);
        facts.choices.push(always);
    }
    Some((
        facts,
        Reply::Choose(offered.into_iter().map(Lifetime::choice).collect()),
    ))
}

/// What an action risks, in the person's words: an unknown name reads as the gravest.
fn risk_words(name: &str) -> String {
    match name {
        "none" | "low" | "destructive" | "privileged" | "sensitive" => {
            word(&format!("network-agent-risk-{name}"))
        }
        _ => word("network-agent-risk-something"),
    }
}

/// An agent's question in its own words: a decision or something only the person knows,
/// answered with one of its choices.
fn agent_words(details: Value) -> Kind {
    let asked: crate::step_questions::AgentQuestion = serde_json::from_value(details).ok()?;
    let mut facts = asking(asked.question, None);
    if !asked.workspace.is_empty() {
        facts
            .facts
            .push(fact(word("agent-ask-workspace"), asked.workspace));
    }
    facts.by = AskedBy::Agent(tilo_actions::remote::asked::Named {
        id: asked.agent.clone(),
        name: asked.agent,
    });
    let reply = Reply::Choose((0..asked.choices.len()).collect());
    facts.choices = asked.choices;
    Some((facts, reply))
}

/// A file handed to a receiver that is not one of the person's devices: the person types the
/// fingerprint of the receiver's certificate, read off another channel than the one that carries
/// the file.
#[cfg(target_os = "linux")]
fn receiver(details: Value) -> Kind {
    use ui::i18n::ArgValue;
    let asked: tilo_workspace_service::apps::nearby_send::VerifyQuestion =
        serde_json::from_value(details).ok()?;
    let mut facts = asking(
        crate::i18n::t("airdrop-verify-title"),
        Some(crate::i18n::t_args(
            "airdrop-verify-detail",
            &[
                ("name", ArgValue::Text(asked.name)),
                ("bytes", ArgValue::Text(asked.bytes.to_string())),
                ("endpoint", ArgValue::Text(asked.endpoint)),
            ],
        )),
    );
    facts.takes = vec![take(crate::i18n::t("airdrop-verify-fingerprint"), false)];
    Some((facts, Reply::Fingerprint(asked.certificate)))
}

/// Another of the person's devices asks for its disk-unlock half: the code on its boot screen is
/// shown beside the one the person answers with.
fn device_unlock(details: Value) -> Kind {
    let asked: crate::network::UnlockQuestion = serde_json::from_value(details).ok()?;
    let mut facts = asking(
        words("network-device-unlock-title", &[("device", &asked.device)]),
        Some(words(
            "network-device-unlock-detail",
            &[("slot", &asked.slot)],
        )),
    );
    facts.facts.push(fact(
        word("network-device-unlock-code"),
        asked.approval_code,
    ));
    facts.by = device(&asked.device);
    Some((facts, Reply::Confirm))
}

/// Another of the person's devices asks this one to sign the key it unlocks its disk with.
fn device_enrol(details: Value) -> Kind {
    let asked: crate::network::EnrolQuestion = serde_json::from_value(details).ok()?;
    let mut facts = asking(
        words("network-device-enrol-title", &[("device", &asked.device)]),
        Some(word("network-device-enrol-detail")),
    );
    if asked.unattended {
        facts
            .facts
            .push(line(word("network-device-enrol-unattended")));
        facts.destructive = true;
    }
    facts.by = device(&asked.device);
    Some((facts, Reply::Confirm))
}

/// Another of the person's devices, by the name it goes by.
fn device(name: &str) -> AskedBy {
    AskedBy::Device(tilo_actions::remote::asked::Named {
        id: name.to_owned(),
        name: name.to_owned(),
    })
}

fn account(who: &str, details: Value) -> Kind {
    let asked: AccountQuestion = serde_json::from_value(details).ok()?;
    let detail = asked
        .reason
        .unwrap_or_else(|| words("portal-account-detail", &[("app", who)]));
    Some((
        asking(words("portal-account-title", &[("app", who)]), Some(detail)),
        Reply::Confirm,
    ))
}

fn email(who: &str, details: Value) -> Kind {
    let asked: EmailQuestion = serde_json::from_value(details).ok()?;
    let mut facts = asking(
        words("portal-email-title", &[("app", who)]),
        Some(word("portal-email-detail")),
    );
    let mut list = |key: &str, addresses: &[String]| {
        if !addresses.is_empty() {
            facts
                .facts
                .push(line(words(key, &[("addresses", &addresses.join(", "))])));
        }
    };
    list("portal-email-to", &asked.addresses);
    list("portal-email-cc", &asked.cc);
    list("portal-email-bcc", &asked.bcc);
    if let Some(subject) = &asked.subject {
        facts
            .facts
            .push(line(words("portal-email-subject", &[("subject", subject)])));
    }
    if !asked.attachments.is_empty() {
        facts.facts.push(line(words(
            "portal-email-attachments",
            &[("count", &asked.attachments.len().to_string())],
        )));
    }
    Some((facts, Reply::Confirm))
}

fn background(who: &str, details: Value) -> Kind {
    let asked: BackgroundQuestion = serde_json::from_value(details).ok()?;
    let mut facts = asking(
        words("portal-background-title", &[("app", who)]),
        Some(asked.name),
    );
    facts.choices = vec![
        word("portal-background-background"),
        word("portal-background-running"),
        word("portal-background-active"),
    ];
    let rows = [BACKGROUND, RUNNING, ACTIVE].map(|result| result as usize);
    Some((facts, Reply::Choose(rows.to_vec())))
}

fn wallpaper(who: &str, details: Value) -> Kind {
    let asked: WallpaperQuestion = serde_json::from_value(details).ok()?;
    let where_ = word(match asked.set_on.as_str() {
        "lockscreen" => "portal-wallpaper-lockscreen",
        "both" => "portal-wallpaper-both",
        _ => "portal-wallpaper-background",
    });
    Some((
        asking(
            words("portal-wallpaper-title", &[("app", who)]),
            Some(words(
                "portal-wallpaper-detail",
                &[("app", who), ("where", &where_)],
            )),
        ),
        Reply::Confirm,
    ))
}

fn printing(who: &str, details: Value) -> Kind {
    let asked: PrintQuestion = serde_json::from_value(details).ok()?;
    let copies = ui::i18n::ArgValue::Number(f64::from(asked.copies));
    let mut facts = asking(
        words(
            "portal-print-title",
            &[("app", who), ("title", &asked.title)],
        ),
        Some(tilo_sheets::words_with(
            "portal-print-detail",
            &[("copies", copies)],
        )),
    );
    // The printer the app suggests stands first.
    let first = asked.selected.min(asked.printers.len().saturating_sub(1));
    let mut order: Vec<usize> = (0..asked.printers.len()).collect();
    if first < order.len() {
        order.remove(first);
        order.insert(0, first);
    }
    facts.choices = order
        .iter()
        .filter_map(|at| asked.printers.get(*at).cloned())
        .collect();
    Some((facts, Reply::Choose(order)))
}

fn screenshot(who: &str, details: Value) -> Kind {
    let _asked: ScreenshotQuestion = serde_json::from_value(details).ok()?;
    Some((
        asking(
            words("portal-screenshot-title", &[("app", who)]),
            Some(word("portal-screenshot-detail")),
        ),
        Reply::Confirm,
    ))
}

fn usb(who: &str, details: Value) -> Kind {
    let asked: UsbQuestion = serde_json::from_value(details).ok()?;
    let mut facts = asking(
        words("portal-usb-title", &[("app", who)]),
        Some(words("portal-usb-detail", &[("app", who)])),
    );
    facts.many = true;
    facts.choices = asked
        .devices
        .iter()
        .map(|device| {
            let label = device.label.clone().unwrap_or_else(|| device.id.clone());
            if device.writable {
                words("portal-usb-writable", &[("device", &label)])
            } else {
                label
            }
        })
        .collect();
    let names = asked.devices.into_iter().map(|device| device.id).collect();
    Some((facts, Reply::Rows(names)))
}

fn app_chooser(who: &str, details: Value) -> Kind {
    let asked: AppChooserQuestion = serde_json::from_value(details).ok()?;
    let what = asked
        .filename
        .clone()
        .or_else(|| asked.uri.clone())
        .unwrap_or_default();
    let mut facts = asking(
        word("portal-app-choice-title"),
        Some(if what.is_empty() {
            word("portal-app-choice-detail-any")
        } else {
            words("portal-app-choice-detail", &[("app", who), ("what", &what)])
        }),
    );
    // The app the person chose last time stands first.
    let last = asked
        .last_choice
        .as_ref()
        .and_then(|last| asked.choices.iter().position(|app| &app.id == last));
    let mut order: Vec<usize> = (0..asked.choices.len()).collect();
    if let Some(last) = last {
        order.remove(last);
        order.insert(0, last);
    }
    facts.choices = order
        .iter()
        .filter_map(|at| asked.choices.get(*at).map(|app| app.name.clone()))
        .collect();
    Some((facts, Reply::Choose(order)))
}

fn launcher(who: &str, details: Value) -> Kind {
    let asked: LauncherQuestion = serde_json::from_value(details).ok()?;
    let mut facts = asking(
        words(
            "portal-launcher-title",
            &[("app", who), ("name", &asked.name)],
        ),
        Some(word("portal-launcher-detail")),
    );
    if asked.editable_name {
        facts.takes = vec![Take {
            initial: Some(asked.name.clone()),
            ..take(word("portal-launcher-name"), false)
        }];
    }
    if asked.editable_icon {
        facts.choices = vec![
            word("portal-launcher-app-icon"),
            word("portal-launcher-monogram-icon"),
            word("portal-launcher-web-icon"),
        ];
    }
    let icons = facts.choices.len();
    Some((
        facts,
        Reply::Launcher {
            name: asked.editable_name,
            icons,
        },
    ))
}

fn shortcuts(who: &str, details: Value) -> Kind {
    let asked: ShortcutsQuestion = serde_json::from_value(details).ok()?;
    let mut facts = asking(
        words("portal-shortcuts-title", &[("app", who)]),
        Some(word("portal-shortcuts-detail")),
    );
    let mut bindings = Vec::new();
    for row in &asked.shortcuts {
        let name = row.description.clone().unwrap_or_else(|| row.id.clone());
        match (row.status.as_deref(), &row.taken_by) {
            (Some("reserved"), _) => facts
                .facts
                .push(fact(name.clone(), word("portal-shortcut-reserved"))),
            (Some("taken"), Some(app)) => facts.facts.push(fact(
                name.clone(),
                words("portal-shortcut-taken", &[("app", &claimant(app))]),
            )),
            _ => {}
        }
        facts.takes.push(Take {
            initial: row
                .trigger
                .as_deref()
                .and_then(Trigger::parse)
                .map(|_| row.trigger.clone().unwrap_or_default()),
            chord: true,
            ..take(name, false)
        });
        bindings.push(Binding {
            id: row.id.clone(),
            proposed: row.trigger.clone(),
        });
    }
    Some((facts, Reply::Bind(bindings)))
}

fn share(who: &str, details: Value, remote: bool) -> Kind {
    let asked: ShareQuestion = serde_json::from_value(details).ok()?;
    let mut facts = asking(
        words(
            if remote {
                "portal-remote-title"
            } else {
                "portal-share-title"
            },
            &[("app", who)],
        ),
        Some(words(
            if remote {
                "portal-remote-detail"
            } else {
                "portal-share-detail"
            },
            &[("app", who)],
        )),
    );
    if !asked.devices.is_empty() {
        facts.facts.push(line(words(
            "portal-remote-devices",
            &[("devices", &asked.devices.join(", "))],
        )));
    }
    facts.choices = asked.sources.iter().map(source_label).collect();
    let names = asked
        .sources
        .into_iter()
        .map(|source| source.choice)
        .collect();
    // Several sources are picked together only where the app asked for several.
    facts.many = asked.multiple;
    Some((
        facts,
        if asked.multiple {
            Reply::Rows(names)
        } else {
            Reply::Pick(names)
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

/// The file chooser: the files or the place, picked on the device that asked.
fn files(who: &str, details: Value) -> Kind {
    let asked: FileChooserQuestion = serde_json::from_value(details).ok()?;
    let title = if asked.title.is_empty() {
        words("portal-files-title", &[("app", who)])
    } else {
        asked.title.clone()
    };
    let mut facts = asking(title, None);
    if asked.writable_directory {
        facts
            .facts
            .push(line(words("portal-files-writable-folder", &[("app", who)])));
    }
    facts.files = Some(FilesAsked {
        mode: if asked.directory {
            FilesMode::Folder
        } else if asked.save {
            FilesMode::Save
        } else {
            FilesMode::Open
        },
        many: asked.multiple,
        types: asked
            .filters
            .iter()
            .map(|filter| FileType {
                label: filter.label.clone(),
                patterns: filter
                    .extensions
                    .iter()
                    .map(|extension| format!("*.{extension}"))
                    .collect(),
            })
            .collect(),
        name: asked.current_name.clone(),
        folder: tilo_paths::home().map(|home| home.display().to_string()),
    });
    Some((facts, Reply::Files))
}

/// The administrator's password, which reaches polkit's helper and nothing else.
fn authentication(details: Value) -> Kind {
    let asked: AuthenticateDetails = serde_json::from_value(details).ok()?;
    let mut facts = asking(asked.message.clone(), Some(word("portal-auth-heading")));
    facts
        .facts
        .push(fact(word("portal-auth-action"), asked.action_id.clone()));
    facts.by = AskedBy::Program {
        name: word("portal-auth-heading"),
        app_id: None,
    };
    facts.retry = asked.notice.as_ref().map(|notice| match notice {
        Notice::Message { key } => word(key),
        Notice::Verbatim(text) => text.clone(),
    });
    facts.takes = vec![Take {
        shown: asked.echo,
        ..take(asked.prompt.clone(), true)
    }];
    // More than one identity may authorise it: picking one is the choice, the one asked for first.
    let mut order: Vec<usize> = (0..asked.identities.len()).collect();
    if asked.identity < order.len() {
        order.remove(asked.identity);
        order.insert(0, asked.identity);
    }
    if asked.identities.len() > 1 {
        facts.choices = order
            .iter()
            .filter_map(|at| asked.identities.get(*at).cloned())
            .collect();
    }
    Some((facts, Reply::Password { order }))
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

/// A network's secrets: the fields it asks for, typed into the card and handed to the network
/// agent alone.
fn network_secrets(details: Value) -> Kind {
    let asked: SecretsDetails = serde_json::from_value(details).ok()?;
    let retry_line = word("portal-network-retry");
    let mut retried = false;
    let detail: Vec<String> = asked
        .detail
        .iter()
        .map(say)
        .filter(|line| {
            retried |= *line == retry_line;
            *line != retry_line
        })
        .collect();
    let mut facts = asking(say(&asked.title), None);
    facts.facts = detail.into_iter().map(line).collect();
    if let Some(network) = &asked.network {
        facts.on = Some(network.clone());
    }
    facts.by = AskedBy::Program {
        name: word("portal-network-asker"),
        app_id: None,
    };
    let refused = asked.refusal.as_ref().map(|refusal| word(&refusal.key));
    let Some(sim) = &asked.sim else {
        facts.takes = asked
            .fields
            .iter()
            .map(|field| Take {
                label: say(&field.label),
                // Everything a network asks for is its secret: the user name of a sign-in
                // included, which is kept by `NetworkManager`, never by the record.
                secret: true,
                shown: !field.masked,
                initial: None,
                chord: false,
            })
            .collect();
        facts.retry = refused.or_else(|| retried.then_some(retry_line));
        if let Some(rule) = asked
            .fields
            .iter()
            .find(|field| field.rule == FieldRule::WpaPassphrase)
            .map(|_| word("portal-secret-wifi-length"))
        {
            facts.facts.push(line(rule));
        }
        let remember = asked.remember.is_some();
        if remember {
            facts.choices = vec![word("portal-connect"), word("portal-remember-device")];
        }
        return Some((facts, Reply::Secrets { remember }));
    };
    if let Some(left) = sim.attempts_left {
        facts.facts.push(line(words(
            "portal-sim-attempts-left",
            &[("count", &left.to_string())],
        )));
    }
    match sim.step {
        SimUnlockStep::Pin => {
            facts.takes = vec![take(
                asked
                    .fields
                    .first()
                    .map(|field| say(&field.label))
                    .unwrap_or_else(|| word("portal-network-sim-pin")),
                true,
            )];
            facts.retry = refused.or_else(|| retried.then(|| word("portal-sim-pin-wrong")));
            Some((facts, Reply::Secrets { remember: false }))
        }
        // The new PIN is asked twice so a typo does not lock the SIM again.
        SimUnlockStep::Puk => {
            facts.takes = vec![
                take(word("portal-sim-puk"), true),
                take(word("portal-sim-new-pin"), true),
                take(word("portal-sim-new-pin-again"), true),
            ];
            facts.retry = refused;
            Some((facts, Reply::Puk))
        }
    }
}

/// A network the person may join, which asks for its password next.
fn cannot_join(details: Value) -> Kind {
    let asked: CannotJoinDetails = serde_json::from_value(details).ok()?;
    let mut facts = asking(say(&asked.title), Some(say(&asked.body)));
    facts.choices = vec![say(&asked.join)];
    facts.by = AskedBy::Program {
        name: word("portal-network-asker"),
        app_id: None,
    };
    Some((facts, Reply::Choose(vec![0])))
}

/// A device pairing, as the device's method asks it.
fn pairing(details: Value) -> Kind {
    let asked: PairingDetails = serde_json::from_value(details).ok()?;
    let device = asked.device.clone();
    let explain = |key: &str| words(key, &[("device", &device)]);
    let title = |key: &str| words(key, &[("device", &device)]);
    let mut code = |facts: &mut AskedFacts| {
        if let Some(code) = &asked.code {
            facts
                .facts
                .push(fact(word("bluetooth-prompt-code"), code.clone()));
        }
    };
    let (mut facts, reply) = match asked.method {
        PairingMethod::ConfirmPasskey => {
            let mut facts = asking(
                title("bluetooth-prompt-title-confirm-passkey"),
                Some(explain("bluetooth-prompt-explain-confirm-passkey")),
            );
            code(&mut facts);
            (facts, Reply::Agree)
        }
        PairingMethod::ConfirmPairing => (
            asking(
                title("bluetooth-prompt-title-confirm-pairing"),
                Some(explain("bluetooth-prompt-explain-confirm-pairing")),
            ),
            Reply::Agree,
        ),
        PairingMethod::AuthorizeService => {
            let mut facts = asking(
                title("bluetooth-prompt-title-authorize-service"),
                Some(explain("bluetooth-prompt-explain-authorize-service")),
            );
            if let Some(service) = asked
                .service_key
                .as_deref()
                .map(word)
                .or_else(|| asked.service.clone())
            {
                facts.facts.push(line(words(
                    "bluetooth-prompt-service",
                    &[("service", &service)],
                )));
            }
            (facts, Reply::Agree)
        }
        PairingMethod::EnterPasskey | PairingMethod::EnterPin => {
            let (key, secret) = if asked.method == PairingMethod::EnterPasskey {
                ("bluetooth-prompt-title-request-passkey", false)
            } else {
                ("bluetooth-prompt-title-request-pin", true)
            };
            let mut facts = asking(
                title(key),
                Some(explain("bluetooth-prompt-explain-request")),
            );
            facts.takes = vec![take(word("bluetooth-prompt-code"), secret)];
            (facts, Reply::Entry)
        }
        // A code on show asks nothing: it is a notice ([`code_on_show`]).
        PairingMethod::DisplayPasskey | PairingMethod::DisplayPin => return None,
    };
    facts.by = AskedBy::Device(tilo_actions::remote::asked::Named {
        id: asked.device_path,
        name: device.clone(),
    });
    Some((facts, reply))
}

/// A pairing that shows a code to type on the other device: the notice saying it, which the
/// person stops the pairing from. `None` for every other pairing.
fn code_on_show(details: Value) -> Option<Shown> {
    let asked: PairingDetails = serde_json::from_value(details).ok()?;
    if !matches!(
        asked.method,
        PairingMethod::DisplayPasskey | PairingMethod::DisplayPin
    ) {
        return None;
    }
    let device = asked.device.as_str();
    let mut body = vec![words(
        "bluetooth-prompt-explain-display",
        &[("device", device)],
    )];
    if let Some(code) = &asked.code {
        body.push(code.clone());
    }
    if let Some(entered) = asked.entered {
        body.push(words(
            "bluetooth-prompt-digits-entered",
            &[("entered", &entered.to_string())],
        ));
    }
    Some(Shown {
        from: device.to_owned(),
        title: words("bluetooth-prompt-title-display", &[("device", device)]),
        body: body.join("\n"),
        stop: word("bluetooth-prompt-cancel"),
    })
}

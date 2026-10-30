//! The Agents tab: the agents admitted to the workspace, each with what it
//! may do and the way to review or take that back, and the workspace's
//! bots apart from them.
//!
//! Reviewing an agent shows, in the tab's place, what it may do app by
//! app: Allow, Ask first or Deny for reading, changes and outside effects,
//! the model it plans with, and the actions always allowed without asking.

use ui::icons::Icon;
use ui::tokens::Rhythm;
use ui::widget::{
    ButtonRole, ChipStyle, Column, ControlSize, Element, EmptyState, ListRow, Row, Section,
    Segment, ToggleGroup, Tone,
};

use super::general::{button, buttons, caption};
use super::{Parts, Settings, WorkspaceSettingsEffect};

/// How a tab introduces itself: what it holds, what it is for, and whether
/// the person may change it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Intro {
    pub title: String,
    pub help: String,
    /// "You manage" or "View only".
    pub authority: String,
}

/// One admitted agent, in the person's words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Agent {
    /// What it is asked for by.
    pub key: String,
    pub name: String,
    /// Whether it is an agent or a service the workspace admitted.
    pub badge: String,
    /// What it was admitted with and holds besides.
    pub detail: String,
    /// "Admitted".
    pub admitted: String,
}

/// One of the workspace's bots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bot {
    pub name: String,
    /// Its entry in the bot directory, for a bot a member asked the
    /// workspace's host for: what Remove takes back. Otherwise the room's
    /// operator runs it and it is not removed here.
    pub entry: Option<String>,
    /// Whether it runs yet, and from which version.
    pub detail: String,
}

/// What an agent's answer for one kind of action is, at one app.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KindLine {
    /// read, change or outside.
    pub kind: String,
    pub label: String,
    /// Whether the answer was set here or follows the admission.
    pub origin: String,
    /// The answer, as an index into [`AgentReview::answers`].
    pub answer: usize,
}

/// One app an agent's answers are given for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessApp {
    /// What its answers are asked for by.
    pub scope: String,
    pub label: String,
    /// Whether any of its answers was set here, and what that means open.
    pub customised: bool,
    pub detail: String,
    pub kinds: Vec<KindLine>,
}

/// A model the agent may plan with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelChoice {
    /// What it is chosen by; empty for the default.
    pub key: String,
    pub label: String,
    pub in_use: bool,
}

/// The models the person may choose for the agent, from their Settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Models {
    pub title: String,
    /// What it plans with now.
    pub summary: String,
    pub choices: Vec<ModelChoice>,
}

/// An action the person always allows the agent, without asking first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exception {
    pub scope: String,
    pub kind: String,
    pub label: String,
}

/// What one agent may do, as its review shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentReview {
    pub agent: String,
    pub name: String,
    pub badge: String,
    pub back: String,
    pub note: String,
    /// Whether the person may change its answers.
    pub editable: bool,
    /// The three answers' names: Allow, Ask first, Deny.
    pub answers: [String; 3],
    pub reset: String,
    pub custom: String,
    pub apps: Vec<AccessApp>,
    /// What the review says when there is no app to answer for.
    pub empty: String,
    pub models: Option<Models>,
    pub use_model: String,
    pub in_use: String,
    pub exceptions_title: String,
    pub exceptions: Vec<Exception>,
    pub stop: String,
}

/// Everything the Agents tab shows, in the person's words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Agents {
    pub intro: Intro,
    pub add: String,
    /// Whether the person may admit, review the controls of, and remove
    /// agents and bots.
    pub manage: bool,
    pub header: String,
    pub agents: Vec<Agent>,
    pub review_label: String,
    pub remove_label: String,
    /// What the tab says with neither agents nor bots: title and detail.
    pub empty: (String, String),
    pub bots_header: String,
    pub add_bot: String,
    pub bot_tag: String,
    pub remove_bot: String,
    pub bots: Vec<Bot>,
    pub bots_empty: String,
    /// Why the controls do not take input, for someone who may not use them.
    pub footnote: Option<String>,
    /// The agent being reviewed, shown in the roster's place.
    pub review: Option<AgentReview>,
}

/// The answers by key, in segment order.
pub(super) const ANSWERS: [&str; 3] = ["allow", "ask", "deny"];

/// The Agents tab of `settings`.
pub(super) fn pane(settings: &Settings, parts: &Parts) -> Element {
    let agents = &settings.agents;
    if let Some(review) = &agents.review {
        return reviewed(review, parts);
    }
    let mut groups: Vec<Element> = vec![intro(&agents.intro, Icon::Sparkles)];
    groups.push(buttons(vec![
        button(parts, "ws-agents-add", &agents.add, || {
            WorkspaceSettingsEffect::AddAgent
        })
        .role(ButtonRole::Primary)
        .with_enabled(agents.manage),
    ]));
    if agents.agents.is_empty() && agents.bots.is_empty() {
        let (title, detail) = &agents.empty;
        groups.push(Box::new(
            EmptyState::new(title.clone())
                .with_icon(Icon::Sparkles)
                .with_message(detail.clone())
                .inline(),
        ));
    } else {
        if !agents.agents.is_empty() {
            groups.push(Box::new(Section::titled(
                agents.header.clone(),
                agents
                    .agents
                    .iter()
                    .map(|agent| agent_row(agents, agent, parts))
                    .collect(),
            )));
        }
        groups.push(bots(agents, parts));
    }
    if let Some(note) = &agents.footnote {
        groups.push(caption(note));
    }
    column(groups)
}

pub(super) fn column(groups: Vec<Element>) -> Element {
    Box::new(
        groups.into_iter().fold(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_rhythm(Rhythm::Section),
            Column::push,
        ),
    )
}

/// A tab's introduction: its icon, what it holds and is for, and whether
/// the person manages it.
pub(super) fn intro(intro: &Intro, icon: Icon) -> Element {
    Box::new(
        ListRow::new("ws-intro", intro.title.clone())
            .icon(icon)
            .secondary(intro.help.clone())
            .two_line_secondary()
            .tags(&[(intro.authority.as_str(), ChipStyle::Neutral)]),
    )
}

/// One agent: its mark, name and what it holds, its kind, Admitted, Review
/// access and Revoke.
fn agent_row(agents: &Agents, agent: &Agent, parts: &Parts) -> Element {
    let (review, remove) = (agent.key.clone(), agent.key.clone());
    let controls = buttons(vec![
        // Anyone in the workspace may see what an agent may do.
        button(
            parts,
            &format!("ws-agents-review-{}", agent.key),
            &agents.review_label,
            move || WorkspaceSettingsEffect::ReviewAgent {
                agent: review.clone(),
            },
        )
        .control_size(ControlSize::Small),
        button(
            parts,
            &format!("ws-agents-remove-{}", agent.key),
            &agents.remove_label,
            move || WorkspaceSettingsEffect::RemoveAgent {
                agent: remove.clone(),
            },
        )
        .role(ButtonRole::Destructive)
        .control_size(ControlSize::Small)
        .with_enabled(agents.manage),
    ]);
    Box::new(
        ListRow::new(format!("ws-agents-agent-{}", agent.key), agent.name.clone())
            .two_line_title()
            .avatar(agent.name.clone(), agent.key.clone())
            .secondary(agent.detail.clone())
            .tags(&[
                (agent.badge.as_str(), ChipStyle::Neutral),
                (agent.admitted.as_str(), ChipStyle::Tinted(Tone::Success)),
            ])
            .trailing(controls),
    )
}

/// The workspace's bots: a header with Add a bot, then a row for each.
fn bots(agents: &Agents, parts: &Parts) -> Element {
    let mut items: Vec<Element> = vec![buttons(vec![
        button(parts, "ws-bots-add", &agents.add_bot, || {
            WorkspaceSettingsEffect::AddBot
        })
        .with_enabled(agents.manage),
    ])];
    if agents.bots.is_empty() {
        items.push(caption(&agents.bots_empty));
    }
    for bot in &agents.bots {
        let mut row = ListRow::new(format!("ws-bots-bot-{}", bot.name), bot.name.clone())
            .avatar(bot.name.clone(), bot.name.clone())
            .secondary(bot.detail.clone())
            .tags(&[(agents.bot_tag.as_str(), ChipStyle::Neutral)]);
        if let Some(entry) = &bot.entry {
            let bot_entry = entry.clone();
            row = row.trailing(Box::new(
                button(
                    parts,
                    &format!("ws-bots-remove-{entry}"),
                    &agents.remove_bot,
                    move || WorkspaceSettingsEffect::RemoveBot {
                        bot: bot_entry.clone(),
                    },
                )
                .role(ButtonRole::Destructive)
                .control_size(ControlSize::Small)
                .with_enabled(agents.manage),
            ));
        }
        items.push(Box::new(row));
    }
    Box::new(Section::titled(agents.bots_header.clone(), items))
}

/// One agent's review in the roster's place: Back, then each app's
/// answers, its model and its standing exceptions.
fn reviewed(review: &AgentReview, parts: &Parts) -> Element {
    let local = parts.local.get();
    let mut groups: Vec<Element> = vec![
        Box::new(
            Row::new()
                .with_rhythm(Rhythm::Related)
                .align_start()
                .push(Box::new(
                    button(parts, "ws-agent-back", &review.back, || {
                        WorkspaceSettingsEffect::EndReview
                    })
                    .with_icon(Icon::ChevronLeft),
                )),
        ),
        Box::new(
            ListRow::new("ws-agent-name", review.name.clone())
                .avatar(review.name.clone(), review.agent.clone())
                .secondary(review.note.clone())
                .two_line_secondary()
                .tags(&[(review.badge.as_str(), ChipStyle::Neutral)]),
        ),
    ];
    if review.apps.is_empty() {
        groups.push(caption(&review.empty));
    }
    for app in &review.apps {
        let open = local.open_app.as_deref() == Some(app.scope.as_str());
        groups.push(access_app(review, app, open, parts));
    }
    if let Some(models) = &review.models {
        groups.push(models_section(review, models, local.models_open, parts));
    }
    if !review.exceptions.is_empty() {
        groups.push(Box::new(Section::titled(
            review.exceptions_title.clone(),
            review
                .exceptions
                .iter()
                .map(|exception| {
                    let (scope, kind) = (exception.scope.clone(), exception.kind.clone());
                    let agent = review.agent.clone();
                    Box::new(
                        ListRow::new(
                            format!("ws-agent-exception-{}-{}", exception.scope, exception.kind),
                            exception.label.clone(),
                        )
                        .trailing(Box::new(
                            button(
                                parts,
                                &format!("ws-agent-stop-{}-{}", exception.scope, exception.kind),
                                &review.stop,
                                move || WorkspaceSettingsEffect::SetAgentAnswer {
                                    agent: agent.clone(),
                                    scope: scope.clone(),
                                    kind: kind.clone(),
                                    answer: String::new(),
                                },
                            )
                            .control_size(ControlSize::Small)
                            .with_enabled(review.editable),
                        )),
                    ) as Element
                })
                .collect(),
        )));
    }
    column(groups)
}

/// One app: its name, and its answers as one tag each while it is closed,
/// which opens to an Allow / Ask first / Deny control for each kind of
/// action, with Reset once any of them was set here.
fn access_app(review: &AgentReview, app: &AccessApp, open: bool, parts: &Parts) -> Element {
    let toggle = parts.clone();
    let scope = app.scope.clone();
    let mut head = ListRow::new(format!("ws-agent-app-{}", app.scope), app.label.clone());
    if open {
        head = head.secondary(app.detail.clone());
    } else {
        let answers: Vec<String> = app
            .kinds
            .iter()
            .filter_map(|kind| {
                let answer = review.answers.get(kind.answer)?;
                Some(format!("{}: {answer}", kind.label))
            })
            .collect();
        let tags: Vec<(&str, ChipStyle)> = answers
            .iter()
            .map(|answer| (answer.as_str(), ChipStyle::Neutral))
            .collect();
        head = head.tags(&tags);
    }
    let mut head = head
        .disclosure()
        .selected(open)
        .on_effect(move || toggle.toggle_app(&scope));
    if app.customised && open {
        let (agent, scope) = (review.agent.clone(), app.scope.clone());
        head = head.trailing(Box::new(
            button(
                parts,
                &format!("ws-agent-reset-{}", app.scope),
                &review.reset,
                move || WorkspaceSettingsEffect::ResetAgentApp {
                    agent: agent.clone(),
                    scope: scope.clone(),
                },
            )
            .control_size(ControlSize::Small)
            .with_enabled(review.editable),
        ));
    } else if app.customised {
        head = head.tags(&[(review.custom.as_str(), ChipStyle::Tinted(Tone::Info))]);
    }
    if !open {
        return Box::new(head);
    }
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Related)
        .push(Box::new(head));
    for line in &app.kinds {
        column = column.push(kind_line(review, app, line, parts));
    }
    Box::new(column)
}

/// One kind of action at one app: its name, where its answer comes from,
/// and the answer, which the arrow keys move along.
fn kind_line(review: &AgentReview, app: &AccessApp, line: &KindLine, parts: &Parts) -> Element {
    let segments = review
        .answers
        .iter()
        .map(|answer| Segment::new(answer.clone()))
        .collect();
    let (press, agent, scope, kind) = (
        parts.clone(),
        review.agent.clone(),
        app.scope.clone(),
        line.kind.clone(),
    );
    let line_id = format!("ws-agent-answer-{}-{}", app.scope, line.kind);
    let answers = ToggleGroup::new(segments, line.answer)
        .with_accessible_name(format!("{} {}", app.label, line.label))
        .with_enabled(review.editable)
        .on_view(&line_id, move |index| {
            if let Some(answer) = ANSWERS.get(index) {
                press.ask(WorkspaceSettingsEffect::SetAgentAnswer {
                    agent: agent.clone(),
                    scope: scope.clone(),
                    kind: kind.clone(),
                    answer: (*answer).to_owned(),
                });
            }
        });
    Box::new(
        ListRow::new(
            format!("ws-agent-kind-{}-{}", app.scope, line.kind),
            line.label.clone(),
        )
        .nested()
        .secondary(line.origin.clone())
        .trailing(Box::new(answers)),
    )
}

/// The agent's model: what it plans with, which opens to the default and
/// every model offered, the one in use marked and each other with Use.
fn models_section(review: &AgentReview, models: &Models, open: bool, parts: &Parts) -> Element {
    let toggle = parts.clone();
    let head = ListRow::new("ws-agent-models", models.title.clone())
        .secondary(models.summary.clone())
        .disclosure()
        .selected(open)
        .on_effect(move || toggle.toggle_models());
    if !open {
        return Box::new(head);
    }
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Related)
        .push(Box::new(head));
    for choice in &models.choices {
        let mut row = ListRow::new(
            format!("ws-agent-model-{}", choice.key),
            choice.label.clone(),
        )
        .nested();
        row = if choice.in_use {
            row.tags(&[(review.in_use.as_str(), ChipStyle::Tinted(Tone::Success))])
        } else {
            let (agent, model) = (review.agent.clone(), choice.key.clone());
            row.trailing(Box::new(
                button(
                    parts,
                    &format!("ws-agent-use-{}", choice.key),
                    &review.use_model,
                    move || WorkspaceSettingsEffect::SetAgentModel {
                        agent: agent.clone(),
                        model: model.clone(),
                    },
                )
                .control_size(ControlSize::Small)
                .with_enabled(review.editable),
            ))
        };
        column = column.push(Box::new(row));
    }
    Box::new(column)
}

/// The Agents tab as the workspace's owner sees it, for captures.
pub(super) fn sample() -> Agents {
    let agent = |key: &str, name: &str, badge: &str, detail: &str| Agent {
        key: key.to_owned(),
        name: name.to_owned(),
        badge: badge.to_owned(),
        detail: detail.to_owned(),
        admitted: "Admitted".to_owned(),
    };
    Agents {
        intro: Intro {
            title: "2 agents".to_owned(),
            help: "Agents act in the workspace with the access you give them.".to_owned(),
            authority: "You manage".to_owned(),
        },
        add: "Add agent".to_owned(),
        manage: true,
        header: "Agents".to_owned(),
        agents: vec![
            agent(
                "claude",
                "claude",
                "Agent",
                "Can change things as a Contributor, 1 more permission",
            ),
            agent(
                "relay",
                "Encrypted backup",
                "Service",
                "Reads until removed",
            ),
        ],
        review_label: "Review access".to_owned(),
        remove_label: "Revoke".to_owned(),
        empty: (
            "No agents yet".to_owned(),
            "Add an agent to have it work here.".to_owned(),
        ),
        bots_header: "Bots".to_owned(),
        add_bot: "Add a bot".to_owned(),
        bot_tag: "Bot".to_owned(),
        remove_bot: "Remove".to_owned(),
        bots: vec![Bot {
            name: "Standup".to_owned(),
            entry: Some("standup".to_owned()),
            detail: "Running version 1.2".to_owned(),
        }],
        bots_empty: "No bots yet.".to_owned(),
        footnote: None,
        review: None,
    }
}

/// One agent's review, for captures: notes open, a model chosen and one
/// action always allowed.
pub(super) fn sample_review() -> AgentReview {
    let line = |kind: &str, label: &str, origin: &str, answer: usize| KindLine {
        kind: kind.to_owned(),
        label: label.to_owned(),
        origin: origin.to_owned(),
        answer,
    };
    AgentReview {
        agent: "claude".to_owned(),
        name: "claude".to_owned(),
        badge: "Agent".to_owned(),
        back: "Agents".to_owned(),
        note: "Actions that send, publish or delete for good still ask first.".to_owned(),
        editable: true,
        answers: [
            "Allow".to_owned(),
            "Ask first".to_owned(),
            "Deny".to_owned(),
        ],
        reset: "Reset".to_owned(),
        custom: "Custom".to_owned(),
        apps: vec![
            AccessApp {
                scope: "so.tilo.notes".to_owned(),
                label: "Notes".to_owned(),
                customised: true,
                detail: "Set here for this agent".to_owned(),
                kinds: vec![
                    line("read", "Reading", "Default", 0),
                    line("change", "Changes", "Set here", 1),
                    line("outside", "Outside effects", "Default", 2),
                ],
            },
            AccessApp {
                scope: "so.tilo.calendar".to_owned(),
                label: "Calendar".to_owned(),
                customised: false,
                detail: "Follows its admission".to_owned(),
                kinds: vec![
                    line("read", "Reading", "Default", 0),
                    line("change", "Changes", "Default", 0),
                    line("outside", "Outside effects", "Default", 1),
                ],
            },
        ],
        empty: "No apps to answer for yet.".to_owned(),
        models: Some(Models {
            title: "Model".to_owned(),
            summary: "Sonnet (Anthropic)".to_owned(),
            choices: vec![
                ModelChoice {
                    key: String::new(),
                    label: "Default: Opus (Anthropic)".to_owned(),
                    in_use: false,
                },
                ModelChoice {
                    key: "anthropic/sonnet".to_owned(),
                    label: "Sonnet (Anthropic)".to_owned(),
                    in_use: true,
                },
            ],
        }),
        use_model: "Use".to_owned(),
        in_use: "In use".to_owned(),
        exceptions_title: "Always allowed (1)".to_owned(),
        exceptions: vec![Exception {
            scope: "so.tilo.notes/notes.notebook/delete".to_owned(),
            kind: "outside".to_owned(),
            label: "Notes: Delete for good".to_owned(),
        }],
        stop: "Stop".to_owned(),
    }
}

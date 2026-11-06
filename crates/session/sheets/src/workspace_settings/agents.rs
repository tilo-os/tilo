//! The Agents tab: the agents admitted to the workspace, each with what it
//! may do and the way to review or take that back, and the workspace's
//! bots apart from them.
//!
//! Reviewing an agent shows, in the tab's place, what it may do app by
//! app: Allow, Ask first or Deny for reading, changes and outside effects,
//! the model it plans with, and the actions always allowed without asking.

use ui::icons::{Icon, app_glyph};
use ui::tokens::Rhythm;
use ui::widget::{
    Button, ButtonRole, ChipStyle, Column, ControlSize, Element, EmptyState, ListRow, Row, Section,
    Segment, TextInput, ToggleGroup, Tone,
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

/// One agent, in the person's words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Agent {
    /// What it is asked for by.
    pub key: String,
    pub name: String,
    /// What it may do, in one plain line.
    pub status: String,
    /// "Yours" or "Service", when it is one of the two: one quiet tag, never
    /// a pile of them.
    pub tag: Option<String>,
    /// The model it plans with, as the row's quiet fact; empty for a service,
    /// which has none.
    pub model: String,
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
    /// The answer, as an index into [`AgentReview::answers`].
    pub answer: usize,
}

/// One app an agent's answers are given for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessApp {
    /// The app it is, for its mark.
    pub app: String,
    /// What its answers are asked for by.
    pub scope: String,
    pub label: String,
    /// Whether any of its answers was set here, which Reset takes back.
    pub customised: bool,
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
    /// Whether the person may choose: only an agent's keeper may.
    pub can_choose: bool,
    /// Who chooses, where it is not the person here.
    pub note: Option<String>,
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
    /// What it may do, in one plain line.
    pub status: String,
    pub tag: Option<String>,
    pub back: String,
    pub note: String,
    /// Whether the person may change its answers.
    pub editable: bool,
    /// Whether the person may retire it here.
    pub can_retire: bool,
    pub retire: String,
    /// The three answers' names: Allow, Ask first, Deny.
    pub answers: [String; 3],
    pub reset: String,
    /// What an app's answers say as one tag, once any was set here.
    pub custom: String,
    pub apps: Vec<AccessApp>,
    /// What the review says when there is no app to answer for.
    pub empty: String,
    /// The model it plans with, for an agent the workspace keeps a record of.
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
    /// What the button making an agent says.
    pub add: String,
    /// What the new agent's name field is called for assistive tools, and
    /// what it says while it is empty.
    pub name: String,
    /// Whether the person may make, review the controls of, and retire
    /// agents and bots.
    pub manage: bool,
    pub header: String,
    pub agents: Vec<Agent>,
    pub review_label: String,
    pub retire_label: String,
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
    groups.push(new_agent(agents, parts));
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

/// The way to make an agent: its name, which it keeps everywhere, and the
/// button making it.
fn new_agent(agents: &Agents, parts: &Parts) -> Element {
    let submit = parts.clone();
    let field = TextInput::from_state(&parts.new_agent)
        .with_id("ws-agents-name")
        .with_placeholder(agents.name.clone())
        .with_accessible_name(agents.name.clone())
        .with_read_only(!agents.manage)
        .on_submit_view(move |_| submit.add_agent());
    let typed = parts.clone();
    Box::new(
        Row::new()
            .with_rhythm(Rhythm::Related)
            .push(Box::new(field))
            .push(Box::new(
                Button::new("ws-agents-add", &agents.add)
                    .role(ButtonRole::Primary)
                    .with_enabled(agents.manage)
                    .on_effect(move || typed.add_agent()),
            )),
    )
}

/// One agent: its mark, name, what it may do and the model it plans with,
/// with Review access and Retire.
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
            &agents.retire_label,
            move || WorkspaceSettingsEffect::RemoveAgent {
                agent: remove.clone(),
            },
        )
        .role(ButtonRole::Destructive)
        .control_size(ControlSize::Small)
        .with_enabled(agents.manage),
    ]);
    let mut row = ListRow::new(format!("ws-agents-agent-{}", agent.key), agent.name.clone())
        .two_line_title()
        .avatar(agent.name.clone(), agent.key.clone())
        .secondary(agent.status.clone());
    if let Some(tag) = &agent.tag {
        row = row.tags(&[(tag.as_str(), ChipStyle::Neutral)]);
    }
    if !agent.model.is_empty() {
        row = row.value(agent.model.clone());
    }
    Box::new(row.trailing(controls))
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
    let mut groups: Vec<Element> = vec![Box::new(
        Row::new()
            .with_rhythm(Rhythm::Related)
            .align_start()
            .push(Box::new(
                button(parts, "ws-agent-back", &review.back, || {
                    WorkspaceSettingsEffect::EndReview
                })
                .with_icon(Icon::ChevronLeft),
            )),
    )];
    let agent = review.agent.clone();
    let mut head = ListRow::new("ws-agent-name", review.name.clone())
        .avatar(review.name.clone(), review.agent.clone())
        .secondary(review.status.clone());
    if let Some(tag) = &review.tag {
        head = head.tags(&[(tag.as_str(), ChipStyle::Neutral)]);
    }
    if review.can_retire {
        head = head.trailing(Box::new(
            button(parts, "ws-agent-retire", &review.retire, move || {
                WorkspaceSettingsEffect::RemoveAgent {
                    agent: agent.clone(),
                }
            })
            .role(ButtonRole::Destructive)
            .control_size(ControlSize::Small)
            .with_enabled(review.editable),
        ));
    }
    groups.push(Box::new(head));
    groups.push(caption(&review.note));
    if review.apps.is_empty() {
        groups.push(caption(&review.empty));
    }
    for app in &review.apps {
        groups.push(access_app(review, app, parts));
    }
    if let Some(models) = &review.models {
        groups.push(models_section(review, models, parts));
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

/// One app: its mark and name, with Reset once any of its answers was set
/// here, then an Allow, Ask first or Deny control for each kind of action.
fn access_app(review: &AgentReview, app: &AccessApp, parts: &Parts) -> Element {
    let mut head = ListRow::new(format!("ws-agent-app-{}", app.scope), app.label.clone())
        .icon(app_glyph(&app.app, None));
    if app.customised {
        head = head.tags(&[(review.custom.as_str(), ChipStyle::Tinted(Tone::Info))]);
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

/// One kind of action at one app: its name and the answer, which the arrow
/// keys move along.
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
    let answers = ToggleGroup::new(segments, line.answer)
        .with_id(format!("ws-agent-answer-{}-{}", app.scope, line.kind))
        .with_accessible_name(format!("{} {}", app.label, line.label))
        .with_enabled(review.editable)
        .on_view(
            &format!("ws-agent-answer-{}-{}", app.scope, line.kind),
            move |index| {
                if let Some(answer) = ANSWERS.get(index) {
                    press.ask(WorkspaceSettingsEffect::SetAgentAnswer {
                        agent: agent.clone(),
                        scope: scope.clone(),
                        kind: kind.clone(),
                        answer: (*answer).to_owned(),
                    });
                }
            },
        );
    Box::new(
        ListRow::new(
            format!("ws-agent-kind-{}-{}", app.scope, line.kind),
            line.label.clone(),
        )
        .nested()
        .trailing(Box::new(answers)),
    )
}

/// The agent's model: what it plans with, who chooses it, and the default
/// and every model offered, the one in use marked and each other with Use
/// for its keeper.
fn models_section(review: &AgentReview, models: &Models, parts: &Parts) -> Element {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Related)
        .push(Box::new(
            ListRow::new("ws-agent-models", models.title.clone()).value(models.summary.clone()),
        ));
    if let Some(note) = &models.note {
        column = column.push(caption(note));
    }
    for choice in &models.choices {
        let mut row = ListRow::new(
            format!("ws-agent-model-{}", choice.key),
            choice.label.clone(),
        )
        .nested();
        row = if choice.in_use {
            row.tags(&[(review.in_use.as_str(), ChipStyle::Tinted(Tone::Success))])
        } else if models.can_choose {
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
                .control_size(ControlSize::Small),
            ))
        } else {
            row
        };
        column = column.push(Box::new(row));
    }
    Box::new(column)
}

/// The Agents tab as the workspace's owner sees it, for captures.
pub(super) fn sample() -> Agents {
    let agent = |key: &str, name: &str, status: &str, tag: &str, model: &str| Agent {
        key: key.to_owned(),
        name: name.to_owned(),
        status: status.to_owned(),
        tag: Some(tag.to_owned()),
        model: model.to_owned(),
    };
    Agents {
        intro: Intro {
            title: "3 agents".to_owned(),
            help: "Agents work alongside you here. They only do what you allow.".to_owned(),
            authority: "You manage".to_owned(),
        },
        add: "Add agent".to_owned(),
        name: "Name a new agent".to_owned(),
        manage: true,
        header: "Agents".to_owned(),
        agents: vec![
            agent(
                "claude",
                "Claude",
                "Can read and change, asks before acting outside",
                "Yours",
                "Model: Sonnet from Anthropic",
            ),
            agent(
                "reviewer",
                "Reviewer",
                "Can read and comment, asks before changing",
                "Yours",
                "Model: No model chosen in Settings",
            ),
            Agent {
                key: "relay".to_owned(),
                name: "Encrypted backup".to_owned(),
                status: "Reads encrypted content until removed".to_owned(),
                tag: Some("Service".to_owned()),
                model: String::new(),
            },
        ],
        review_label: "Review access".to_owned(),
        retire_label: "Retire".to_owned(),
        empty: (
            "No agents yet".to_owned(),
            "Add an agent to have it work here.".to_owned(),
        ),
        bots_header: "Bots".to_owned(),
        add_bot: "Add a bot".to_owned(),
        bot_tag: "Bot".to_owned(),
        remove_bot: "Remove".to_owned(),
        bots: vec![
            Bot {
                name: "Standup".to_owned(),
                entry: Some("so.tilo.bots.standup".to_owned()),
                detail: "Running version 1.2".to_owned(),
            },
            // One the workspace's host runs from its own choice: it is
            // named here and not a member's to remove.
            Bot {
                name: "Greeter".to_owned(),
                entry: None,
                detail: "Running".to_owned(),
            },
        ],
        bots_empty: "No bots yet.".to_owned(),
        footnote: None,
        review: None,
    }
}

/// One agent's review, for captures: answers set in two apps, a model
/// chosen and one action always allowed.
pub(super) fn sample_review() -> AgentReview {
    let line = |kind: &str, label: &str, answer: usize| KindLine {
        kind: kind.to_owned(),
        label: label.to_owned(),
        answer,
    };
    AgentReview {
        agent: "claude".to_owned(),
        name: "Claude".to_owned(),
        status: "Can read and change, asks before acting outside".to_owned(),
        tag: Some("Yours".to_owned()),
        back: "All agents".to_owned(),
        note: "Sending, publishing and deleting for good still ask you first, unless you allowed that action for good.".to_owned(),
        editable: true,
        can_retire: true,
        retire: "Retire".to_owned(),
        answers: [
            "Allow".to_owned(),
            "Ask first".to_owned(),
            "Deny".to_owned(),
        ],
        reset: "Use defaults".to_owned(),
        custom: "Set here".to_owned(),
        apps: vec![
            AccessApp {
                app: "so.tilo.notes".to_owned(),
                scope: "so.tilo.notes".to_owned(),
                label: "Notes".to_owned(),
                customised: true,
                kinds: vec![
                    line("read", "Reading", 0),
                    line("change", "Changes", 1),
                    line("outside", "Outside effects", 2),
                ],
            },
            AccessApp {
                app: "so.tilo.calendar".to_owned(),
                scope: "so.tilo.calendar".to_owned(),
                label: "Calendar".to_owned(),
                customised: false,
                kinds: vec![
                    line("read", "Reading", 0),
                    line("change", "Changes", 0),
                    line("outside", "Outside effects", 1),
                ],
            },
        ],
        empty: "No apps to answer for yet.".to_owned(),
        models: Some(Models {
            title: "Model".to_owned(),
            summary: "Sonnet from Anthropic".to_owned(),
            choices: vec![
                ModelChoice {
                    key: String::new(),
                    label: "No model chosen in Settings".to_owned(),
                    in_use: false,
                },
                ModelChoice {
                    key: "anthropic/sonnet".to_owned(),
                    label: "Sonnet from Anthropic".to_owned(),
                    in_use: true,
                },
            ],
            can_choose: true,
            note: Some("You choose the model it runs on your devices".to_owned()),
        }),
        use_model: "Use".to_owned(),
        in_use: "In use".to_owned(),
        exceptions_title: "Always allowed: 1".to_owned(),
        exceptions: vec![Exception {
            scope: "so.tilo.notes/notes.notebook/delete".to_owned(),
            kind: "outside".to_owned(),
            label: "Notes: Delete for good".to_owned(),
        }],
        stop: "Ask again".to_owned(),
    }
}

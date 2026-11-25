//! The Agents tab: the agents admitted to the workspace, each with what it
//! may do and the way to review or take that back.
//!
//! Reviewing an agent shows, in the tab's place, what it may do app by
//! app: Allow, Ask first or Deny for reading, changes and outside effects,
//! the model it plans with, and the actions always allowed without asking.

use ui::icons::{Icon, app_glyph};
use ui::peniko::Color;
use ui::tokens::{PresenceHue, Rhythm};
use ui::widget::{
    Button, ButtonRole, ChipStyle, Column, Container, ControlSize, Dropdown, Element, EmptyState,
    ListRow, Row, Section, Segment, TextInput, ToggleGroup, Tone,
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
    /// Who added it, in one plain line.
    pub status: String,
    /// What it spent of the day the workspace allows, as the line under that one;
    /// empty where no limit is set, or it spent nothing of one yet.
    pub spend: String,
    /// The model it plans with, as the row's quiet fact; empty for a service,
    /// which has none.
    pub model: String,
}

/// What an agent's answer for one kind of action is, at one app.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KindLine {
    /// read, change or outside.
    pub kind: String,
    pub label: String,
    /// The answer, as an index into [`AgentReview::answers`].
    pub answer: usize,
    /// The answers it may be given, as indices into [`AgentReview::answers`]
    /// in segment order: an outside effect is never allowed unasked, so its
    /// line offers no Allow.
    pub choices: Vec<usize>,
    /// What its actions are, where its name alone does not say: the outside
    /// effects of the person's own account. Empty for an app's own line.
    pub caption: Option<String>,
}

/// One app an agent's answers are given for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessApp {
    /// The mark it wears: an app's own, or the person's account's.
    pub icon: Icon,
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
    /// Whether the person may choose: whoever the workspace's grants let change its agents.
    pub can_choose: bool,
    /// Who chooses, where it is not the person here.
    pub note: Option<String>,
}

/// What an agent may spend a day, as its review shows it: the workspace's
/// limit, what it spent of it, and the way to set it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Limit {
    pub title: String,
    /// What it may spend a day now: `$5 a day`, or none.
    pub summary: String,
    /// What it spent of it today: `$0.42 today`.
    pub today: String,
    /// What the limit is for, and who sets it.
    pub note: String,
    /// What the field says while it is empty.
    pub placeholder: String,
    pub set: String,
    /// Whether the person here sets it: whoever the workspace's grants let change its agents.
    pub can_choose: bool,
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
    /// The model it plans with, for an agent the workspace keeps a record of.
    pub models: Option<Models>,
    /// What it may spend a day, for an agent the workspace keeps a record of.
    pub limit: Option<Limit>,
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
    /// What the button opening a new agent's form says, and the form's title.
    pub add: String,
    /// The form's two answers: make it, or put it away.
    pub create: String,
    pub cancel: String,
    /// The form's name field: what it is called for assistive tools, and what
    /// it says while it is empty.
    pub name: String,
    /// The form's model row.
    pub model: String,
    /// What the model row says while no model is chosen.
    pub model_default: String,
    /// The models a new agent may plan with, from the person's Settings.
    pub models: Vec<ModelChoice>,
    /// Whether the person may make, review the controls of, and retire
    /// agents.
    pub manage: bool,
    pub header: String,
    pub agents: Vec<Agent>,
    pub review_label: String,
    pub retire_label: String,
    /// What the tab says with no agents: title and detail.
    pub empty: (String, String),
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
    // With no agents the empty state is the tab's whole introduction; the
    // count and authority above it would only say "none" twice.
    let mut groups: Vec<Element> = Vec::new();
    if !agents.agents.is_empty() {
        groups.push(intro(&agents.intro, Icon::Sparkles));
    }
    groups.push(new_agent(agents, parts));
    if agents.agents.is_empty() {
        let (title, detail) = &agents.empty;
        groups.push(Box::new(
            EmptyState::new(title.clone())
                .with_icon(Icon::Sparkles)
                .with_message(detail.clone())
                .inline(),
        ));
    } else {
        groups.push(Box::new(Section::titled(
            agents.header.clone(),
            agents
                .agents
                .iter()
                .map(|agent| agent_row(agents, agent, parts))
                .collect(),
        )));
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

/// The way to make an agent: one button, and the small form it opens with
/// the name people will ask it by and the model it plans with.
fn new_agent(agents: &Agents, parts: &Parts) -> Element {
    if !parts.local.get().creating {
        let open = parts.clone();
        return Box::new(
            Row::new().with_rhythm(Rhythm::Related).push(Box::new(
                Button::new("ws-agents-new", &agents.add)
                    .role(ButtonRole::Primary)
                    .with_icon(Icon::Plus)
                    .with_enabled(agents.manage)
                    .on_click(move || open.start_agent()),
            )),
        );
    }
    let submit = parts.clone();
    let field = TextInput::from_state(&parts.new_agent)
        .with_id("ws-agents-name")
        .with_placeholder(agents.name.clone())
        .with_accessible_name(agents.name.clone())
        .with_read_only(!agents.manage)
        .on_submit(move |_| submit.add_agent());
    let name = Row::new()
        .with_rhythm(Rhythm::Related)
        .push(Box::new(field));

    // Its model: one of this device's Settings, or the workspace's default. Where this device offers
    // none (a page, whose agents run on the workspace's server) there is nothing to choose.
    let offered: Vec<(String, String)> = agents
        .models
        .iter()
        .map(|model| (model.key.clone(), model.label.clone()))
        .collect();
    let mut model = Dropdown::from_state(
        offered.iter().map(|(_, label)| label.clone()).collect(),
        &parts.new_model,
    )
    .with_id("ws-agents-new-model")
    .with_accessible_name(agents.model.clone())
    .with_placeholder(&agents.model_default)
    .with_enabled(agents.manage);
    model.set_value_options(offered);
    let model = ListRow::new("ws-agents-new-model-row", agents.model.clone())
        .nested()
        .trailing(Box::new(model));

    // The form's answers last, at its end: Create at the right, Cancel beside
    // it.
    let typed = parts.clone();
    let put_away = parts.clone();
    let answers = Row::new()
        .with_rhythm(Rhythm::Related)
        .with_justify(ui::taffy::JustifyContent::FlexEnd)
        .push(Box::new(
            Button::new("ws-agents-create-cancel", &agents.cancel)
                .with_enabled(agents.manage)
                .on_click(move || put_away.cancel_agent()),
        ))
        .push(Box::new(
            Button::new("ws-agents-create", &agents.create)
                .role(ButtonRole::Primary)
                .with_enabled(agents.manage)
                .on_click(move || typed.add_agent()),
        ));

    let mut rows: Vec<Element> = vec![Box::new(name)];
    if !agents.models.is_empty() {
        rows.push(Box::new(model));
    }
    rows.push(Box::new(answers));
    Box::new(Section::titled(agents.add.clone(), rows))
}

/// The colour the agent `key` wears everywhere, as a row's avatar wears it.
fn colour_of(key: &str) -> Color {
    let hue = PresenceHue::of_member(&ui::permissions::MemberId::agent(key, "ada"));
    ui::Theme::dark().tokens.colors.presence.hue(hue)
}

/// One agent: its face in its own colour, name, who added it and the model it
/// plans with, with Review access and Remove.
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
        .avatar_colour(colour_of(&agent.key))
        .secondary(agent.status.clone());
    if !agent.spend.is_empty() {
        row = row.note(agent.spend.clone());
    }
    if !agent.model.is_empty() {
        row = row.value(agent.model.clone());
    }
    Box::new(row.trailing(controls))
}

/// One agent's review in the roster's place: Back, then each app's
/// answers, its model and its access exceptions.
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
        .avatar_colour(colour_of(&review.agent))
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
            .control_size(ControlSize::Small),
        ));
    }
    groups.push(Box::new(head));
    groups.push(caption(&review.note));
    for app in &review.apps {
        groups.push(access_app(review, app, parts));
    }
    if let Some(models) = &review.models {
        groups.push(models_section(review, models, parts));
    }
    if let Some(limit) = &review.limit {
        groups.push(limit_section(review, limit, parts));
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
    let mut head =
        ListRow::new(format!("ws-agent-app-{}", app.scope), app.label.clone()).icon(app.icon);
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
    let segments = line
        .choices
        .iter()
        .filter_map(|choice| review.answers.get(*choice))
        .map(|answer| Segment::new(answer.clone()))
        .collect();
    let chosen = line
        .choices
        .iter()
        .position(|choice| *choice == line.answer)
        .unwrap_or_default();
    let choices = line.choices.clone();
    let (press, agent, scope, kind) = (
        parts.clone(),
        review.agent.clone(),
        app.scope.clone(),
        line.kind.clone(),
    );
    let answers = ToggleGroup::new(segments, chosen)
        .with_id(format!("ws-agent-answer-{}-{}", app.scope, line.kind))
        .with_accessible_name(format!("{} {}", app.label, line.label))
        // Every line keeps a place per answer, so a line offering fewer lines up with the rest.
        .with_columns(ANSWERS.len())
        .with_enabled(review.editable)
        .on_choose(
            &format!("ws-agent-answer-{}-{}", app.scope, line.kind),
            move |index| {
                if let Some(answer) = choices.get(index).and_then(|choice| ANSWERS.get(*choice)) {
                    press.ask(WorkspaceSettingsEffect::SetAgentAnswer {
                        agent: agent.clone(),
                        scope: scope.clone(),
                        kind: kind.clone(),
                        answer: (*answer).to_owned(),
                    });
                }
            },
        );
    let mut row = ListRow::new(
        format!("ws-agent-kind-{}-{}", app.scope, line.kind),
        line.label.clone(),
    )
    .nested();
    if let Some(caption) = &line.caption {
        // A caption wraps to the width the control leaves it, onto as many lines as it
        // takes: it is never cut short.
        row = row.secondary(caption.as_str()).secondary_lines(4);
    }
    Box::new(row.trailing(Box::new(answers)))
}

/// The agent's model: what it plans with, who chooses it, and the default
/// and every model offered, the one in use marked and each other with Use
/// for whoever may change it.
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

/// What an agent may spend a day: what it is, what it spent of it, and for
/// whoever may change it the way to set it — a number of dollars, taken by Set.
fn limit_section(review: &AgentReview, limit: &Limit, parts: &Parts) -> Element {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Related)
        .push(Box::new(
            ListRow::new("ws-agent-limit", limit.title.clone())
                .meta(limit.today.clone())
                .value(limit.summary.clone()),
        ))
        .push(caption(&limit.note));
    if !limit.can_choose {
        return Box::new(column);
    }
    let submit = parts.clone();
    let typed = review.agent.clone();
    let field = TextInput::from_state(&parts.limit)
        .with_id("ws-agent-limit-field")
        .with_placeholder(limit.placeholder.clone())
        .with_accessible_name(limit.title.clone())
        .on_submit(move |_| submit.commit_agent_limit(&typed));
    let press = parts.clone();
    let agent = review.agent.clone();
    let set = Button::new("ws-agent-limit-set", &limit.set)
        .control_size(ControlSize::Small)
        .on_click(move || press.commit_agent_limit(&agent));
    column = column.push(Box::new(
        Row::new()
            .no_wrap()
            .with_rhythm(Rhythm::Related)
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_child(Box::new(field)),
            ))
            .push(Box::new(set)),
    ));
    Box::new(column)
}

/// The Agents tab as the workspace's owner sees it, for captures.
pub(super) fn sample() -> Agents {
    let agent = |key: &str, name: &str, status: &str, spend: &str, model: &str| Agent {
        key: key.to_owned(),
        name: name.to_owned(),
        status: status.to_owned(),
        spend: spend.to_owned(),
        model: model.to_owned(),
    };
    Agents {
        intro: Intro {
            title: "3 agents".to_owned(),
            help: "Agents work alongside you here. They only do what you allow.".to_owned(),
            authority: "You manage".to_owned(),
        },
        add: "New agent".to_owned(),
        create: "Create".to_owned(),
        cancel: "Cancel".to_owned(),
        name: "Name".to_owned(),
        model: "Model".to_owned(),
        model_default: "Your default model".to_owned(),
        models: vec![
            ModelChoice {
                key: "anthropic/sonnet".to_owned(),
                label: "Sonnet from Anthropic".to_owned(),
                in_use: false,
            },
            ModelChoice {
                key: "openai/gpt".to_owned(),
                label: "GPT from OpenAI".to_owned(),
                in_use: false,
            },
        ],
        manage: true,
        header: "Agents".to_owned(),
        agents: vec![
            agent(
                "claude",
                "Claude",
                "Kept by you",
                "$0.42 of $5 today",
                "Model: Sonnet from Anthropic",
            ),
            agent(
                "reviewer",
                "Reviewer",
                "Kept by you",
                "",
                "Model: No model chosen in Settings",
            ),
            agent(
                "server",
                "Encrypted backup",
                "Kept by Ada",
                "$1.20 today",
                "",
            ),
        ],
        review_label: "Review access".to_owned(),
        retire_label: "Remove".to_owned(),
        empty: (
            "No agents yet".to_owned(),
            "Add an agent to have it work here.".to_owned(),
        ),
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
        choices: if kind == "outside" {
            vec![1, 2]
        } else {
            vec![0, 1, 2]
        },
        caption: None,
    };
    AgentReview {
        agent: "claude".to_owned(),
        name: "Claude".to_owned(),
        status: "Can read and change, asks before acting outside".to_owned(),
        tag: None,
        back: "All agents".to_owned(),
        note: "Sending, publishing and deleting for good still ask you first, unless you allowed that action for good.".to_owned(),
        editable: true,
        can_retire: true,
        retire: "Remove".to_owned(),
        answers: [
            "Allow".to_owned(),
            "Ask first".to_owned(),
            "Deny".to_owned(),
        ],
        reset: "Use defaults".to_owned(),
        custom: "Set here".to_owned(),
        apps: vec![
            AccessApp {
                icon: app_glyph("so.tilo.notes", None),
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
                icon: app_glyph("so.tilo.calendar", None),
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
            note: None,
        }),
        limit: Some(Limit {
            title: "Daily limit".to_owned(),
            summary: "$5 a day".to_owned(),
            today: "$0.42 today".to_owned(),
            note: "It stops at this limit and tells you when a run reaches it.".to_owned(),
            placeholder: "Dollars a day".to_owned(),
            set: "Set".to_owned(),
            can_choose: true,
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

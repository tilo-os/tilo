//! Agents: every run of every agent across the person's workspaces, and the run chosen, as
//! the timeline of its actions.

use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_actions::remote::agent_door as door;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::{
    ButtonRole, Fact, Facts, ListRow, ObjectHeader, Section, SectionHeader, Surface, SurfaceLevel,
    Tone,
};

use crate::effects::ConsoleEffect;
use crate::{Console, Doors, Nav, Reads, ago, faced, page, tr, trf};

/// What Agents reads: every run in every workspace.
pub(crate) fn query() -> wire::RunsQuery {
    wire::RunsQuery {
        scope: wire::Scope::AllWorkspaces,
        limit: 0,
    }
}

pub(crate) const RUN_KIND: &str = "console.run";

/// A run as the type its rows are items of: what a published run says it is, so search
/// names it as it names a record's item ("runs of Console") instead of taking the app's
/// word for it.
pub(crate) struct Run;

impl ui::tilo_resource::words::RowType for Run {
    const KIND: &'static str = RUN_KIND;
    const LABEL: &'static str = "Run";
}

/// How many of the runs read are published as resources of their own: a published tree
/// stays bounded, and any other run's address is answered by [`live_run`] alone.
pub(crate) const RUN_RESOURCE_LIMIT: usize = 16;

/// The address of the run `id`: what a link to it names.
pub(crate) fn run_resource_id(id: &str) -> String {
    format!("{RUN_KIND}:{id}")
}

/// The run whose address `resource` is, when it is a run's.
pub(crate) fn run_id(resource: &str) -> Option<&str> {
    resource
        .strip_prefix(RUN_KIND)?
        .strip_prefix(':')
        .filter(|id| !id.is_empty())
}

/// The view a run opens in: its page in this window, opened by [`ConsoleEffect::ViewRun`].
fn run_view() -> ui::tilo_actions::ResourceViewSpec {
    ui::tilo_actions::ResourceViewSpec::new("run", "Run", ConsoleEffect::VIEW_RUN.id)
        .with_primary(true)
        .on_desktop()
}

/// The run `id` as the resource its address names, labelled by the run when `runs` lists
/// it: its page in this window, and its link.
pub(crate) fn live_run(runs: &[wire::AgentRun], id: &str) -> LiveRow {
    let label = runs
        .iter()
        .find(|run| run.id == id)
        .map_or_else(|| tr("console-run"), |run| run.instruction.clone());
    LiveRow::typed::<Run>(run_resource_id(id), label)
        .effect(&ConsoleEffect::ViewRun { run: id.to_owned() })
        .view(run_view())
}

/// The runs `runs` lists as resources of their own, up to the limit, first run first.
pub(crate) fn live_runs(runs: &[wire::AgentRun]) -> Vec<LiveRow> {
    runs.iter()
        .take(RUN_RESOURCE_LIMIT)
        .map(|run| live_run(runs, &run.id))
        .collect()
}

/// The row the address `resource` names, when it names a run: a link opens it before the
/// history has answered, so the run's page shows at once and the run arrives on it.
pub(crate) fn live_run_at(runs: &[wire::AgentRun], resource: &str) -> Option<LiveRow> {
    Some(live_run(runs, run_id(resource)?))
}

/// The runs, beside the door that says which agents are connected.
#[derive(Clone, Default)]
pub(crate) struct Seen {
    pub(crate) runs: wire::Runs,
    pub(crate) door: door::Door,
}

impl Seen {
    /// The door's open run `run` is, with the grant it came in by: History names a run as
    /// the door does ("Claude Code #2").
    pub(crate) fn open(&self, run: &wire::AgentRun) -> Option<door::RunRef> {
        self.door
            .run(&run.agent.name)
            .map(|(grant, open)| door::RunRef {
                grant: grant.id.clone(),
                run: open.name.clone(),
            })
    }

    /// Whether `run` is working now: its session through the door is open and it is that
    /// session's newest run.
    pub(crate) fn working(&self, run: &wire::AgentRun) -> bool {
        self.open(run).is_some()
            && self
                .runs
                .runs
                .iter()
                .filter(|other| other.agent.name == run.agent.name)
                .max_by_key(|other| other.last_ms)
                .is_some_and(|newest| newest.id == run.id)
    }

    /// The workspace `run` worked in, as the door lists it.
    pub(crate) fn workspace(&self, run: &wire::AgentRun) -> Option<&door::Workspace> {
        let name = run.workspace.as_deref()?;
        self.door
            .workspaces
            .iter()
            .find(|workspace| workspace.name == name || workspace.id == name)
    }
}

/// The place's content: every run, or the run chosen, whose way back is the shell's.
pub(crate) fn view(cx: &ViewCx<Console>, nav: &Nav, reads: &Reads) -> Element {
    let runs = &reads.runs;
    let door_state = reads.door.answer().state();
    let chosen = nav.run.get();
    let nav = nav.clone();
    Box::new(cx.arriving(&runs.answer(), move |cx, runs| {
        let (nav, runs, chosen) = (nav.clone(), runs.clone(), chosen.clone());
        cx.switch(&door_state, move |cx, door| {
            let seen = Seen {
                runs: runs.clone(),
                door: ready(door),
            };
            let run = chosen
                .as_deref()
                .and_then(|id| seen.runs.runs.iter().find(|run| run.id == id));
            match (run, chosen.as_deref()) {
                (Some(run), _) => inspect(cx, &nav, &seen, run),
                (None, Some(_)) => unknown(&seen),
                (None, None) => list(cx, &nav, &seen),
            }
        })
    })) as Element
}

/// The door as far as it answered: nothing connected until it does, and nothing where no
/// door is served.
pub(crate) fn ready<E>(door: &Arrival<door::Door, E>) -> door::Door {
    match door {
        Arrival::Ready(door) => door.clone(),
        Arrival::Pending { .. } | Arrival::Failed(_) => door::Door::default(),
    }
}

/// A run's page whose run the history has not listed: still being read, or not here at all
/// (a workspace this person is not in, a run long gone).
pub(crate) fn unknown(seen: &Seen) -> Element {
    let state = if seen.runs.pending {
        EmptyState::new(tr("console-run-pending")).with_icon(Icon::Clock)
    } else {
        EmptyState::new(tr("console-run-missing"))
            .with_message(tr("console-run-missing-message"))
            .with_icon(Icon::Bot)
    };
    page(vec![Box::new(state.with_id("console-run-missing"))])
}

/// Every run the record lets show: working ones first, then the rest.
fn list(cx: &ViewCx<Console>, nav: &Nav, seen: &Seen) -> Element {
    let filter = cx.value(Console::clone).get();
    let mut parts = vec![filters(cx, seen, &filter)];
    let shown: Vec<&wire::AgentRun> = seen
        .runs
        .runs
        .iter()
        .filter(|run| crate::record::shows(&filter, run))
        .collect();
    if shown.is_empty() {
        let state = if seen.runs.pending {
            EmptyState::new(tr("console-runs-pending")).with_icon(Icon::Clock)
        } else if filter.narrows() {
            EmptyState::new(tr("console-runs-none-here"))
                .with_message(tr("console-runs-none-here-message"))
                .with_icon(Icon::Bot)
        } else {
            EmptyState::new(tr("console-runs-empty"))
                .with_message(tr("console-runs-empty-message"))
                .with_icon(Icon::Bot)
        };
        parts.push(Box::new(state.with_id("console-runs-empty")));
        return page(parts);
    }
    let (working, earlier): (Vec<&wire::AgentRun>, Vec<&wire::AgentRun>) =
        shown.into_iter().partition(|run| seen.working(run));
    let chosen = nav.run.get();
    for (heading, runs) in [
        ("console-runs-working", working),
        ("console-runs-earlier", earlier),
    ] {
        if runs.is_empty() {
            continue;
        }
        let rows = runs
            .into_iter()
            .map(|run| run_row(nav, seen, run, chosen.as_deref() == Some(run.id.as_str())))
            .collect();
        parts.push(Box::new(
            Section::new(rows).with_header(SectionHeader::group(tr(heading))),
        ));
    }
    page(parts)
}

pub(crate) fn run_row(nav: &Nav, seen: &Seen, run: &wire::AgentRun, selected: bool) -> Element {
    let working = seen.working(run);
    let mut row = faced(
        ListRow::new(format!("console-run-{}", run.id), run.instruction.clone()),
        &run.agent,
    )
    .subtitle(
        run.now
            .as_ref()
            .filter(|_| working)
            .map_or_else(|| started_by(&run.agent), now_line),
    )
    .secondary(run.workspace.clone().unwrap_or_else(|| run_size(run)))
    .meta(ago(run.last_ms))
    .selected(selected);
    if working {
        row = row.active();
    }
    let (chosen, id) = (nav.clone(), run.id.clone());
    Box::new(row.on_view(move || chosen.open_run(&id)))
}

/// Who started a run, as its row says it: "Started by you" for the reader's own, the
/// starter's name for someone else's.
fn started_by(agent: &wire::Author) -> String {
    starter(agent).unwrap_or_else(|| agent.name.clone())
}

/// The line naming who started a run, when the agent worked for someone.
fn starter(agent: &wire::Author) -> Option<String> {
    if agent.for_reader {
        return Some(tr("console-run-started-by-you"));
    }
    agent
        .on_behalf_of
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .map(|name| {
            trf(
                "console-run-started-by",
                &[("name", name.to_owned().into())],
            )
        })
}

/// How much a run did, as its row says it: the changes it made, as its finished notice
/// counts them ("4 changes" for four paragraphs written in one note), else the commands and
/// pages it went through.
fn run_size(run: &wire::AgentRun) -> String {
    match run.changes() {
        0 => trf(
            "console-run-actions",
            &[("count", run.actions.len().into())],
        ),
        changes => trf("console-run-changes", &[("count", changes.into())]),
    }
}

/// What a working run is doing now, as its row says it: "Running `cargo test`…", "Editing
/// src/email.rs".
pub(crate) fn now_line(now: &wire::Now) -> String {
    match now {
        wire::Now::Running { command, .. } => trf(
            "console-now-running",
            &[("command", command.clone().into())],
        ),
        wire::Now::Editing { path, .. } => {
            trf("console-now-editing", &[("path", path.clone().into())])
        }
        wire::Now::Browsing { url, title, .. } => trf(
            "console-now-browsing",
            &[(
                "page",
                if title.is_empty() { url } else { title }.clone().into(),
            )],
        ),
        wire::Now::Changing { summary, .. } => summary.clone(),
        wire::Now::Ran { command, .. } => {
            trf("console-now-ran", &[("command", command.clone().into())])
        }
    }
}

/// A dropdown of `options` (value, label) bound to `field` of the record, each option under
/// the heading its group carries in the open list.
fn picker(
    cx: &ViewCx<Console>,
    id: &'static str,
    name: &str,
    options: Vec<(String, Vec<(String, String)>)>,
    field: Path<Console, String>,
) -> Element {
    let name = tr(name);
    Box::new(cx.control(
        move |state| {
            let mut picker = Dropdown::from_state(Vec::new(), state)
                .with_id(id)
                .with_accessible_name(name);
            picker.set_grouped_value_options(options);
            picker
        },
        field,
    ))
}

/// The agent picker's options: every agent, then each client with all of its runs under it,
/// so a person narrows to one run or to the client behind every one of them.
fn agent_options(seen: &Seen) -> Vec<(String, Vec<(String, String)>)> {
    let mut groups: Vec<(String, Vec<(String, String)>)> = vec![(
        String::new(),
        vec![(String::new(), tr("console-every-agent"))],
    )];
    for run in &seen.runs.runs {
        let client = crate::client::of(&run.agent.name);
        let known = groups
            .iter()
            .skip(1)
            .position(|(heading, _)| heading == client.name());
        let at = known.map_or_else(
            || {
                groups.push((
                    client.name().to_owned(),
                    vec![(
                        client.name().to_owned(),
                        trf(
                            "console-client-every-run",
                            &[("client", client.name().to_owned().into())],
                        ),
                    )],
                ));
                groups.len().saturating_sub(1)
            },
            |found| found.saturating_add(1),
        );
        if let Some((_, runs)) = groups.get_mut(at)
            && !runs.iter().any(|(value, _)| value == &run.agent.name)
        {
            runs.push((run.agent.name.clone(), run.agent.name.clone()));
        }
    }
    groups
}

/// The workspace picker's options: every workspace, with no heading over them, and then
/// each workspace by name.
fn workspace_options(seen: &Seen) -> Vec<(String, Vec<(String, String)>)> {
    let mut workspaces: Vec<(String, String)> =
        vec![(String::new(), tr("console-every-workspace"))];
    for run in &seen.runs.runs {
        if let Some(workspace) = &run.workspace
            && !workspaces.iter().any(|(value, _)| value == workspace)
        {
            workspaces.push((workspace.clone(), workspace.clone()));
        }
    }
    vec![(String::new(), workspaces)]
}

/// The row narrowing the runs by agent and workspace, with a way back to all of them while
/// it narrows them, and only then.
fn filters(cx: &ViewCx<Console>, seen: &Seen, filter: &Console) -> Element {
    let paths = Console::paths();
    let mut row = Row::new()
        .with_rhythm(Rhythm::Related)
        .push(picker(
            cx,
            "console-filter-agent",
            "console-filter-agent",
            agent_options(seen),
            paths.agent,
        ))
        .push(picker(
            cx,
            "console-filter-workspace",
            "console-filter-workspace",
            workspace_options(seen),
            paths.workspace,
        ));
    if filter.narrows() {
        row = row.push(Box::new(cx.actions().bind(
            Button::new("console-show-everything", "console-show-everything").with_icon(Icon::X),
            Console::SHOW_EVERYTHING,
        )));
    }
    Box::new(row)
}

/// The chosen run: what it was asked, what can be done with it, and its actions.
pub(crate) fn inspect(
    cx: &ViewCx<Console>,
    nav: &Nav,
    seen: &Seen,
    run: &wire::AgentRun,
) -> Element {
    let doors = Doors::of(cx);
    let mut facts = vec![Fact::with_icon(Icon::Bot, run.agent.name.clone())];
    if let Some(starter) = starter(&run.agent) {
        facts.push(Fact::with_icon(Icon::User, starter));
    }
    facts.extend(
        run.workspace
            .clone()
            .map(|workspace| Fact::with_icon(Icon::LayoutGrid, workspace)),
    );
    facts.push(Fact::with_icon(Icon::Clock, ago(run.started_ms)));
    let mut parts: Vec<Element> = vec![
        Box::new(ObjectHeader::new(run.instruction.clone())),
        Box::new(Facts::new(facts)),
    ];
    parts.push(commands(&doors, seen, run));
    if let Some(blocked) = &run.undo_blocked {
        parts.push(Box::new(
            Text::new(blocked)
                .with_role(TextRole::Caption)
                .dimmed()
                .with_id("console-run-undo-blocked"),
        ));
    }
    if let Some(decision) = &run.decision {
        parts.push(Box::new(Section::titled(
            tr("console-run-why"),
            vec![Box::new(Text::new(decision))],
        )));
    }
    let nav = nav.clone();
    let shown = run.clone();
    parts.push(cx.switch(&nav.action.clone(), move |cx, open| {
        let (nav, shown, open) = (nav.clone(), shown.clone(), *open);
        cx.switch(&nav.full_output.clone(), move |cx, full| {
            let Some((index, run)) = full.and_then(|index| Some((index, more_of(&shown, index)?)))
            else {
                return actions(cx, &nav, &shown, open, None);
            };
            let read = Doors::of(cx).follow_history::<wire::RunOutput>(wire::RUN_OUTPUT, &run);
            let (nav, shown) = (nav.clone(), shown.clone());
            Box::new(cx.arriving(&read.answer(), move |cx, whole| {
                actions(cx, &nav, &shown, open, Some((index, whole)))
            }))
        })
    }));
    page(parts)
}

/// Stop, Take over and Undo, as the run allows them.
pub(crate) fn commands(doors: &Doors, seen: &Seen, run: &wire::AgentRun) -> Element {
    let mut row = Row::new().with_rhythm(Rhythm::Related);
    if seen.open(run).is_some() && seen.working(run) {
        let (doors, seen, run) = (doors.clone(), seen.clone(), run.clone());
        row = row.push(Box::new(
            Button::new("console-run-stop", "console-run-stop")
                .with_icon(Icon::Square)
                .role(ButtonRole::Destructive)
                .on_view(move || {
                    stop(&doors, &seen, &run);
                }),
        ));
    }
    if take_over_target(seen, run).is_some() {
        let (doors, seen, run) = (doors.clone(), seen.clone(), run.clone());
        row = row.push(Box::new(
            Button::new("console-run-take-over", "console-run-take-over")
                .with_icon(Icon::Hand)
                .on_view(move || take_over(&doors, &seen, &run)),
        ));
    }
    let (undo, undone) = (doors.clone(), run.clone());
    row = row.push(Box::new(
        Button::new("console-run-undo", "console-run-undo")
            .with_icon(Icon::Undo2)
            .with_enabled(run.undo_blocked.is_none())
            .on_view(move || {
                undo_run(&undo, &undone);
            }),
    ));
    let link = doors.clone();
    let resource = run_resource_id(&run.id);
    row = row.push(Box::new(
        Button::new("console-run-copy-link", "console-run-copy-link")
            .with_icon(Icon::Link)
            .on_view(move || link.share_link(&resource)),
    ));
    Box::new(row)
}

/// Where taking `run` over opens: the record of its last action, else its workspace in the
/// full desktop.
fn take_over_target(seen: &Seen, run: &wire::AgentRun) -> Option<Result<wire::Target, String>> {
    run.actions
        .last()
        .map(|action| Ok(action.view.target().clone()))
        .or_else(|| {
            seen.workspace(run)
                .map(|workspace| Err(workspace.open.clone()))
        })
}

/// End `run` now, keeping its grant, when it is working.
pub(crate) fn stop(doors: &Doors, seen: &Seen, run: &wire::AgentRun) -> bool {
    match seen.open(run).filter(|_| seen.working(run)) {
        Some(open) => {
            doors.door(door::STOP, &open);
            true
        }
        None => false,
    }
}

/// Take `run` over: stop the agent and open where it was working.
pub(crate) fn take_over(doors: &Doors, seen: &Seen, run: &wire::AgentRun) {
    stop(doors, seen, run);
    match take_over_target(seen, run) {
        Some(Ok(target)) => doors.history(wire::OPEN, &target),
        Some(Err(href)) => doors.open_in_tilo(&href),
        None => {}
    }
}

/// Undo all of `run`, when the history lets it.
pub(crate) fn undo_run(doors: &Doors, run: &wire::AgentRun) -> bool {
    if run.undo_blocked.is_some() {
        return false;
    }
    doors.history(wire::UNDO_RUN, &run.range);
    true
}

const fn kind_icon(view: &wire::ActionView) -> Icon {
    match view {
        wire::ActionView::File { .. } => Icon::FileText,
        wire::ActionView::Command { .. } => Icon::Terminal,
        wire::ActionView::Page { .. } => Icon::Globe,
        wire::ActionView::Record { .. } => Icon::AppWindow,
    }
}

/// The run's actions, oldest first, the one `open` names with its view under it.
fn actions(
    cx: &ViewCx<Console>,
    nav: &Nav,
    run: &wire::AgentRun,
    open: Option<usize>,
    full: Option<(usize, &wire::RunOutput)>,
) -> Element {
    let doors = Doors::of(cx);
    let mut rows: Vec<Element> = Vec::new();
    for (index, action) in run.actions.iter().enumerate() {
        let target = action.view.target();
        let mut row = ListRow::new(format!("console-action-{index}"), action.summary.clone())
            .icon(kind_icon(&action.view))
            .secondary(target.label.clone())
            .meta(ago(action.at_ms))
            .selected(open == Some(index));
        if let Some(offer) = &action.undo {
            let doors = doors.clone();
            let query = wire::RecordQuery {
                range: run.range.clone(),
                task: None,
                app_id: action.app_id.clone(),
                id: action.record.clone(),
            };
            let (kind, range) = (offer.action, action.range.clone());
            row = row.trailing(Box::new(
                Button::new(&format!("console-action-{index}-undo"), &offer.label)
                    .role(ButtonRole::Plain)
                    .on_view(move || match kind {
                        wire::DetailAction::UndoRecord => {
                            doors.history(wire::UNDO_RECORD, &query);
                        }
                        _ => doors.history(wire::UNDO, &range),
                    }),
            ));
        }
        let chosen = nav.clone();
        row = row.on_view(move || {
            chosen
                .action
                .update(|open| *open = (*open != Some(index)).then_some(index));
        });
        rows.push(Box::new(row));
        if open == Some(index) {
            rows.push(action_view(
                cx,
                nav,
                &doors,
                index,
                action,
                full.filter(|(shown, _)| *shown == index)
                    .map(|(_, whole)| whole),
            ));
        }
    }
    Box::new(Section::titled(
        trf(
            "console-run-timeline",
            &[("count", run.actions.len().into())],
        ),
        rows,
    ))
}

/// What one action shows when opened: never just its sentence.
fn action_view(
    cx: &ViewCx<Console>,
    nav: &Nav,
    doors: &Doors,
    index: usize,
    action: &wire::RunAction,
    whole: Option<&wire::RunOutput>,
) -> Element {
    let id = format!("console-action-{index}-view");
    let target = action.view.target().clone();
    let open = {
        let (doors, target) = (doors.clone(), target.clone());
        Button::new(&format!("{id}-open"), "console-action-open")
            .with_icon(Icon::ExternalLink)
            .on_view(move || doors.history(wire::OPEN, &target))
    };
    let mut parts: Vec<Element> = Vec::new();
    match &action.view {
        wire::ActionView::File { path, lines, .. } => {
            parts.push(code_line(path, None));
            if lines.is_empty() {
                parts.push(compared(cx, doors, &action.range));
            } else {
                parts.push(well(lines.iter().map(diff_line).collect()));
            }
        }
        wire::ActionView::Command {
            command,
            cwd,
            output,
            more,
            exit,
            started_ms,
            ended_ms,
            run,
            ..
        } => {
            if !cwd.is_empty() {
                parts.push(Box::new(
                    Text::new(&trf("console-command-in", &[("cwd", cwd.clone().into())]))
                        .with_role(TextRole::Caption)
                        .dimmed()
                        .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph),
                ));
            }
            if run.is_some() && *more {
                let output = Output {
                    index,
                    prompt: format!("$ {command}"),
                    tail: output.clone(),
                };
                parts.push(output_part(&nav.full_output, whole, &output));
            } else {
                let mut lines = vec![code_line(&format!("$ {command}"), None)];
                lines.extend(output.iter().map(|line| code_line(line, None)));
                parts.push(well(lines));
            }
            parts.push(command_status(
                action.view.is_running(),
                *exit,
                *started_ms,
                *ended_ms,
            ));
        }
        wire::ActionView::Page { url, title, .. } => {
            let title = if title.is_empty() {
                &target.label
            } else {
                title
            };
            parts.push(Box::new(
                Text::new(title).with_role(TextRole::Label).strong(),
            ));
            parts.push(code_line(url, None));
        }
        wire::ActionView::Record { .. } => {
            let mut place = vec![Fact::with_icon(Icon::AppWindow, target.label.clone())];
            place.extend(target.app_label.map(Fact::text));
            parts.push(Box::new(Facts::new(place)));
            parts.push(compared(cx, doors, &action.range));
        }
    }
    parts.push(Box::new(
        Row::new().with_rhythm(Rhythm::Related).push(Box::new(open)),
    ));
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Related);
    for part in parts {
        column = column.push(part);
    }
    Box::new(
        Container::new()
            .with_inset(Rhythm::Related)
            .with_id(&id)
            .with_child(Box::new(column)),
    )
}

/// How a command ended, as a caption: still running, or its exit status and how long it
/// took, a failure in the danger ink.
fn command_status(
    running: bool,
    exit: Option<i32>,
    started_ms: Option<i64>,
    ended_ms: Option<i64>,
) -> Element {
    if running {
        return Box::new(
            Text::new(&tr("console-command-running"))
                .with_role(TextRole::Caption)
                .tone(Tone::Info)
                .with_id("console-command-running"),
        );
    }
    let Some(code) = exit else {
        return Box::new(Column::new().with_no_padding().with_intrinsic_height());
    };
    let took = started_ms
        .zip(ended_ms)
        .and_then(|(started, ended)| u64::try_from(ended.saturating_sub(started)).ok())
        .map(|ms| ui::format::duration(std::time::Duration::from_millis(ms)));
    let said = match took {
        Some(took) => trf(
            "console-command-exit-after",
            &[("code", code.into()), ("took", took.into())],
        ),
        None => trf("console-command-exit", &[("code", code.into())]),
    };
    let said = Text::new(&said).with_role(TextRole::Caption);
    Box::new(if code == 0 {
        said.dimmed()
    } else {
        said.tone(Tone::Danger)
    })
}

/// One command's output as its view shows it: the command's place in the run, its prompt
/// line and the last lines it printed.
struct Output {
    index: usize,
    prompt: String,
    tail: Vec<String>,
}

/// A command's output: its last lines, or every line once the person asks, read from the
/// run's record as the history serves it.
fn output_part(
    shown: &LocalState<Option<usize>>,
    whole: Option<&wire::RunOutput>,
    output: &Output,
) -> Element {
    let index = output.index;
    let full = whole.is_some();
    let toggle = {
        let shown = shown.clone();
        Button::new(
            &format!("console-action-{index}-output"),
            if full {
                "console-command-less"
            } else {
                "console-command-full"
            },
        )
        .role(ButtonRole::Plain)
        .on_view(move || shown.update(|open| *open = (!full).then_some(index)))
    };
    let printed: Element = match whole {
        Some(whole) if whole.unavailable => {
            Box::new(Text::new(&tr("console-command-output-unavailable")).dimmed())
        }
        Some(whole) => well(
            std::iter::once(&output.prompt)
                .chain(&whole.lines)
                .map(|line| code_line(line, None))
                .collect(),
        ),
        None => well(
            std::iter::once(&output.prompt)
                .chain(&output.tail)
                .map(|line| code_line(line, None))
                .collect(),
        ),
    };
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Related)
            .push(printed)
            .push(Box::new(toggle)),
    )
}

/// The run record of the command at `index` of `run`, when it printed more than its view
/// shows.
fn more_of(run: &wire::AgentRun, index: usize) -> Option<wire::CommandRun> {
    match &run.actions.get(index)?.view {
        wire::ActionView::Command {
            run: Some(record),
            more: true,
            ..
        } => Some(record.clone()),
        _ => None,
    }
}

/// What the action's changes did, as the history compares the workspace either side of
/// them: each field before and after, or else each change in a line.
fn compared(cx: &ViewCx<Console>, doors: &Doors, range: &wire::Range) -> Element {
    let read = doors.follow_history::<wire::Detail>(
        wire::DETAIL,
        &wire::DetailQuery {
            range: range.clone(),
            compare: true,
        },
    );
    Box::new(cx.arriving(&read.answer(), move |_, detail| {
        if !detail.changes.is_empty() {
            let mut lines = Vec::new();
            for change in &detail.changes {
                lines.push(code_line(&change.field, Some(TextRole::Label)));
                if !change.before.is_empty() {
                    lines.push(diff_line(&wire::DiffLine {
                        side: wire::DiffSide::Removed,
                        text: change.before.clone(),
                    }));
                }
                if !change.after.is_empty() {
                    lines.push(diff_line(&wire::DiffLine {
                        side: wire::DiffSide::Added,
                        text: change.after.clone(),
                    }));
                }
            }
            return well(lines);
        }
        match &detail.comparison {
            Some(wire::Comparison::Changes { lines }) if !lines.is_empty() => {
                well(lines.iter().map(|line| code_line(line, None)).collect())
            }
            _ => Box::new(Text::new(&tr("console-action-no-preview")).dimmed()),
        }
    }))
}

/// A line of code or output, in the code role unless `role` says otherwise.
fn code_line(text: &str, role: Option<TextRole>) -> Element {
    let text = if text.is_empty() { "\u{a0}" } else { text };
    Box::new(
        Text::new(text)
            .with_role(role.unwrap_or(TextRole::Code))
            .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph),
    )
}

/// A line of a file's change: what it removed in the danger ink, what it added in the
/// success ink, the rest as it reads.
fn diff_line(line: &wire::DiffLine) -> Element {
    let (mark, tone) = match line.side {
        wire::DiffSide::Both => ("  ", None),
        wire::DiffSide::Removed => ("− ", Some(Tone::Danger)),
        wire::DiffSide::Added => ("+ ", Some(Tone::Success)),
    };
    let text = Text::new(&format!("{mark}{}", line.text))
        .with_role(TextRole::Code)
        .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph);
    Box::new(match tone {
        Some(tone) => text.tone(tone),
        None => text.dimmed(),
    })
}

/// `lines` in a raised well, as code and output read everywhere else.
fn well(lines: Vec<Element>) -> Element {
    let column = lines.into_iter().fold(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Tight),
        Column::push,
    );
    Box::new(Surface::new(SurfaceLevel::Raised, Box::new(column)).padded(Rhythm::Tight))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_by(for_reader: bool, on_behalf_of: Option<&str>) -> wire::Author {
        wire::Author {
            name: "Claude Code".to_owned(),
            relation: wire::Relation::Software,
            kind: ui::tilo_actions::ActorKind::Agent,
            for_reader,
            on_behalf_of: on_behalf_of.map(str::to_owned),
            ..wire::Author::default()
        }
    }

    /// A run's row names who started it, not the agent its face already shows.
    #[test]
    fn a_run_row_says_who_started_it() {
        assert_eq!(started_by(&run_by(true, Some("Ada"))), "Started by you");
        assert_eq!(started_by(&run_by(false, Some("Ada"))), "Started by Ada");
        assert_eq!(started_by(&run_by(false, None)), "Claude Code");
    }
}

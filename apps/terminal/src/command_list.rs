//! A session's commands as a window without a shell of its own shows them:
//! a page's. Each command the session kept, with who typed it, how it ended
//! and what it printed; and, for a session the workspace's server keeps, a
//! command line that sends what the person types to that shell, follows it
//! while it works and stops it. A session whose shell is on someone's own
//! computer stays there: its commands show, and the line says where it is.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use ui::LocalState;
use ui::framework::{People, Query, ResourceApp as _, ResourceEnv, ViewCx};
use ui::prelude::{Arrival, Button, Column, Container, Element, Row, Scrollable, Text, TextInput};
use ui::tilo_actions::remote::{InvokeArgument, RESOURCE_PROVIDER, RemoteActionTarget};
use ui::tokens::{Rhythm, TextRole};
use ui::widget::{ButtonRole, ListRow, Surface, SurfaceLevel};

use crate::i18n::{t, t_args};
use crate::resource::{Command, Terminal, TerminalShell};

/// The most of a command's output a row shows: its last lines, where a
/// build's errors and a test summary are.
const SHOWN_LINES: usize = 200;

/// A command the person typed here that the server is working on: its line
/// and the call following it, whose answers are what it printed so far and
/// then how it ended. Dropping the call cancels it, which stops the command.
struct Working {
    line: String,
    call: Query<serde_json::Value>,
}

/// Where the window's session takes its commands: its resource's own verb,
/// asked of whoever keeps the session (a page's workspace server).
#[derive(Clone)]
struct Session {
    env: ResourceEnv<Terminal>,
    provider: String,
    resource: String,
    working: Rc<RefCell<BTreeMap<u64, Working>>>,
    /// The working commands' keys, in the order they were typed: what the
    /// list is built again on.
    shown: LocalState<Vec<u64>>,
}

impl Session {
    fn send(&self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let mut arguments = serde_json::Map::new();
        arguments.insert("command".into(), line.into());
        let target = RemoteActionTarget {
            provider: self.provider.clone(),
            address: Terminal::RUN.on(&self.resource),
        };
        let call = self.env.query(target, InvokeArgument::Json(arguments));
        let key = self
            .working
            .borrow()
            .keys()
            .next_back()
            .map_or(0, |last| last.saturating_add(1));
        self.working.borrow_mut().insert(
            key,
            Working {
                line: line.to_owned(),
                call,
            },
        );
        self.shown.update(|shown| shown.push(key));
    }

    /// Stop the command at `key`: its call is cancelled, which ends it in the
    /// server's shell, and the session keeps it as stopped.
    fn stop(&self, key: u64) {
        drop(self.working.borrow_mut().remove(&key));
        self.shown
            .update(|shown| shown.retain(|shown| *shown != key));
    }
}

/// The window: the session's commands, then the line the next is typed on.
pub(crate) fn view(cx: &ViewCx<Terminal>) -> Element {
    let document = cx
        .document_id()
        .map(|document| document.route().to_owned())
        .unwrap_or_default();
    let session = Session {
        env: ResourceEnv::clone(cx),
        provider: format!("{RESOURCE_PROVIDER}:{}:{document}", TerminalShell::ID),
        resource: cx.actions().resource_id(),
        working: Rc::default(),
        shown: cx.local(Vec::new()),
    };
    let state = cx.value(Terminal::clone);
    let (people, content) = (cx.people(), cx.content());
    let listed = session.clone();
    let list = cx.switch(&state, move |cx, terminal| {
        let (session, people, content) = (listed.clone(), people.clone(), content.clone());
        let terminal = terminal.clone();
        cx.switch(&listed.shown.clone(), move |cx, shown| {
            commands(cx, &terminal, &people, content.as_ref(), &session, shown)
        })
    });
    let draft = cx.local(String::new());
    let read_only = cx.read_only();
    let line = cx.switch(&state, move |_, terminal| {
        command_line(terminal, &session, &draft, read_only)
    });
    Box::new(
        Column::new()
            .with_no_padding()
            .push(Box::new(
                Scrollable::new_vertical()
                    .fill_parent_height()
                    .with_child(Box::new(
                        Container::new()
                            .readable()
                            .centered()
                            .with_inset(Rhythm::Group)
                            .with_child(list),
                    )),
            ))
            .push(line),
    )
}

/// Whether the session's shell is on someone's computer: a window there
/// set down where its shell is, or took the session for its process. A
/// session no device took is the workspace's, kept by its server.
pub(crate) fn on_a_computer(terminal: &Terminal) -> bool {
    terminal.pane.is_some() || terminal.target.is_some()
}

/// Every command the session kept, oldest first, then those typed here that
/// are still working.
fn commands(
    cx: &ViewCx<Terminal>,
    terminal: &Terminal,
    people: &People,
    content: Option<&tilo_content::ContentStore>,
    session: &Session,
    shown: &[u64],
) -> Element {
    let mut ended: Vec<(&ui::tilo_resource::Id, &Command)> = terminal.commands.iter().collect();
    ended.sort_by_key(|(_, command)| command.started_ms);
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Group);
    if ended.is_empty() && shown.is_empty() {
        column = column.push(Box::new(
            Text::new(&t("terminal-commands-none"))
                .dimmed()
                .with_id("terminal-commands-none"),
        ));
    }
    for (id, command) in ended {
        column = column.push(kept(id.as_str(), command, people, content));
    }
    let working = session.working.borrow();
    for key in shown {
        let Some(command) = working.get(key) else {
            continue;
        };
        // Once it ended and the session holds it, it shows with the others.
        let answered = command.call.answer().with(|answer| match answer {
            Arrival::Ready(value) => value["id"].as_str().map(str::to_owned),
            Arrival::Pending { .. } | Arrival::Failed(_) => None,
        });
        if answered.is_some_and(|id| {
            terminal
                .commands
                .iter()
                .any(|(kept, _)| kept.as_str() == id)
        }) {
            continue;
        }
        column = column.push(working_row(cx, *key, command, session));
    }
    Box::new(column)
}

/// One command the session kept: its line, who typed it and how it ended,
/// and what it printed.
fn kept(
    id: &str,
    command: &Command,
    people: &People,
    content: Option<&tilo_content::ContentStore>,
) -> Element {
    // Who typed it, by name and face, a face an agent's; else where it ran.
    let mut row = ListRow::new(
        format!("terminal-command-{id}"),
        format!("$ {}", command.command.0),
    )
    .secondary(command.cwd.0.clone())
    .meta(status(command));
    if let Some(actor) = &command.actor {
        let actor = people.actor_of(actor);
        row = row
            .secondary(actor.name.clone())
            .avatar(actor.name.clone(), actor.identity().to_owned())
            .avatar_hue(actor.hue);
        if actor.agent() {
            row = row.agent_face();
        }
    }
    let printed = command
        .output
        .inline()
        .map(<[u8]>::to_vec)
        .or_else(|| content.and_then(|store| command.output.read(store)));
    let output = match printed {
        Some(bytes) => well(&plain(&bytes), command.truncated),
        None => Box::new(
            Text::new(&t("terminal-commands-output-arriving"))
                .with_role(TextRole::Caption)
                .dimmed(),
        ) as Element,
    };
    block(Box::new(row), output)
}

/// A command typed here that the server is working on: what it printed so
/// far and the stop, then how it ended until the session holds it.
fn working_row(cx: &ViewCx<Terminal>, key: u64, command: &Working, session: &Session) -> Element {
    let line = format!("$ {}", command.line);
    let session = session.clone();
    Box::new(cx.arriving(&command.call.answer(), move |_, value| {
        let (said, printed, ended) = match value.get("exit").and_then(serde_json::Value::as_i64) {
            Some(exit) => (
                t_args("terminal-commands-exit", &[("code", exit.into())]),
                value["output"].as_str().unwrap_or_default(),
                true,
            ),
            None => (
                t("terminal-commands-working"),
                value["tail"].as_str().unwrap_or_default(),
                false,
            ),
        };
        let mut row = ListRow::new(format!("terminal-working-{key}"), line.clone()).meta(said);
        if !ended {
            let session = session.clone();
            row = row.trailing(Box::new(
                Button::new(
                    &format!("terminal-working-{key}-stop"),
                    "terminal-commands-stop",
                )
                .role(ButtonRole::Destructive)
                .on_open(move || session.stop(key)),
            ));
        }
        block(Box::new(row), well(&plain(printed.as_bytes()), false))
    }))
}

/// The line the next command is typed on, for a session the server keeps;
/// for one on someone's computer, where it is.
fn command_line(
    terminal: &Terminal,
    session: &Session,
    draft: &LocalState<String>,
    read_only: bool,
) -> Element {
    if on_a_computer(terminal) {
        return Box::new(
            Container::new()
                .with_inset(Rhythm::Related)
                .with_child(Box::new(
                    Text::new(&t("terminal-commands-on-a-computer"))
                        .with_role(TextRole::Caption)
                        .dimmed()
                        .with_id("terminal-commands-on-a-computer"),
                )),
        );
    }
    if read_only {
        return Box::new(Column::new().with_no_padding());
    }
    let submitted = (session.clone(), draft.clone());
    let input = TextInput::from_state(draft)
        .with_id("terminal-command")
        .with_placeholder("terminal-commands-placeholder")
        .on_submit(move |line| {
            submitted.0.send(line);
            submitted.1.set(String::new());
        });
    let pressed = (session.clone(), draft.clone());
    let send = Button::new("terminal-command-send", "terminal-commands-send").on_open(move || {
        pressed.0.send(&pressed.1.get());
        pressed.1.set(String::new());
    });
    Box::new(
        Container::new()
            .with_inset(Rhythm::Related)
            .with_child(Box::new(
                Row::new()
                    .no_wrap()
                    .with_rhythm(Rhythm::Related)
                    .push(Box::new(
                        Container::new()
                            .with_flex_grow()
                            .with_no_padding()
                            .with_child(Box::new(input)),
                    ))
                    .push(Box::new(send)),
            )),
    )
}

/// How a command ended: its exit status and how long it took.
fn status(command: &Command) -> String {
    let took = u64::try_from(command.ended_ms.saturating_sub(command.started_ms))
        .ok()
        .map(|ms| ui::format::duration(std::time::Duration::from_millis(ms)));
    match took {
        Some(took) => t_args(
            "terminal-commands-exit-after",
            &[("code", command.exit.into()), ("took", took.into())],
        ),
        None => t_args("terminal-commands-exit", &[("code", command.exit.into())]),
    }
}

/// A command's row over what it printed.
fn block(row: Element, output: Element) -> Element {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Tight)
            .push(row)
            .push(output),
    )
}

/// What a command printed: its last lines in the code face.
fn well(text: &str, truncated: bool) -> Element {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Box::new(Column::new().with_no_padding());
    }
    let shown = lines
        .get(lines.len().saturating_sub(SHOWN_LINES)..)
        .unwrap_or_default();
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Tight);
    if truncated || shown.len() < lines.len() {
        column = column.push(Box::new(
            Text::new(&t("terminal-commands-output-cut"))
                .with_role(TextRole::Caption)
                .dimmed(),
        ));
    }
    for line in shown {
        let line = if line.is_empty() { "\u{a0}" } else { line };
        column = column.push(Box::new(
            Text::new(line)
                .with_role(TextRole::Code)
                .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph),
        ));
    }
    Box::new(Surface::new(SurfaceLevel::Raised, Box::new(column)).padded(Rhythm::Tight))
}

/// `bytes` as the text a terminal leaves on screen, without its escape sequences: the engine's
/// reading, with tab stops widened so the page's text lays out as the grid did.
pub(crate) fn plain(bytes: &[u8]) -> String {
    tilo_terminal::parse::plain(&String::from_utf8_lossy(bytes)).replace('\t', "    ")
}

#[cfg(test)]
#[path = "command_list_tests.rs"]
mod tests;

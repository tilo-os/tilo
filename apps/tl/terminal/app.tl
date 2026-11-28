app so.tilo.terminal "Terminal" (
    icon: .terminal,
    category: .development,
    tagline: "A shell on this device, in a workspace's environment or on a Linux VM",
    keywords: ["terminal", "shell", "console", "command line", "bash", "zsh", "fish", "ssh", "konsole", "gnome terminal", "iterm", "windows terminal", "xterm", "terminal emulator", "cli", "prompt", "tty", "commands", "cmd", "powershell", "command prompt", "kommandozeile", "befehlszeile", "eingabeaufforderung"],
    document: session,
)

use tilo.environment as environment

op "inspect"
op "open"
op "close" leaves
op "environments"
op "run" changes device
op "rerun_test"
op "interrupt"
op "send"
op "paste"
op "clear"
op "read_screen"
op "inspect_editor"
op "replace_editor_selection"
op "answer_history"

// ---------------------------------------------------------------- what a terminal keeps
/// How a window's shell stands.
enum SessionState {
    /// The shell is being opened.
    opening
    /// The shell runs; what it prints is on its grid.
    open
    /// The session runs on another device or for another member: the window shows what it
    /// recorded and starts no shell of its own until the person asks.
    elsewhere
    /// The shell could not start.
    failed
    /// The connection to the shell was lost; the window opens it again.
    lost
    /// The shell exited.
    ended
}

/// What one thing a run's parsers found in its output is.
enum ItemKind {
    error
    warning
    location
    url
    test_passed
    test_failed
    test_ignored
    change
}

/// One thing a run's parsers found in its output: a diagnostic, a location, a link, a test, a
/// changed file.
type CommandItem {
    kind: ItemKind = .location
    /// What the output says about it.
    text: Text = ""
    /// The file it is in, or the address a link opens.
    path: Text = ""
    line: Int = 0
    column: Int = 0
    /// A diagnostic's code, a test's name, a change's status.
    code: Text = ""
}

/// One command a session ran: what was typed, where it ran and what it answered.
type Command (title: command) {
    /// The command line, as the shell marked it.
    command: Text = ""
    /// The folder it ran in.
    cwd: Text = ""
    /// Where it ran: the session's encoded target.
    environment: Text = ""
    /// Who ran it: the person at the keyboard, or whoever asked with `run`.
    actor: Option<Actor> = .none
    started_ms: Int = 0
    ended_ms: Int = 0
    /// Its exit status.
    exit: Int = 0
    /// What it printed, as the session's shell marked it: its end, when it printed more. Large
    /// output is kept in the device's content store, not in the record.
    output: Bytes
    /// It printed more than the output keeps.
    truncated: Bool = false
    /// The tool whose parser read it.
    tool: Text = ""
    /// It changed something a later action can put back.
    undoable: Bool = false
    /// The id its caller recorded it under, when one did: a run recorded again is the same one.
    run: Text = ""
    /// What its parsers found.
    items: List<CommandItem>
}

/// One command line a terminal ran, kept in the workspace's history for Up to recall.
type Remembered (title: command, readers: author) {
    command: Text = ""
    cwd: Text = ""
    /// Where it ran: the session's encoded target.
    environment: Text = ""
    exit: Int = 0
    ended_ms: Int = 0
    actor: Option<Actor> = .none
    /// The session that ran it, and its run there.
    session: Text = ""
    run: Text = ""
}

/// The workspace's terminal: every command line its terminals ran, which Up recalls.
type Terminal (title: name, untitled: "Terminal history") {
    name: Text = ""
    /// Every command line typed in a terminal, oldest first.
    history: Collection<Remembered>
}

/// One vault item a program in the workspace's machine gets, by the variable it is handed in.
type MachineSecret (title: variable) {
    variable: Text
    item: Text
}

/// The vault items the workspace's machine hands the programs it starts.
type MachineSecrets {
    secrets: Collection<MachineSecret>
}

/// What this device knows of where its terminals run, and the environment its person used last,
/// which a new window opens in.
type Here {
    /// The device runs environments at all.
    available: Bool = false
    /// The environment a new window opens in, encoded; empty for the default.
    recent: Text = ""
    /// This device, as the sessions it holds name it: drawn once, the first time it opened one.
    device: Text = ""
}

/// A terminal window: where its shell runs, the shell it shows, and the commands it ran.
type Session (title: title, untitled: "Terminal") {
    /// The window's title, as the program set it; empty shows the app's.
    title: Text = ""
    /// Where the session runs, encoded: `host`, `environment`, `environment:<record>` or
    /// `linux-vm:<vm>`. It outlives every shell that runs in it, so reopening goes back to it.
    target: Text = ""
    /// The session on a relay this one re-attaches to, when it was carried.
    relay: Text = ""
    /// The pane the session's presence is scoped to.
    pane: Text = ""
    /// The device whose window holds the session's shell.
    device: Text = ""
    /// Who set the session up there.
    holder: Option<Actor> = .none
    /// The live shell on that device, by the id its engine gave it; empty until one opened.
    shell: Text = ""
    /// The program at the prompt, as the shell named itself.
    shell_name: Text = ""
    /// How the shell stands.
    state: SessionState = .opening
    /// Why the shell could not start, or why its connection was lost.
    trouble: Text = ""
    /// A command the session's host holds until someone acts, as it said why.
    held: Text = ""
    /// The commands the session ran, oldest first.
    commands: Collection<Command>
}

/// A window's own session, which the app opens as each of its windows.
root session: Session

root terminal: Terminal

root machine: MachineSecrets

device root here: Here

/// What the person keeps for their terminals, on every device of theirs.
prefs Preferences {
    /// Finishing a selection also copies it to the clipboard, not only to the primary selection.
    copy_on_select: Bool = false
    /// The grid flashes when a program rings the bell.
    bell_flash: Bool = false
    /// A program may put text on the clipboard (OSC 52). Off unless the person allows it: anything
    /// printing to the terminal, a remote host included, could otherwise replace what they copied.
    allow_osc52_clipboard: Bool = false
}

default view session_window for Session

default view terminal_overview for Terminal

default view secrets_page for MachineSecrets

// ---------------------------------------------------------------- what the device answers
type OpenAsk {
    target: Text
    columns: Int
    rows: Int
    context: Text = ""
    record: Text = ""
    attach: Text = ""
    agent: Bool = false
}

type OpenAnswer {
    shell: Text
    shell_name: Text = ""
    target: Text = ""
    relay_session: Text = ""
}

type ShellAsk {
    shell: Text
    text: Text = ""
    agent: Bool = false
}

type EditorAsk {
    shell: Text
    lease: Text
    text: Text
}

type InspectAnswer {
    shell: Text
    shell_name: Text = ""
    relay_session: Text = ""
    ended: Bool = false
}

type ScreenAnswer {
    text: Text
    truncated: Bool = false
    links: List<ScreenLink>
}

type ScreenLink {
    id: Int
    url: Text
}

/// A run as its shell marked it, as the engine and the window's grid report it.
type Finished {
    command: Text
    cwd: Text
    started_ms: Int = 0
    ended_ms: Int = 0
    exit: Int = 0
    output: Text = ""
    truncated: Bool = false
    tool: Text = ""
    undoable: Bool = false
    items: List<FinishedItem>
    /// A background job's run: nobody asked for it by name.
    job: Int = 0
}

type FinishedItem {
    kind: Text
    text: Text = ""
    path: Text = ""
    line: Int = 0
    column: Int = 0
    code: Text = ""
}

/// What a run's job says when it ended.
type RunDetail {
    run: Finished
}

type RunAgain {
    command: Text
    test: Text = ""
}

type Listing {
    available: Bool = false
    self_hosted: Bool = false
}

/// What the shell asked of its history, as the grid reports it.
type HistoryAsk {
    key: Text
    skip: Int = 0
    limit: Int = 16
    matching: Text = ""
}

type HistoryAnswer {
    shell: Text
    key: Text
    skip: Int
    limit: Int
    matching: Text
    /// The matching lines, newest first; the engine leaves out the repeats.
    lines: List<Text>
}

/// A command a session's host holds, as its shell said why.
type Held {
    title: Text = ""
    message: Text = ""
    url: Text = ""
    agent: Bool = false
}

/// How wide and tall the first grid is until the window measures it.
fn first_columns() -> Int {
    100
}

fn first_rows() -> Int {
    30
}

/// The longest command line `run` takes, and the most text `send` types at once.
fn max_command_bytes() -> Int {
    4096
}

fn max_send_bytes() -> Int {
    8192
}

/// The commands a session keeps before the oldest are let go, and the lines the history keeps.
fn max_commands() -> Int {
    1000
}

fn max_remembered() -> Int {
    10000
}

/// The newest lines of the history one recall looks through.
fn max_recall_scan() -> Int {
    4096
}

// ---------------------------------------------------------------- opening a window
/// Opening a window rejoins its shell: the one this device still runs for it, or a new one
/// where the session runs. A session another device or member holds is only shown.
opened action attach(s: Session) uses random {
    if here.device == "" {
        here.device = random.id()
    }
    list_environments(here)
    if s.target == "" {
        s.target = here.recent
    }
    if runs_elsewhere(s, invoker) {
        s.state = .elsewhere
        return
    }
    if s.shell != "" {
        match engine.call("inspect", json.encode(ShellAsk(shell: s.shell))) {
            .ok(text) => match json.decode<InspectAnswer>(text) {
                .ok(seen) => if not seen.ended {
                    s.state = .open
                    return
                },
                .err(_) => {}
            },
            .err(_) => {}
        }
    }
    open_shell(s)
}

/// Whether the session's shell is held by another device or another member: this window then
/// claims nothing, and runs reach the shell where it runs.
fn runs_elsewhere(s: Session, me: Actor) -> Bool {
    if s.shell == "" or s.device == "" {
        return false
    }
    if s.device != here.device {
        return true
    }
    if let who = s.holder {
        return who != me
    }
    false
}

/// Opens a shell where the session says it runs and shows it in the window.
action open_shell(s: Session) uses shell {
    require s.target != "host" or invoker.kind() != .agent "an agent never opens the device's own shell"
    s.state = .opening
    s.trouble = ""
    s.held = ""
    // An agent's session that names no environment runs in the workspace's own shell, as the
    // device runs it: no environment is built for a command line.
    let target = if s.target == "" and invoker.kind() == .agent { "workspace" } else { s.target }
    match engine.call(
        "open",
        json.encode(
            OpenAsk(
                target: target,
                columns: first_columns(),
                rows: first_rows(),
                context: tool.context(),
                record: s.id,
                attach: s.relay,
                agent: invoker.kind() == .agent,
            ),
        ),
    ) {
        .ok(text) => match json.decode<OpenAnswer>(text) {
            .ok(opened) => {
                s.shell = opened.shell
                s.shell_name = opened.shell_name
                if opened.target != "" {
                    s.target = opened.target
                }
                s.relay = opened.relay_session
                s.device = here.device
                s.holder = .some(invoker)
                s.state = .open
            }
            .err(why) => { failed(s, why) }
        },
        .err(why) => {
            // A carried session that is gone is opened afresh, once, without it.
            if s.relay != "" {
                s.relay = ""
                open_shell(s)
            } else { failed(s, why) }
        }
    }
}

action failed(s: Session, why: Text) {
    went(s, .failed, why)
}

/// How the window's shell went, and why.
action went(s: Session, state: SessionState, why: Text) {
    s.state = state
    s.trouble = why
    if state == .failed or state == .ended {
        s.shell = ""
    }
}

/// Starts the shell again in the window, where it runs: after it ended, when its connection was
/// lost, or to take over a session another device holds.
destructive action restart(s: Session) (
    icon: .rotate_cw,
    label: "Restart shell",
    keywords: ["reset", "reload", "new shell"],
) {
    close_shell(s)
    open_shell(s)
}

/// Closes this session's shell on this device, ending for good what runs in it.
action close_shell(s: Session) {
    if s.shell != "" and s.device == here.device {
        let _ = engine.call("close", json.encode(ShellAsk(shell: s.shell)))
    }
    s.shell = ""
}

// ---------------------------------------------------------------- where a session runs
/// Runs the session in another environment, and opens new windows there from now on.
destructive action open_in(s: Session, target: Text) (
    icon: .settings,
    label: "Open in…",
    keywords: ["environment", "target", "machine", "vm", "move"],
) {
    require target != "" "choose where the session runs"
    require target == "host"
        or target == "environment"
        or target.starts_with("environment:")
        or target.starts_with("linux-vm:") "a session runs on this device's own shell, in an environment, or on a Linux VM"
    require target != "host" or invoker.kind() != .agent "an agent never opens the device's own shell"
    close_shell(s)
    s.relay = ""
    record_target(s, target)
    if target.starts_with("environment") {
        here.recent = target
    }
    open_shell(s)
}

/// What a person calls where `target` runs: an environment by the name it was given.
fn target_name(target: Text, all: environment.Environments) -> Text {
    if target == "host" {
        "Your shell on this device"
    } else if target == "workspace" {
        "This workspace's shell"
    } else if target.starts_with("linux-vm:") {
        let vm = target.slice(9, target.len())
        "{vm} virtual machine"
    } else if target.starts_with("environment:") and target != "environment:default" {
        let record = target.slice(12, target.len())
        match all.environments.find(e -> e.record == record) {
            .some(entry) => entry.name,
            .none => "Default environment",
        }
    } else { "Default environment" }
}

/// Learns whether this device runs environments at all.
action list_environments(h: Here) {
    match engine.call("environments", "\{\}") {
        .ok(text) => match json.decode<Listing>(text) {
            .ok(listed) => {
                h.available = listed.available
            }
            .err(_) => {}
        },
        .err(_) => {}
    }
}

/// Records where the session runs and the device of the window it is set up in.
action record_target(s: Session, target: Text, device: Option<Text> = .none) {
    require target != "" "session target is invalid"
    s.target = target
    s.device = device ?? s.device
    s.holder = .some(invoker)
}

/// Records the pane the session's presence is scoped to.
action claim_pane(s: Session, pane: Text, device: Option<Text> = .none) {
    require pane != "" "pane id is invalid"
    s.pane = pane
    s.device = device ?? s.device
    s.holder = .some(invoker)
}

/// Records the relay session this one re-attaches to, or that it re-attaches to none.
action record_relay_session(s: Session, session: Option<Text> = .none) {
    require relay_fits(session ?? "") "a relay session's id is 32 hex characters"
    s.relay = session ?? ""
}

/// Whether `id` names no relay session, or one: 32 hex characters.
/// Whether every character of `text` is one of `allowed`.
fn only_of(text: Text, allowed: Text) -> Bool {
    not text.split("").any(c -> c != "" and not allowed.contains(c))
}

fn relay_fits(id: Text) -> Bool {
    id == "" or id.len() == 32 and only_of(id, "0123456789abcdef")
}

// ---------------------------------------------------------------- driving the shell
/// Runs one command line in the session's shell, at its prompt, and answers once its shell
/// marked it finished. With no window open the app opens the session's shell for it, so an
/// agent's command runs and is recorded the same; one that can run no shell (a page) is answered
/// by the device that keeps the session.
action run(s: Session, command: Text) uses shell (
    icon: .play,
    label: "Run command",
    keywords: ["execute", "command", "type", "shell"],
) {
    require command.trim() != "" and command.len() <= max_command_bytes() "a command line of 1 to 4096 bytes"
    require not command.contains("\n") and not command.contains("\r") "one command line"
    require s.target != "host" or invoker.kind() != .agent "commands are not run in the device's own shell for anyone"
    if s.shell == "" {
        // Run with no shell open and no environment named, a command line runs in the
        // workspace's own shell, as the device runs one: no environment is built for it.
        if s.target == "" {
            s.target = "workspace"
        }
        open_shell(s)
    }
    run_in_shell(s, command)
}

/// Runs a command line in the session's open shell.
action run_in_shell(s: Session, command: Text) {
    require s.shell != "" or s.trouble == "" "the session's shell did not open: {s.trouble}"
    require s.shell != "" "the session's shell runs on the device that keeps it"
    match engine.start(
        "run",
        json.encode(
            ShellAsk(shell: s.shell, text: command.trim(), agent: invoker.kind() == .agent),
        ),
        done: j -> ran(s, j),
    ) {
        .ok(_) => {}
        .err(why) => {
            s.trouble = why
        }
    }
}

/// The run a command line became, recorded once its shell marked it finished.
action ran(s: Session, j: host.Job) uses time {
    if j.state == .failed {
        s.trouble = j.error
        return
    }
    match json.decode<RunDetail>(j.detail) {
        .ok(detail) => { record(s, detail.run, "") }
        .err(_) => {}
    }
}

/// Runs the command of an earlier run again.
action rerun(s: Session, prior: Command) (
    icon: .rotate_cw,
    label: "Run again",
    keywords: ["repeat", "again", "retry"],
) {
    run(s, prior.command)
}

/// Runs one test of an earlier run again, the way its tool runs one test.
action rerun_test(s: Session, prior: Command, test: Text) (
    icon: .rotate_cw,
    label: "Run this test again",
    keywords: ["test", "retry", "failing", "single test"],
) {
    require prior.items.any(i -> i.code == test) "the command reported no such test"
    match engine.call("rerun_test", json.encode(RunAgain(command: prior.command, test: test))) {
        .ok(text) => match json.decode<RunAgain>(text) {
            .ok(again) => { run(s, again.command) }
            .err(why) => {
                s.trouble = why
            }
        },
        .err(why) => {
            s.trouble = why
        }
    }
}

/// Stops the program in the foreground, as Ctrl+C does.
destructive action stop(s: Session) (
    icon: .square,
    label: "Stop command",
    keywords: ["interrupt", "cancel", "kill", "ctrl+c"],
) {
    require s.shell != "" "the session's shell runs on the device that keeps it"
    let _ = engine.call("interrupt", json.encode(ShellAsk(shell: s.shell)))
}

/// Types text at the prompt without pressing Enter.
destructive action send(s: Session, text: Text) (label: "Type text") {
    require text != "" and text.len() <= max_send_bytes() "1 to 8192 bytes of text"
    require not text.contains("\n") and not text.contains("\r") "text without Enter"
    require s.shell != "" "this window's shell is not running"
    require s.target != "host" or invoker.kind() != .agent "an agent never types into the device's own shell"
    let _ = engine.call("send", json.encode(ShellAsk(shell: s.shell, text: text)))
}

/// Pastes text into the shell exactly as given. Anyone but the person at the window is asked
/// first: what lands at a prompt runs.
destructive action paste(s: Session, text: Text) (
    icon: .clipboard_paste,
    label: "Paste",
    keywords: ["insert", "clipboard"],
) {
    require text.len() <= max_send_bytes() * 8 "at most 64 KiB at once"
    require s.shell != "" and s.state == .open "this window's shell is not running"
    require s.target != "host" or invoker.kind() != .agent "an agent never types into the device's own shell"
    let _ = engine.call("paste", json.encode(ShellAsk(shell: s.shell, text: text)))
}

/// Pastes what the clipboard holds: the person at the window, so nothing asks first.
action paste_clipboard(s: Session) uses clipboard {
    if let text = clipboard.paste() {
        if s.shell != "" and s.state == .open {
            let _ = engine.call("paste", json.encode(ShellAsk(shell: s.shell, text: text)))
        }
    }
}

/// Clears the screen and its scrollback.
action clear(s: Session) (
    icon: .eraser,
    label: "Clear screen",
    key: "Ctrl+Shift+K",
    keywords: ["clean", "reset", "cls", "scrollback"],
) {
    require s.shell != "" "this window's shell is not running"
    let _ = engine.call("clear", json.encode(ShellAsk(shell: s.shell)))
}

/// Copies what the window shows, without escape sequences.
action copy_visible(s: Session) uses clipboard (
    icon: .copy,
    label: "Copy visible output",
    keywords: ["copy", "output", "screen", "text"],
) {
    let text = screen_text(s)
    if text == "" {
        notice("Nothing to copy", kind: .warning)
        return
    }
    clipboard.copy(text)
    notice("Copied", kind: .success)
}

/// Copies the words a selection covers, as the grid hands them over.
action copy_selection(s: Session, text: Text = "") uses clipboard (
    icon: .copy,
    label: "Copy selection",
    keywords: ["copy", "selected"],
) {
    if text == "" {
        notice("Select text on the grid first", kind: .warning)
        return
    }
    clipboard.copy(text)
    notice("Copied", kind: .success)
}

/// Selects everything the session printed and copies it: the grid's whole output.
action select_all(s: Session) uses clipboard (
    icon: .text_cursor,
    label: "Select all output",
    keywords: ["select", "all", "everything"],
) {
    copy_visible(s)
}

/// The oldest output the session keeps and the newest it shows, as text.
action history_top(s: Session) -> Text (
    icon: .arrow_up,
    label: "Go to the top",
    keywords: ["scroll", "start", "beginning", "oldest"],
) {
    screen_text(s)
}

action history_live(s: Session) -> Text (
    icon: .arrow_down,
    label: "Go to the newest output",
    keywords: ["scroll", "end", "bottom", "live"],
) {
    screen_text(s)
}

/// What the window shows: its newest output as text, and the public links on it.
action read_screen(s: Session) -> Text (
    label: "Read output",
    keywords: ["read", "screen", "output"],
) {
    require s.shell != "" "this window's shell is not running"
    match engine.call("read_screen", json.encode(ShellAsk(shell: s.shell))) {
        .ok(text) => text,
        .err(why) => why,
    }
}

fn screen_text(s: Session) -> Text {
    if s.shell == "" {
        return ""
    }
    match engine.call("read_screen", json.encode(ShellAsk(shell: s.shell))) {
        .ok(text) => match json.decode<ScreenAnswer>(text) {
            .ok(screen) => screen.text,
            .err(_) => "",
        },
        .err(_) => "",
    }
}

/// The editor running in the session that offers its text, when one does: the lease its
/// selection is replaced under, and whether it can undo or redo.
action inspect_editor(s: Session) -> Text (label: "Inspect editor") {
    require s.shell != "" "this window's shell is not running"
    match engine.call("inspect_editor", json.encode(ShellAsk(shell: s.shell))) {
        .ok(text) => text,
        .err(why) => why,
    }
}

/// Replaces the selection of the editor running in the session, under the lease it gave,
/// instead of typing at the shell.
destructive action replace_editor_selection(s: Session, lease: Text, text: Text) -> Text (
    label: "Replace editor selection",
) {
    require s.shell != "" and s.state == .open "this window's shell is not running"
    require s.target != "host" or invoker.kind() != .agent "an agent never types into the device's own shell"
    match engine.call(
        "replace_editor_selection",
        json.encode(EditorAsk(shell: s.shell, lease: lease, text: text)),
    ) {
        .ok => "replaced",
        .err(why) => why,
    }
}

/// How the session stands: where it runs, whether its shell is live, and why not.
action inspect_session(s: Session) -> Text (label: "Inspect session") {
    let place = target_name(s.target, environment.environments)
    let standing = state_word(s.state)
    if s.trouble == "" { "{standing}: {place}" } else { "{standing}: {place}, {s.trouble}" }
}

fn state_word(state: SessionState) -> Text {
    match state {
        .opening => "opening",
        .open => "open",
        .elsewhere => "elsewhere",
        .failed => "failed",
        .lost => "lost",
        .ended => "ended",
    }
}

/// Opens a public web address the session printed, in the browser. A terminal prints what a
/// program tells it to, so it offers only an address that cannot carry a secret.
action open_link(s: Session, url: Text) (
    icon: .arrow_up_right,
    label: "Open link",
    keywords: ["url", "browser", "web", "address"],
) {
    require public_link(url) "only a plain web address opens"
    match tool.open_link(url) {
        .ok => {}
        .err(why) => { notice(why, kind: .error) }
    }
}

/// Whether `url` is a plain public web address: `http` or `https`, with no credentials, no query
/// and no fragment.
fn public_link(url: Text) -> Bool {
    let lower = url.lower()
    let rest = if lower.starts_with("https://") {
        url.slice(8, url.len())
    } else if lower.starts_with("http://") {
        url.slice(7, url.len())
    } else {
        return false
    }
    rest != ""
        and not rest.contains("?")
        and not rest.contains("#")
        and not rest.contains("@")
        and not rest.contains("\\")
        and not rest.contains(" ")
}

/// Opens one thing a run's parsers found: a link in the browser, a file in the app that opens it.
action open_item(s: Session, prior: Command, position: Int) uses files (
    icon: .arrow_up_right,
    label: "Open",
) {
    if let item = prior.items.at(position) {
        if item.kind == .url {
            open_link(s, item.path)
        } else if item.path != "" {
            let path = if item.path.starts_with("/") {
                item.path
            } else { "{prior.cwd}/{item.path}" }
            match tool.document_apps(path) {
                .ok(apps) => {
                    let chosen = apps.filter(a -> a.is_default).first() ?? apps.first()
                    if let opener = chosen {
                        match tool.open_with(path, opener.id) {
                            .ok => {}
                            .err(why) => { notice(why, kind: .error) }
                        }
                    } else {
                        notice("No app opens {item.path}", kind: .warning)
                    }
                }
                .err(why) => { notice(why, kind: .error) }
            }
        }
    }
}

// ---------------------------------------------------------------- what the grid reports
/// A run the shell marked finished, as the grid reported it: the person typed it.
action finished(s: Session, said: Text) uses time {
    match json.decode<Finished>(said) {
        .ok(run) => { record(s, run, "") }
        .err(_) => {}
    }
}

/// Records a run on the session, and in the workspace's history when a person typed it. What it
/// printed is `run`'s output, or `printed` when it was kept on this device in its place.
action record(s: Session, run: Finished, id: Text, printed: Option<File> = .none) uses time {
    if run.command.trim() == "" {
        return
    }
    let made = command_of(run, printed, s.target, .some(invoker))
    if id == "" {
        s.commands.insert(made)
    } else {
        let kept = s.commands.insert(made, key: id)
        kept.run = id
    }
    if s.commands.len() > max_commands() {
        if let oldest = s.commands.first() { s.commands.remove(oldest) }
    }
    if run.job == 0 {
        remember(
            terminal,
            run.command,
            run.cwd,
            run.exit,
            run.ended_ms,
            s.id,
            id,
            environment: s.target,
            actor: .some(invoker),
        )
    }
}

fn command_of(
    run: Finished,
    printed: Option<File>,
    environment: Text,
    actor: Option<Actor>,
) -> Command {
    Command(
        command: run.command,
        cwd: run.cwd,
        environment: environment,
        actor: actor,
        started_ms: run.started_ms,
        ended_ms: run.ended_ms,
        exit: run.exit,
        output: match printed {
            .some(f) => match f.bytes() {
                .ok(bytes) => bytes,
                .err(_) => Bytes.from_text(run.output),
            },
            .none => Bytes.from_text(run.output),
        },
        truncated: run.truncated,
        tool: run.tool,
        undoable: run.undoable,
        items: run.items.map(
            i -> CommandItem(
                kind: item_kind(i.kind),
                text: i.text,
                path: i.path,
                line: i.line,
                column: i.column,
                code: i.code,
            ),
        ),
    )
}

fn item_kind(kind: Text) -> ItemKind {
    if kind == "error" {
        .error
    } else if kind == "warning" {
        .warning
    } else if kind == "url" {
        .url
    } else if kind == "test-passed" {
        .test_passed
    } else if kind == "test-failed" {
        .test_failed
    } else if kind == "test-ignored" {
        .test_ignored
    } else if kind == "change" { .change } else { .location }
}

/// Records a command that ended on the session where its shell runs for someone who asked with
/// `run` (an agent, a script, another device), under the id its caller gave it. What it printed
/// comes as `output`, or, when it is too long to travel in the call, as `output_file`: kept on
/// this device, and named in its place.
action record_command(
    s: Session,
    command: Text,
    cwd: Text,
    started_ms: Int,
    ended_ms: Int,
    exit: Int,
    output: Text = "",
    truncated: Bool = false,
    tool: Text = "",
    undoable: Bool = false,
    items: List<FinishedItem> = [],
    request: Option<Text> = .none,
    id: Text = "",
    output_file: Option<File> = .none,
) uses time {
    require command.trim() != "" "a command line"
    require id != "" and not s.commands.any(c -> c.run == id) "a command takes an id of its own"
    require match output_file {
        .some(f) => f.here(),
        .none => true,
    } "the output it printed, kept on this device"
    record(
        s,
        Finished(
            command: command,
            cwd: cwd,
            started_ms: started_ms,
            ended_ms: ended_ms,
            exit: exit,
            output: output,
            truncated: truncated,
            tool: tool,
            undoable: undoable,
            items: items,
        ),
        id,
        printed: output_file,
    )
}

/// The program set the window's title.
action titled(s: Session, title: Text) {
    s.title = title
}

/// The shell exited: the session ends, and the window that showed it closes.
action ended(s: Session, how: Text) {
    went(s, .ended, "")
}

/// The connection to the shell was lost: the window says so and opens it again, re-attaching to
/// the session where it still runs.
action lost(s: Session, why: Text) {
    went(s, .lost, why)
    if s.relay != "" {
        s.shell = ""
        open_shell(s)
    }
}

/// The session's host holds a command until someone acts, and says why; empty text says it
/// let it go.
action held(s: Session, said: Text) {
    s.held = said
}

/// The person let go of the held command: it is cancelled at the prompt.
action let_go(s: Session) {
    s.held = ""
    if s.shell != "" {
        let _ = engine.call("interrupt", json.encode(ShellAsk(shell: s.shell)))
    }
}

/// Opens the page where the person adds the card the held command waits for.
action add_card(s: Session, url: Text) {
    require url.starts_with("https://") "the card is added on a secure page"
    match tool.open_link(url) {
        .ok => {}
        .err(why) => { notice(why, kind: .error) }
    }
}

/// The shell asked for the lines it recalls on Up: the newest of the person's history that
/// hold what was typed, each once.
action recall(s: Session, asked: Text) {
    match json.decode<HistoryAsk>(asked) {
        .ok(ask) => {
            let _ = engine.call(
                "answer_history",
                json.encode(
                    HistoryAnswer(
                        shell: s.shell,
                        key: ask.key,
                        skip: ask.skip,
                        limit: ask.limit,
                        matching: ask.matching,
                        lines: recalled(terminal, ask.matching, invoker),
                    ),
                ),
            )
        }
        .err(_) => {}
    }
}

/// The person's command lines of the history that hold `matching`, newest first: the engine
/// answers the shell with each once, past the ones it already showed.
fn recalled(t: Terminal, matching: Text, me: Actor) -> List<Text> {
    t.history
        .filter(r -> r.actor == .some(me) and r.command.contains(matching))
        .sort_by(r -> r.ended_ms, desc)
        .take(max_recall_scan())
        .map(r -> r.command)
}

// ---------------------------------------------------------------- the workspace's history
/// Keeps a command line in the workspace's history, for Up to recall.
action remember(
    t: Terminal,
    command: Text,
    cwd: Text,
    exit: Int,
    ended_ms: Int,
    session: Text,
    run: Text,
    environment: Option<Text> = .none,
    actor: Option<Actor> = .none,
) {
    require command.trim() != "" "a command line"
    t.history.insert(
        Remembered(
            command: command,
            cwd: cwd,
            environment: environment ?? "",
            exit: exit,
            ended_ms: ended_ms,
            actor: .some(actor ?? invoker),
            session: session,
            run: run,
        ),
    )
    if t.history.len() > max_remembered() {
        if let oldest = t.history.first() { t.history.remove(oldest) }
    }
}

/// Keeps a run another device recorded in the history, once: importing it again keeps nothing.
action import_run(
    t: Terminal,
    command: Text,
    cwd: Text,
    exit: Int,
    ended_ms: Int,
    session: Text,
    run: Text,
    environment: Option<Text> = .none,
    actor: Option<Actor> = .none,
) {
    if run != "" and t.history.any(r -> r.session == session and r.run == run) {
        return
    }
    remember(t, command, cwd, exit, ended_ms, session, run, environment: environment, actor: actor)
}

// ---------------------------------------------------------------- what a machine's programs get
/// Hands a vault item to every program the workspace's machine starts, in `variable`. Naming a
/// variable again replaces its item.
destructive action add_secret(m: MachineSecrets, variable: Text, item: Text) (
    icon: .key,
    label: "Add secret",
    keywords: ["vault", "environment variable", "token", "key"],
) {
    require variable_fits(variable.trim()) "a variable is letters, digits and _, not starting with a digit, at most 128 of them"
    require not kept_variable(variable.trim()) "the machine's own software reads that variable"
    require item.trim().starts_with("vault.item:") and item.trim().len() > 11 "a vault item is named by its reference, vault.item:<id>"
    let name = variable.trim()
    for kept in m.secrets.filter(x -> x.variable == name) {
        m.secrets.remove(kept)
    }
    m.secrets.insert(MachineSecret(variable: name, item: item.trim()))
}

/// Stops handing a variable to the machine's programs.
destructive action remove_secret(m: MachineSecrets, variable: Text) (
    icon: .trash,
    label: "Remove secret",
) {
    require m.secrets.any(x -> x.variable == variable.trim()) "no secret is handed to the machine in {variable}"
    for kept in m.secrets.filter(x -> x.variable == variable.trim()) {
        m.secrets.remove(kept)
    }
}

/// Whether `name` is a variable a program can be started with: letters, digits and `_`, not
/// starting with a digit.
fn variable_fits(name: Text) -> Bool {
    name != ""
        and name.len() <= 128
        and not "0123456789".contains(name.slice(0, 1))
        and only_of(name, "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_")
}

/// The variables the machine's own software reads, which no secret takes the place of.
fn kept_variable(name: Text) -> Bool {
    ["HOME", "PATH", "USER", "LOGNAME", "SHELL", "TERM", "PWD", "LANG", "TMPDIR"].contains(name)
        or name.starts_with("LD_")
        or name.starts_with("LC_")
        or name.starts_with("USH_")
        or name.starts_with("TILO_")
        or name.starts_with("XDG_")
}

// ---------------------------------------------------------------- views
/// A terminal window: the grid of its shell, with where it runs in the window's own bar.
view session_window(s: Session) window "Terminal" {
    ui.NavShell(content_kind: .document, title: if s.title == "" { "Terminal" } else { s.title }) {
        actions: {
            target_menu(s)
        }
        content: {
            session_body(s)
        }
    }
}

/// Where the session runs and where else it can, and the workspace's environments: each one's
/// tools and folders, where a session can open in it, rename it or grant it a folder, and a
/// field that makes a new one.
view target_menu(s: Session) {
    state managing: Bool = false
    state name: Text = ""
    state renaming: Text = ""
    state granting: Text = ""
    state folder: Text = ""
    state writable: Bool = true
    let all = environment.environments
    ui.MenuButton(target_name(s.target, all), icon: .chevron_down) {
        ui.MenuItem(
            "Default environment",
            checked: s.target == "environment"
                or s.target == ""
                or s.target == "environment:default",
            disabled: not here.available,
        ) {
            on choose: open_in(s, "environment")
        }
        for entry in all.environments.filter(e -> e.record != "default") {
            ui.MenuItem(entry.name, checked: s.target == "environment:{entry.record}") {
                on choose: open_in(s, "environment:{entry.record}")
            }
        }
        if invoker.kind() != .agent {
            ui.MenuItem("Your shell on this device", checked: s.target == "host") {
                on choose: open_in(s, "host")
            }
        }
        ui.MenuSeparator()
        ui.MenuItem("Manage environments…", disabled: not here.available) {
            on choose: set(managing, true)
        }
    }
    ui.ModalDialog(
        title: "Environments",
        open: managing,
        primary: if granting != "" {
            "Grant"
        } else if renaming != "" { "Rename" } else { "Create" },
        secondary: "Close",
    ) {
        body: {
            stack(gap: .group) {
                if all.environments.is_empty() {
                    ui.EmptyState(
                        "No environments yet",
                        message: "Name one below to make it.",
                        icon: .terminal,
                    )
                }
                for entry in all.environments.sort_by(e -> e.name) {
                    stack(gap: .related) {
                        ui.Text(entry.name, role: .title)
                        ui.Text(tools_line(entry), secondary: true)
                        ui.Text(folders_line(entry), secondary: true)
                        row(gap: .related) {
                            ui.Button("Open here", role: .plain) {
                                on press: open_in(s, "environment:{entry.record}")
                                    .then(set(managing, false))
                            }
                            ui.Button("Rename", role: .plain) {
                                on press: set(renaming, entry.record)
                                    .then(set(granting, ""))
                                    .then(set(name, entry.name))
                            }
                            ui.Button("Grant folder…", role: .plain) {
                                on press: set(granting, entry.record)
                                    .then(set(renaming, ""))
                                    .then(set(folder, ""))
                            }
                        }
                    } as environment_row
                }
                if granting != "" {
                    stack(gap: .related) {
                        ui.TextInput(
                            bind: folder,
                            placeholder: "The folder's path on this device",
                            autofocus: true,
                        ) as grant_folder
                        ui.Checkbox("Programs may change what is in it", bind: writable)
                    }
                } else {
                    ui.TextInput(
                        bind: name,
                        placeholder: if renaming != "" {
                            "New name"
                        } else { "Name of a new environment" },
                        autofocus: true,
                    ) as environment_name
                }
            }
        }
        on act_primary: if granting != "" {
            grant_folder(granting, folder, writable).then(set(granting, "")).then(set(folder, ""))
        } else if renaming != "" {
            rename_environment(renaming, name).then(set(renaming, "")).then(set(name, ""))
        } else {
            create_environment(name).then(set(name, ""))
        }
        on act_secondary: set(managing, false).then(set(renaming, "")).then(set(granting, ""))
        on dismiss: set(managing, false).then(set(renaming, "")).then(set(granting, ""))
    }
}

/// What an environment's programs were installed from, in a line.
fn tools_line(entry: environment.EnvironmentEntry) -> Text {
    if entry.tools.is_empty() {
        "No tools yet. Run pkg install inside the environment."
    } else { "Tools: {entry.tools.join(", ")}" }
}

/// The folders an environment may reach, in a line, each saying when it may only read there.
fn folders_line(entry: environment.EnvironmentEntry) -> Text {
    if entry.grants.is_empty() {
        "No folders yet. Use Grant folder…, or run perm trust-dir inside the environment."
    } else {
        let named = entry.grants.map(g -> if g.write { g.path } else { "{g.path} (read-only)" })
        "Folders: {named.join(", ")}"
    }
}

/// The workspace's environments, as their verbs are called on.
fn environments() -> environment.Environment {
    resource("environment", of: environment.Environment)
}

/// Makes a new environment of the workspace, called `name`.
action create_environment(name: Text) (
    label: "Create environment",
    keywords: ["environment", "new", "make"],
) {
    require name != "" "name the new environment"
    environment.create(environments(), name: name)
}

/// Calls the environment `record` `name`.
action rename_environment(record: Text, name: Text) (label: "Rename environment") {
    require name != "" "name the environment"
    environment.rename(environments(), record: record, name: name)
}

/// Lets the environment `record` reach the folder at `path` on this device, and change what is
/// in it when `write`.
action grant_folder(record: Text, path: Text, write: Bool) (
    label: "Grant a folder to an environment",
) {
    require path != "" "say which folder"
    environment.grant_dir(environments(), record: record, path: path, write: write)
}

view session_body(s: Session) {
    let prefs = root Preferences
    stack(gap: .none, fill: true, padding: .none) {
        if s.held != "" {
            held_banner(s)
        } else if s.state == .lost {
            ui.Banner(
                "Reconnecting…",
                message: s.trouble,
                kind: .warning,
                action: "Restart shell",
            ) {
                on act: restart(s)
            }
        } else if s.state == .elsewhere {
            ui.Banner(
                "This session runs on another device",
                message: "Its commands are shown here. Restart the shell to take it over on this one.",
                kind: .info,
                action: "Restart shell",
            ) {
                on act: restart(s)
            }
        }
        if s.state == .failed {
            ui.EmptyState(
                "The shell couldn't start",
                message: s.trouble,
                icon: .terminal,
                action: "Try again",
                error: true,
            ) {
                on act: open_shell(s)
            }
        } else if s.state == .opening {
            ui.EmptyState(opening_line(s), icon: .terminal)
        } else if s.state == .elsewhere {
            commands_list(s)
        } else if s.state == .ended {
            ui.EmptyState("The shell exited", icon: .terminal, action: "Restart shell") {
                on act: open_shell(s)
            }
        } else if s.shell != "" {
            ui.ContextMenu {
                ui.MenuItem("Copy", icon: .copy, key: "Ctrl+Shift+C") {
                    on choose: copy_visible(s)
                }
                ui.MenuItem(
                    "Paste",
                    icon: .clipboard_paste,
                    key: "Ctrl+Shift+V",
                    disabled: s.state != .open,
                ) {
                    on choose: paste_clipboard(s)
                }
                ui.MenuItem("Select all output", icon: .text_cursor) {
                    on choose: select_all(s)
                }
                ui.MenuSeparator()
                ui.MenuItem("Clear screen", icon: .eraser, key: "Ctrl+Shift+K") {
                    on choose: clear(s)
                }
                ui.MenuItem("Restart shell", icon: .rotate_cw) {
                    on choose: restart(s)
                }
                ui.MenuSeparator()
                ui.MenuItem("Copy on select", bind: prefs.copy_on_select)
                ui.MenuItem("Flash on a bell", bind: prefs.bell_flash)
                ui.MenuItem("Let programs set the clipboard", bind: prefs.allow_osc52_clipboard)
            }
            ui.Terminal(
                s.shell,
                label: "Terminal, {target_name(s.target, environment.environments)}",
                copy_on_select: prefs.copy_on_select,
                bell_flash: prefs.bell_flash,
                allow_clipboard: prefs.allow_osc52_clipboard,
            ) {
                on title(text): titled(s, text)
                on finished(said): finished(s, said)
                on exit(how): ended(s, how).then(close())
                on lost(why): lost(s, why)
                on selected(text): copy_selection(s, text)
                on history(asked): recall(s, asked)
                on held(said): held(s, said)
            }
        } else {
            ui.EmptyState("No shell running", icon: .terminal, action: "Restart shell") {
                on act: open_shell(s)
            }
        }
    }
}

fn opening_line(s: Session) -> Text {
    if s.target == "host" {
        "Opening your shell on this device…"
    } else {
        "Opening the shell where this workspace runs…"
    }
}

view held_banner(s: Session) {
    match json.decode<Held>(s.held) {
        .ok(why) => {
            let title = if why.title == "" { "This needs a Linux machine." } else { why.title }
            if why.url != "" {
                ui.Banner(
                    title,
                    message: "Add a card to continue.",
                    kind: .info,
                    action: "Add a card",
                    secondary_action: "Cancel",
                ) {
                    on act: add_card(s, why.url)
                    on secondary_act: let_go(s)
                }
            } else {
                ui.Banner(title, message: why.message, kind: .info, action: "Cancel") {
                    on act: let_go(s)
                }
            }
        }
        .err(_) => {
            ui.Banner("This command waits", message: s.held, kind: .info, action: "Cancel") {
                on act: let_go(s)
            }
        }
    }
}

/// The commands a session ran, newest last: what a window shows of a session it does not run.
view commands_list(s: Session) {
    if s.commands.is_empty() {
        ui.EmptyState(
            "No commands yet",
            message: "No command has run in this session.",
            icon: .terminal,
            inline: true,
        )
    } else {
        stack(gap: .related) {
            for c in s.commands.sort_by(c -> c.started_ms) {
                command_row(s, c)
            }
        }
    }
}

/// One command: who ran it, how it ended, what it printed, and what its parsers found.
view command_row(s: Session, c: Command) {
    // The menu belongs to the row it lands on: a layout runs the menu a view places in it, and
    // the row's own layout is where a right-click, the Menu key or a long press opens it.
    stack(gap: .related) {
        ui.ContextMenu {
            ui.MenuItem("Run again", icon: .rotate_cw) {
                on choose: rerun(s, c)
            }
            ui.MenuItem("Copy command", icon: .copy) {
                on choose: copy_command(c)
            }
        }
        ui.DisclosureRow(
            title: "$ {c.command}",
            secondary: command_line(c),
            icon: if c.exit == 0 { .check } else { .x },
            who: c.actor,
        ) {
            if not c.output.is_empty() {
                ui.Text(c.output.text(), role: .code)
            }
            for item in c.items.take(50) {
                ui.ListRow(
                    title: item.text,
                    secondary: item_line(item),
                    icon: if item.kind == .error {
                        .circle_x
                    } else if item.kind == .warning {
                        .triangle_alert
                    } else if item.kind == .url {
                        .link
                    } else if item.kind == .test_passed {
                        .check
                    } else if item.kind == .test_failed {
                        .x
                    } else if item.kind == .change { .file_code } else { .map_pin },
                ) {
                    on activate: open_item(s, c, c.items.index_of(item) ?? 0)
                }
                if item.kind == .test_failed and item.code != "" {
                    ui.Button("Run this test again", icon: .rotate_cw, role: .plain) {
                        on press: rerun_test(s, c, item.code)
                    }
                }
            }
        }
    }
}

/// How a command ended, as its row says.
fn command_line(c: Command) -> Text {
    let status = if c.exit == 0 { "Done" } else { "Exit {c.exit}" }
    let cut = if c.truncated { ", output cut" } else { "" }
    if c.cwd == "" { "{status}{cut}" } else { "{status}{cut}, in {c.cwd}" }
}

fn item_line(item: CommandItem) -> Text {
    if item.path == "" {
        item.code
    } else if item.line > 0 { "{item.path}:{item.line}" } else { item.path }
}

/// Copies a command line again.
action copy_command(c: Command) uses clipboard {
    clipboard.copy(c.command)
    notice("Copied", kind: .success)
}

/// The workspace's terminal: every command line its terminals ran, newest first.
view terminal_overview(t: Terminal) {
    std.page(t) {
        stack(gap: .related) {
            if t.history.is_empty() {
                ui.EmptyState(
                    "No commands yet",
                    message: "No command has run in this workspace's terminals.",
                    icon: .terminal,
                )
            } else {
                for r in t.history.sort_by(r -> r.ended_ms, desc).take(500) {
                    ui.ListRow(
                        title: r.command,
                        note: if r.exit == 0 { r.cwd } else { "Exit {r.exit}, in {r.cwd}" },
                        icon: if r.exit == 0 { .check } else { .x },
                        item: r,
                    )
                }
            }
        }
    }
}

/// What the workspace's machine hands the programs it starts.
view secrets_page(m: MachineSecrets) {
    std.page(m) {
        stack(gap: .related) {
            if m.secrets.is_empty() {
                ui.EmptyState(
                    "No secrets for the machine",
                    message: "A vault item added here is handed to every program the workspace's machine starts, in the variable you name.",
                    icon: .key,
                )
            } else {
                for x in m.secrets.sort_by(x -> x.variable) {
                    ui.ListRow(title: x.variable, secondary: x.item, icon: .key, item: x)
                }
            }
        }
    }
}

// ---------------------------------------------------------------- scenarios
scenario "a build that failed" {
    let s = root Session
    s.target = "environment"
    s.state = .elsewhere
    s.shell = "shell-elsewhere"
    s.device = "another-device"
    s.commands.insert(
        Command(
            command: "cargo test",
            cwd: "/home/ada/src/tilo",
            environment: "environment",
            started_ms: 1000,
            ended_ms: 9000,
            exit: 101,
            output: Bytes.from_text(
                "running 2 tests\ntest parse::reads_a_line ... ok\ntest parse::reads_a_long_line ... FAILED\n\nfailures:\n    parse::reads_a_long_line\n\ntest result: FAILED. 1 passed; 1 failed",
            ),
            tool: "cargo",
            items: [
                CommandItem(
                    kind: .test_passed,
                    text: "parse::reads_a_line",
                    code: "parse::reads_a_line",
                ),
                CommandItem(
                    kind: .test_failed,
                    text: "parse::reads_a_long_line",
                    path: "src/parse.rs",
                    line: 88,
                    column: 5,
                    code: "parse::reads_a_long_line",
                ),
            ],
        ),
    )
    s.commands.insert(
        Command(
            command: "git status --short",
            cwd: "/home/ada/src/tilo",
            environment: "environment",
            started_ms: 10000,
            ended_ms: 10100,
            output: Bytes.from_text(" M src/parse.rs"),
        ),
    )
}

scenario "a shell that could not start" {
    let s = root Session
    s.target = "environment"
    s.state = .failed
    s.trouble = "The workspace's machine is not running. Start it in Settings, then try again."
}

scenario "a command waits for a card" {
    let s = root Session
    s.target = "environment"
    s.state = .elsewhere
    s.shell = "shell-elsewhere"
    s.device = "another-device"
    s.held = "\{\"title\":\"This needs a Linux machine.\",\"url\":\"https://tilo.sh/billing\"\}"
}

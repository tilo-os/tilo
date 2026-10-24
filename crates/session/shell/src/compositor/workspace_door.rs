//! Actions for a program running in an app, with that app's workspace scope.
//!
//! `tilo action`, `tilo do` and `tilo open` typed in a terminal reach the
//! session's agent door (`agent_door`) in the control protocol. The admin
//! control socket refuses them: they run inside an app. The door hands such
//! a connection here, once the kernel's account of the process has named
//! the app it runs in and that app's workspace ([`attribute`]):
//!
//! - **A tab of Tilo's Terminal.** Its shell is `tilo-ush`'s child, not the
//!   Terminal's, so the process tree alone ends at the shell runtime. The
//!   runtime records which Terminal launch asked for each Host session
//!   (`ush::terminal_sessions`); the desktop accepts a record only while
//!   both processes it names are still those processes, and then
//!   attributes the Terminal through its own record of what it launched.
//!   The tab — its session's shell — is what a "this tab" grant follows.
//! - **An unconfined app this session launched** (another terminal, say),
//!   and what runs in it: attributed directly through the launch record.
//!
//! A sandboxed app is never served here: it shares neither the session's
//! namespaces nor a Host session, and acts through its own collaboration
//! connection. Nothing here widens what any socket accepts.
//!
//! What such a connection may do ([`permitted`]) is listing and invoking
//! actions — no input, no screenshots, no state dumps, never while the
//! session is locked, and never the person's own consent decisions
//! ([`hidden`]). Within that, [`reach`] sorts an action into what runs at
//! once (it works in the app's own workspace) and what the person is asked
//! about first.

use crate::shell::desktop_control::DesktopControlAsk;
pub(crate) use crate::shell::desktop_control::Holder;
use crate::shell::test_control::{self, Command};

/// The app a connection runs in, the workspace it acts for, and the tabs
/// it asked from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppScope {
    /// The app's package id (`so.tilo.terminal`).
    pub package: String,
    /// The room of the workspace the app was opened in.
    pub room: String,
    /// The process whose life a "this tab" grant lasts for: a Terminal
    /// tab's shell, or the app's own process.
    pub holder: Holder,
    /// The desktop's tabs the program was typed in: where a "this tab"
    /// grant shows. One for a Terminal tab; every window of the launch for
    /// an app that shows several.
    pub tabs: Vec<u64>,
}

/// What the kernel and the launch records say about a connection's
/// process, before the desktop found the tabs it asked from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Attribution {
    pub package: String,
    /// The launch's workspace room; `None` for a Terminal tab the desktop
    /// shows itself, whose workspace is its tab's.
    pub room: Option<String>,
    pub holder: Holder,
    pub asked_from: AskedFrom,
}

/// Where the tabs a connection asked from are found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AskedFrom {
    /// A launch this session made: its windows.
    Launch(crate::shell::launcher::VerifiedLaunchCustody),
    /// A Terminal tab the desktop shows in its own process: the one whose
    /// Terminal reported this session-supervisor session.
    HostedTerminal { session: String },
}

impl Attribution {
    /// The connection's scope, once its tabs are found: `tab_of_session`
    /// names the Terminal tab the desktop shows that runs a session,
    /// `tabs_of_launch` the tabs of a launch, `room_of_tab` the room of a
    /// tab's workspace. `None` when a Terminal tab is not (or no longer)
    /// shown.
    pub(crate) fn resolve(
        self,
        tab_of_session: impl FnOnce(&str) -> Option<u64>,
        tabs_of_launch: impl FnOnce(crate::shell::launcher::VerifiedLaunchCustody) -> Vec<u64>,
        room_of_tab: impl FnOnce(u64) -> Option<String>,
    ) -> Option<AppScope> {
        let (room, tabs) = match self.asked_from {
            AskedFrom::HostedTerminal { session } => {
                let tab = tab_of_session(&session)?;
                (room_of_tab(tab)?, vec![tab])
            }
            AskedFrom::Launch(custody) => (self.room?, tabs_of_launch(custody)),
        };
        Some(AppScope {
            package: self.package,
            room,
            holder: self.holder,
            tabs,
        })
    }
}

/// What the process `pid` acts for, from the kernel's facts and the
/// desktop's own launch records; `None` when it runs in no app this session
/// launched, or in a sandboxed one.
pub(crate) fn attribute(pid: u32) -> Option<Attribution> {
    if !shares_user_namespace(pid) {
        return None;
    }
    let records = ush::terminal_sessions::dir(std::env::var_os("XDG_RUNTIME_DIR"));
    let desktop = {
        let pid = std::process::id();
        let (_, start) = crate::shell::host_runtime::proc_stat(pid)?;
        Holder { pid, start }
    };
    attribute_with(
        pid,
        records.as_deref(),
        super::native::peer_shares_host_security_namespaces(pid),
        crate::shell::launcher::verified_launch_custody_for_descendant,
        desktop,
    )
}

/// What the desktop's launch record says a process was launched as.
pub(crate) type Custody = (
    crate::shell::launcher::VerifiedLaunchIdentity,
    crate::shell::launcher::VerifiedLaunchCustody,
);

/// [`attribute`] over the Host session records in `records`, whether `pid`
/// shares the session's namespaces, the launch records `custody`, and the
/// desktop's own process.
pub(crate) fn attribute_with(
    pid: u32,
    records: Option<&std::path::Path>,
    shares_namespaces: bool,
    custody: impl Fn(u32) -> Option<Custody>,
    desktop: Holder,
) -> Option<Attribution> {
    if let Some(records) = records
        && let Some(session) = ush::terminal_sessions::session_of(records, pid)
    {
        let holder = Holder {
            pid: session.leader_pid,
            start: session.leader_start,
        };
        // The desktop asked: a Terminal tab it shows in its own process.
        if session.requester_pid == desktop.pid && session.requester_start == desktop.start {
            return Some(Attribution {
                package: ush::host::TERMINAL_APP_ID.to_owned(),
                room: None,
                holder,
                asked_from: AskedFrom::HostedTerminal {
                    session: session.session,
                },
            });
        }
        let (identity, launch) = custody(session.requester_pid)?;
        // Only Terminal opens Host sessions; a record naming anything else
        // is not one the runtime wrote.
        if identity.package_id != ush::host::TERMINAL_APP_ID {
            return None;
        }
        return Some(Attribution {
            package: identity.package_id,
            room: Some(identity.workspace_id?),
            holder,
            asked_from: AskedFrom::Launch(launch),
        });
    }
    if !shares_namespaces {
        return None;
    }
    let (identity, launch) = custody(pid)?;
    if identity.confined {
        return None;
    }
    Some(Attribution {
        package: identity.package_id,
        room: Some(identity.workspace_id?),
        holder: Holder {
            pid: launch.supervisor_pid,
            start: launch.supervisor_start_time,
        },
        asked_from: AskedFrom::Launch(launch),
    })
}

fn shares_user_namespace(pid: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    let ours = std::fs::metadata("/proc/self/ns/user");
    let theirs = std::fs::metadata(format!("/proc/{pid}/ns/user"));
    matches!((ours, theirs), (Ok(a), Ok(b)) if a.dev() == b.dev() && a.ino() == b.ino())
}

/// Whether a scoped connection may send `request` at all: listing and
/// invoking actions on this device, and nothing else.
pub(crate) fn permitted(request: &test_control::Request) -> bool {
    request.agent.is_none()
        && request.window.is_none()
        && matches!(
            request.command,
            Command::ActionsEnumerate { .. } | Command::ActionsInvoke { device: None, .. }
        )
}

/// Providers whose rows a scoped connection never sees or invokes: the
/// person's own consent and sign-in decisions, which only the person
/// answers, in trusted chrome.
pub(crate) fn hidden(provider: &str) -> bool {
    use crate::shell::search::providers as p;
    provider == p::permission_prompts::PROVIDER_ID
        || provider == p::authentication_prompts::PROVIDER_ID
        || provider == p::terminal_device_shell::PROVIDER_ID
        || provider == p::file_picker::PROVIDER_ID
        || provider == "onboarding"
        || provider == super::diagnostic_actions::PROVIDER
}

/// Providers that open apps: in the workspace in front, as the palette does.
const APP_PROVIDERS: [&str; 3] = [
    "applications",
    "host-applications",
    crate::shell::search::providers::store_catalogue::PROVIDER_ID,
];

/// How far an action reaches beyond the app's workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
    /// It works in the app's own workspace: run it.
    Workspace,
    /// It acts on the desktop beyond that workspace: the person allows it
    /// first, for this tab or always.
    Desktop,
    /// Identity, security, updates, deleting data and the like: the person
    /// confirms it every time, whatever else they allowed.
    Sensitive,
}

/// What [`reach`] needs to know about one action.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Row<'a> {
    pub provider: &'a str,
    /// The room of the workspace the action's resource belongs to, when it
    /// belongs to one.
    pub room: Option<&'a str>,
    pub contract: Option<&'a tilo_actions::ActionContract>,
}

/// Sort `row` for an app in `scope`, with `active_room` the workspace in
/// front. Opening an app is in scope when the app's own workspace is in
/// front (that is where it opens); an action on a resource is in scope when
/// the resource is in the app's workspace; anything else reaches the desktop.
pub(crate) fn reach(row: Row<'_>, scope: &AppScope, active_room: Option<&str>) -> Reach {
    if row.contract.is_some_and(is_sensitive) {
        return Reach::Sensitive;
    }
    let in_workspace = match row.room {
        Some(room) => room == scope.room,
        None => APP_PROVIDERS.contains(&row.provider) && active_room == Some(&scope.room),
    };
    if in_workspace {
        Reach::Workspace
    } else {
        Reach::Desktop
    }
}

/// Whether an action is one the person confirms every time: it deletes or
/// overwrites, crosses a security boundary, reads private data, or asks for
/// confirmation itself.
pub(crate) fn is_sensitive(contract: &tilo_actions::ActionContract) -> bool {
    use tilo_actions::RiskLevel;
    matches!(
        contract.risk,
        RiskLevel::Destructive | RiskLevel::Privileged | RiskLevel::Sensitive
    ) || contract.confirmation != tilo_actions::ConfirmationPolicy::None
}

/// What becomes of a scoped connection's request.
#[derive(Debug)]
pub(crate) enum Decision {
    /// Served as any connection's.
    Serve,
    /// Answered with this refusal instead.
    Refuse(test_control::Response),
    /// Held until the person answers this.
    Ask(DesktopControlAsk),
}

/// What becomes of invoking `row`, titled `title`, from `scope`: run at once
/// when it works in the app's workspace, or reaches the desktop the person
/// already let this tab or app control (`allowed`), or is a sensitive action
/// the person just confirmed (`confirmed`); otherwise held for the person.
pub(crate) fn decide_invoke(
    row: Row<'_>,
    title: &str,
    scope: &AppScope,
    active_room: Option<&str>,
    allowed: bool,
    confirmed: bool,
) -> Decision {
    let ask = |sensitive| {
        Decision::Ask(DesktopControlAsk {
            package: scope.package.clone(),
            app_label: crate::shell::launcher::display_name_for_app_id_without_waiting(
                &scope.package,
            )
            .unwrap_or_else(|| scope.package.clone()),
            room: scope.room.clone(),
            action: title.to_owned(),
            sensitive,
            holder: scope.holder,
            tabs: scope.tabs.clone(),
        })
    };
    match reach(row, scope, active_room) {
        Reach::Workspace => Decision::Serve,
        Reach::Desktop if allowed => Decision::Serve,
        Reach::Desktop => ask(false),
        Reach::Sensitive if confirmed => Decision::Serve,
        Reach::Sensitive => ask(true),
    }
}

/// What the person's answer to `ask` does: `None` to run the held request
/// now, or the refusal to answer it with.
pub(crate) fn refusal_for(outcome: crate::portal::PromptOutcome) -> Option<test_control::Response> {
    (outcome == crate::portal::PromptOutcome::Deny).then(|| test_control::Response::Err {
        code: "not_allowed".into(),
        message: crate::shell::i18n::t("desktop-control-refused"),
    })
}

/// Whether a listed row is shown to `scope`: never a [`hidden`] provider's;
/// with the desktop allowed, everything else; otherwise what belongs to the
/// app's workspace or to no workspace at all. Another workspace's things
/// stay out of a listing the person did not open to the desktop.
pub(crate) fn listed(row: &serde_json::Value, scope: &AppScope, desktop_allowed: bool) -> bool {
    let provider = row
        .get("provider")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if hidden(provider) {
        return false;
    }
    desktop_allowed
        || row
            .get("room")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|room| room == scope.room)
}

/// Keep only the rows of `answer` (an `actions_enumerate` answer) that
/// [`listed`] shows `scope`.
pub(crate) fn filter_listing(
    answer: &mut serde_json::Value,
    scope: &AppScope,
    desktop_allowed: bool,
) {
    let Some(rows) = answer
        .get_mut("actions")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    rows.retain(|row| listed(row, scope, desktop_allowed));
    let count = rows.len();
    if let Some(object) = answer.as_object_mut() {
        object.insert("count".into(), count.into());
        // The launcher is the person's view of all their workspaces; a
        // program in one app's window lists actions, not that.
        object.remove("launcher");
    }
}

/// The refusal of a request a scoped connection may not send.
pub(crate) fn not_permitted() -> test_control::Response {
    test_control::Response::Err {
        code: "not_allowed".into(),
        message: crate::shell::i18n::t("desktop-control-only-actions"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> AppScope {
        AppScope {
            package: "so.tilo.terminal".into(),
            room: "home".into(),
            holder: Holder { pid: 1, start: 1 },
            tabs: vec![7],
        }
    }

    fn launched(package: &str, room: &str, confined: bool, pid: u32) -> Custody {
        (
            crate::shell::launcher::VerifiedLaunchIdentity {
                package_id: package.into(),
                workspace_id: Some(room.into()),
                confined,
            },
            crate::shell::launcher::VerifiedLaunchCustody {
                supervisor_pid: pid,
                supervisor_start_time: 9,
            },
        )
    }

    /// The desktop's own process, as the records name it.
    fn desktop() -> Holder {
        Holder {
            pid: u32::MAX,
            start: 1,
        }
    }

    /// A Host session's shell (a child of this test) and a program typed in
    /// it (the shell's child), with the runtime's record naming `requester`
    /// as the process that asked.
    struct Typed {
        shell: std::process::Child,
        program: u32,
        session: ush::terminal_sessions::Session,
        records: std::path::PathBuf,
        _dir: tempfile::TempDir,
    }

    impl Typed {
        fn in_a_session_asked_by(requester: u32) -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let records = dir.path().join("terminal-sessions");
            let mut shell = std::process::Command::new("sh")
                .args(["-c", "sleep 30 & echo $!; wait"])
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("shell");
            let mut line = String::new();
            std::io::BufRead::read_line(
                &mut std::io::BufReader::new(shell.stdout.take().expect("stdout")),
                &mut line,
            )
            .expect("the shell names its child");
            let typed: u32 = line.trim().parse().expect("a pid");
            let session = ush::terminal_sessions::mark(&records, shell.id(), requester, "s-41")
                .expect("the runtime's record");
            Self {
                shell,
                program: typed,
                session,
                records,
                _dir: dir,
            }
        }

        fn holder(&self) -> Holder {
            Holder {
                pid: self.session.leader_pid,
                start: self.session.leader_start,
            }
        }

        /// Both processes are this test's own.
        fn close(mut self) {
            let _ = std::process::Command::new("kill")
                .arg(self.program.to_string())
                .status();
            let _ = self.shell.wait();
        }
    }

    /// A program typed in a tab of a Terminal launched as its own app is
    /// the tab's shell's child, the shell `tilo-ush`'s: it is attributed to
    /// that Terminal launch, with its workspace and its windows, and a
    /// grant for "this tab" follows the shell.
    #[test]
    fn a_program_in_a_launched_terminals_tab_acts_for_that_terminal() {
        let terminal = std::process::id();
        let typed = Typed::in_a_session_asked_by(terminal);
        let custody = |pid: u32| {
            (pid == terminal).then(|| launched("so.tilo.terminal", "home", true, terminal))
        };
        let attribution = attribute_with(
            typed.program,
            Some(&typed.records),
            false,
            custody,
            desktop(),
        )
        .expect("attributed");
        assert_eq!(attribution.holder, typed.holder());
        let scope = attribution
            .resolve(
                |_| None,
                |launch| {
                    assert_eq!(launch.supervisor_pid, terminal);
                    vec![11, 12]
                },
                |_| None,
            )
            .expect("resolved");
        assert_eq!(
            (scope.package.as_str(), scope.room.as_str()),
            ("so.tilo.terminal", "home")
        );
        assert_eq!(scope.tabs, [11, 12], "the launch's windows");
        assert!(scope.holder.is_live());
        // A record naming some other app is not the runtime's.
        let other =
            |pid: u32| (pid == terminal).then(|| launched("so.tilo.notes", "home", true, pid));
        assert_eq!(
            attribute_with(typed.program, Some(&typed.records), false, other, desktop()),
            None
        );
        let holder = typed.holder();
        typed.close();
        assert!(!holder.is_live(), "the tab closed");
    }

    /// A program typed in a Terminal tab the desktop shows in its own
    /// process belongs to exactly that tab: the one whose Terminal reported
    /// the session the runtime recorded, in that tab's workspace.
    #[test]
    fn a_program_in_a_terminal_tab_the_desktop_shows_belongs_to_that_tab() {
        let me = std::process::id();
        let (_, start) = crate::shell::host_runtime::proc_stat(me).expect("own stat");
        let as_desktop = Holder { pid: me, start };
        let typed = Typed::in_a_session_asked_by(me);
        let attribution = attribute_with(
            typed.program,
            Some(&typed.records),
            false,
            |_| None,
            as_desktop,
        )
        .expect("attributed");
        assert_eq!(
            attribution.asked_from,
            AskedFrom::HostedTerminal {
                session: "s-41".into()
            }
        );
        let scope = attribution
            .clone()
            .resolve(
                |session| (session == "s-41").then_some(7),
                |_| unreachable!("no launch"),
                |tab| (tab == 7).then(|| "home".to_owned()),
            )
            .expect("resolved");
        assert_eq!(scope.tabs, [7]);
        assert_eq!(scope.room, "home");
        assert_eq!(scope.package, "so.tilo.terminal");
        assert_eq!(
            attribution.resolve(|_| None, |_| Vec::new(), |_| None),
            None,
            "a tab no longer shown acts for nothing"
        );
        typed.close();
    }

    /// Outside a Terminal tab, only an unconfined app this session launched
    /// is served, by its own launch record; a sandboxed app never is.
    #[test]
    fn only_an_unconfined_app_is_served_by_its_launch_record() {
        let me = std::process::id();
        let unconfined = |pid: u32| Some(launched("org.example.term", "work", false, pid));
        let attribution =
            attribute_with(me, None, true, unconfined, desktop()).expect("attributed");
        assert_eq!(attribution.room.as_deref(), Some("work"));
        assert_eq!(attribution.package, "org.example.term");
        assert_eq!(
            attribute_with(me, None, false, unconfined, desktop()),
            None,
            "not in the session's namespaces"
        );
        let sandboxed = |pid: u32| Some(launched("so.tilo.notes", "work", true, pid));
        assert_eq!(attribute_with(me, None, true, sandboxed, desktop()), None);
        assert_eq!(
            attribute_with(me, None, true, |_| None, desktop()),
            None,
            "launched by nobody"
        );
    }

    #[test]
    fn a_terminal_works_in_its_own_workspace_and_asks_beyond_it() {
        let scope = scope();
        let row = |provider, room| Row {
            provider,
            room,
            contract: None,
        };
        assert_eq!(
            reach(row("app-actions", Some("home")), &scope, Some("work")),
            Reach::Workspace
        );
        assert_eq!(
            reach(row("app-actions", Some("work")), &scope, Some("home")),
            Reach::Desktop
        );
        assert_eq!(
            reach(row("applications", None), &scope, Some("home")),
            Reach::Workspace,
            "an app opens where the terminal is"
        );
        assert_eq!(
            reach(row("applications", None), &scope, Some("work")),
            Reach::Desktop,
            "an app opening in another workspace reaches beyond this one"
        );
        assert_eq!(
            reach(row("actions", None), &scope, Some("home")),
            Reach::Desktop,
            "the desktop's own actions reach the whole desktop"
        );
    }

    #[test]
    fn identity_security_and_deleting_always_ask() {
        let scope = scope();
        for (risk, confirmation) in [
            (
                tilo_actions::RiskLevel::Privileged,
                tilo_actions::ConfirmationPolicy::None,
            ),
            (
                tilo_actions::RiskLevel::Destructive,
                tilo_actions::ConfirmationPolicy::None,
            ),
            (
                tilo_actions::RiskLevel::Sensitive,
                tilo_actions::ConfirmationPolicy::None,
            ),
            (
                tilo_actions::RiskLevel::Low,
                tilo_actions::ConfirmationPolicy::Confirm,
            ),
        ] {
            let contract = tilo_actions::ActionContract {
                risk,
                confirmation,
                ..tilo_actions::ActionContract::default()
            };
            assert_eq!(
                reach(
                    Row {
                        provider: "app-actions",
                        room: Some("home"),
                        contract: Some(&contract),
                    },
                    &scope,
                    Some("home"),
                ),
                Reach::Sensitive,
                "{contract:?}"
            );
        }
    }

    /// The first desktop-wide action asks; once allowed it runs; an action
    /// that always asks does so whatever was allowed, until confirmed.
    #[test]
    fn beyond_its_workspace_a_terminal_asks_first_and_sensitive_actions_always_ask() {
        let scope = scope();
        let desktop = Row {
            provider: "actions",
            room: None,
            contract: None,
        };
        let Decision::Ask(ask) =
            decide_invoke(desktop, "Show overview", &scope, Some("home"), false, false)
        else {
            panic!("the first desktop-wide action asks");
        };
        assert!(!ask.sensitive);
        assert_eq!(ask.action, "Show overview");
        assert_eq!(ask.holder, scope.holder);
        assert!(matches!(
            decide_invoke(desktop, "Show overview", &scope, Some("home"), true, false),
            Decision::Serve
        ));
        let contract = tilo_actions::ActionContract {
            risk: tilo_actions::RiskLevel::Privileged,
            ..tilo_actions::ActionContract::default()
        };
        let identity = Row {
            provider: "actions",
            room: None,
            contract: Some(&contract),
        };
        let Decision::Ask(ask) =
            decide_invoke(identity, "Sign out", &scope, Some("home"), true, false)
        else {
            panic!("a sensitive action asks even with the desktop allowed");
        };
        assert!(ask.sensitive);
        assert!(matches!(
            decide_invoke(identity, "Sign out", &scope, Some("home"), true, true),
            Decision::Serve
        ));
        assert!(refusal_for(crate::portal::PromptOutcome::Deny).is_some());
        assert!(refusal_for(crate::portal::PromptOutcome::AllowSession).is_none());
    }

    #[test]
    fn a_listing_shows_this_workspace_and_the_desktop_but_never_consent() {
        let scope = scope();
        let mut answer = serde_json::json!({"count": 4, "actions": [
            {"provider": "applications", "id": "calc", "room": null},
            {"provider": "app-actions", "id": "mine", "room": "home"},
            {"provider": "app-actions", "id": "theirs", "room": "work"},
            {"provider": "permission-prompts", "id": "allow-once@x", "room": null},
        ]});
        let mut opened = answer.clone();
        filter_listing(&mut answer, &scope, false);
        let ids: Vec<_> = answer["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|row| row["id"].as_str().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(ids, ["calc", "mine"]);
        assert_eq!(answer["count"], 2);
        filter_listing(&mut opened, &scope, true);
        assert_eq!(opened["count"], 3, "{opened}");
    }

    #[test]
    fn a_scoped_connection_only_lists_and_invokes() {
        for (line, allowed) in [
            (r#"{"op":"actions_enumerate","query":""}"#, true),
            (r#"{"op":"actions_invoke","provider":"p","id":"i"}"#, true),
            (
                r#"{"op":"actions_invoke","provider":"p","id":"i","device":"d"}"#,
                false,
            ),
            (r#"{"op":"actions_status","invocation_id":1}"#, false),
            (r#"{"op":"click_at","x":1,"y":1}"#, false),
            (r#"{"op":"snapshot","path":"x.png"}"#, false),
            (r#"{"op":"state_dump"}"#, false),
            (
                r#"{"op":"actions_enumerate","query":"","agent":"a"}"#,
                false,
            ),
        ] {
            let request = test_control::parse_request(line).expect(line);
            assert_eq!(permitted(&request), allowed, "{line}");
        }
    }
}

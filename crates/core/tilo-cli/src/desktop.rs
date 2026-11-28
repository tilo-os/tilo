//! `tilo open`, `tilo action …` and `tilo do` on this computer.
//!
//! `tilo do` and `tilo action list|show` are calls of Tilo's actions ([`crate::hosted`]): the
//! agent surface's walk names the action by its path, binds what was typed to it, and the
//! daemon decides the call as the person whose program connects, the app's scope for a program
//! an app started. `tilo open` sends the action model's frames to the first control socket that
//! serves this process ([`tilo_control::front`]):
//!
//! - **The device owner's own login or ssh** reaches the admin socket, with
//!   their authority at the keyboard, through the same capability check as
//!   the palette.
//! - **A terminal app, and everything run in it,** is turned away there by
//!   the desktop (the kernel says it is an app's process), and goes on to the
//!   session's MCP server, which serves it with the terminal workspace's
//!   scope; anything beyond that is the person's to allow.
//! - **Any process where no desktop session runs** reaches the device's
//!   daemon, which answers it as whoever the kernel says it is.
//! - **A development session** named by `$TILO_TEST_CONTROL` is reached
//!   there alone.

use std::io::Write;
use std::process::ExitCode;

use clap::Subcommand;
use clap_complete::Shell;
use tilo_control::client::{ControlSocket, desktop_present, sockets_from_env};
use tilo_control::front::{self, Io, Spelling};

/// `tilo action …` and the standalone `action`.
#[derive(Debug, Subcommand)]
pub enum ActionCmd {
    /// List the apps, what is under a path and the actions it offers, or what words find.
    #[command(visible_alias = "ls")]
    List {
        /// The app or resource, as `app/resource`, or words to look for; the apps with none.
        path: Vec<String>,
    },
    /// Do one thing an app can do.
    #[command(
        after_help = "Examples:\n  tilo do terminal#new-window\n  tilo do notes/Todo#rename title=Groceries\n  tilo do sound#set-volume 40\n\n\
                      Options are written name=value; a single value goes to the one the action needs."
    )]
    Run {
        /// The action's path, app/resource#action, as `tilo action list` lists it.
        action: String,
        /// Its options, as name=value.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show what an action does and what it takes.
    Show {
        /// The action's path, app/resource#action.
        action: String,
    },
}

/// The sockets of this process.
pub fn sockets() -> Vec<ControlSocket> {
    sockets_from_env()
}

/// Whether a desktop session serves this process at all.
pub fn available() -> bool {
    desktop_present(&sockets())
}

fn exit(code: u8) -> ExitCode {
    if code == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(code)
    }
}

fn with_io(run: impl FnOnce(&mut Io<'_>) -> u8) -> ExitCode {
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    let code = run(&mut Io {
        out: &mut out,
        err: &mut err,
    });
    let _ = out.flush();
    exit(code)
}

/// Answer one `action` subcommand as `reach` calls Tilo's actions, in `workspace` or the one
/// these calls serve.
pub fn run_action(
    reach: crate::api::Reach<'_>,
    workspace: Option<&str>,
    cmd: ActionCmd,
) -> ExitCode {
    match cmd {
        ActionCmd::List { path } => {
            let path = path.join(" ");
            crate::hosted::ls(
                reach,
                workspace,
                (!path.is_empty()).then_some(path.as_str()),
                None,
            )
        }
        ActionCmd::Run { action, args } => {
            match (workspace.is_none() && reach.key_file.is_none())
                .then(|| section_here(&action, &args))
                .flatten()
            {
                Some(answered) => with_io(|io| front::say(answered, reach.json, io)),
                None => crate::hosted::act(reach, workspace, &action, &args),
            }
        }
        ActionCmd::Show { action } => crate::hosted::ls(reach, workspace, Some(&action), None),
    }
}

/// An action of a settings section (`settings/appearance#set-mode dark`), the person's or this
/// device's: the desktop's own, which its seat serves to its control socket alone, run there as
/// a process outside the session runs it ([`settings_store::outside`]); while no session runs,
/// a setter's value is kept for the session and read at once by the device's processes. A lone
/// value is the setter's. `None` for any other action, which goes through the walk.
fn section_here(action: &str, args: &[String]) -> Option<Result<serde_json::Value, String>> {
    if !tilo_actions::remote::settings_resources::is_section(action) {
        return None;
    }
    let (resource, verb) = action.split_once('#')?;
    let args = match args {
        [word] if !word.contains('=') && !word.starts_with("--") => {
            let value = serde_json::from_str(word)
                .unwrap_or_else(|_| serde_json::Value::from(word.as_str()));
            serde_json::Map::from_iter([("value".to_owned(), value)])
        }
        words => {
            let mut input = std::io::stdin().lock();
            match crate::bind::arguments(&serde_json::Value::Null, words, &mut input) {
                Ok(args) => args,
                Err(why) => return Some(Err(why)),
            }
        }
    };
    let sockets = sockets();
    if !desktop_present(&sockets) {
        let value = args.get("value").cloned().unwrap_or_default();
        return Some(
            settings_store::outside::set_on_this_device(resource, verb, &value)
                .map(|()| serde_json::json!({"status": "ok", "path": action})),
        );
    }
    let target = tilo_actions::remote::RemoteActionTarget {
        provider: tilo_actions::remote::settings_resources::PROVIDER.to_owned(),
        address: tilo_actions::ActionAddress::new(resource, verb),
    };
    Some(front::call_target(
        &sockets,
        target,
        verb,
        &serde_json::Value::Object(args),
        &Spelling::tilo(),
    ))
}

/// `tilo open APP [file|link…]` and `tilo app open …`.
pub fn run_open(app: &str, items: &[String], spelling: &Spelling) -> ExitCode {
    let sockets = sockets();
    let cwd = std::env::current_dir().unwrap_or_default();
    with_io(|io| front::open(&sockets, app, items, &cwd, spelling, io))
}

/// What `tilo complete` completes.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Completing {
    /// Action paths, app/resource#action: the apps they start with.
    Actions,
    /// App names.
    Apps,
}

/// `tilo complete KIND [PREFIX]`: one candidate per line, for the shell
/// completions below. Prints nothing where there is no desktop.
pub fn complete(what: Completing, prefix: &str) -> ExitCode {
    let names = match what {
        // An action's path starts with its app.
        Completing::Actions | Completing::Apps => front::complete_apps(&sockets(), prefix),
    };
    let mut out = std::io::stdout().lock();
    for name in names {
        if writeln!(out, "{name}").is_err() {
            break;
        }
    }
    ExitCode::SUCCESS
}

/// Shell glue that asks the desktop for app and action names as they are
/// typed: after `tilo open`, `tilo app open`, `tilo do`, `tilo action
/// run|show` and the standalone `action run|show`.
pub fn dynamic_completions(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash => BASH,
        Shell::Zsh => ZSH,
        Shell::Fish => FISH,
        _ => "",
    }
}

const BASH: &str = r#"
# Names the desktop holds, asked for as they are typed.
_tilo_dynamic() {
    local cur="${COMP_WORDS[COMP_CWORD]}" first="${COMP_WORDS[1]}" second="${COMP_WORDS[2]}"
    local what=""
    if [[ "$1" == action ]]; then
        if (( COMP_CWORD == 2 )) && [[ "$first" == run || "$first" == show ]]; then what=actions; fi
    elif (( COMP_CWORD == 2 )) && [[ "$first" == open ]]; then what=apps
    elif (( COMP_CWORD == 2 )) && [[ "$first" == do ]]; then what=actions
    elif (( COMP_CWORD == 3 )) && [[ "$first" == action && ( "$second" == run || "$second" == show ) ]]; then what=actions
    elif (( COMP_CWORD == 3 )) && [[ ( "$first" == app || "$first" == store ) && "$second" == open ]]; then what=apps
    fi
    [[ -n "$what" ]] || return 1
    mapfile -t COMPREPLY < <(tilo complete "$what" "$cur" 2>/dev/null)
    return 0
}
_tilo_action_dynamic() {
    _tilo_dynamic action || _action "$@"
}
_tilo_complete_all() {
    _tilo_dynamic tilo && return
    if declare -F _tilo_with_app_completion >/dev/null; then
        _tilo_with_app_completion "$@"
    else
        _tilo "$@"
    fi
}
complete -F _tilo_complete_all -o bashdefault -o default tilo
complete -F _tilo_action_dynamic -o bashdefault -o default action
"#;

const ZSH: &str = r#"
# Names the desktop holds, asked for as they are typed.
_tilo_dynamic() {
    local what=""
    if [[ "${words[1]}" == action ]]; then
        (( CURRENT == 3 )) && [[ "${words[2]}" == run || "${words[2]}" == show ]] && what=actions
    elif (( CURRENT == 3 )) && [[ "${words[2]}" == open ]]; then what=apps
    elif (( CURRENT == 3 )) && [[ "${words[2]}" == do ]]; then what=actions
    elif (( CURRENT == 4 )) && [[ "${words[2]}" == action && ( "${words[3]}" == run || "${words[3]}" == show ) ]]; then what=actions
    elif (( CURRENT == 4 )) && [[ ( "${words[2]}" == app || "${words[2]}" == store ) && "${words[3]}" == open ]]; then what=apps
    fi
    [[ -n "$what" ]] || return 1
    local -a names
    names=("${(@f)$(tilo complete "$what" "${words[CURRENT]}" 2>/dev/null)}")
    compadd -U -a names
    return 0
}
_tilo_action_dynamic() {
    _tilo_dynamic || _action "$@"
}
_tilo_complete_all() {
    _tilo_dynamic && return
    if (( $+functions[_tilo_with_app_completion] )); then
        _tilo_with_app_completion "$@"
    else
        _tilo "$@"
    fi
}
compdef _tilo_complete_all tilo
compdef _tilo_action_dynamic action
"#;

const FISH: &str = r"
# Names the desktop holds, asked for as they are typed.
complete -c tilo -n '__fish_seen_subcommand_from open; and test (count (commandline -opc)) -eq 2' -f -a '(tilo complete apps (commandline -ct))'
complete -c tilo -n '__fish_seen_subcommand_from do; and test (count (commandline -opc)) -eq 2' -f -a '(tilo complete actions (commandline -ct))'
complete -c tilo -n '__fish_seen_subcommand_from action; and __fish_seen_subcommand_from run show; and test (count (commandline -opc)) -eq 3' -f -a '(tilo complete actions (commandline -ct))'
complete -c tilo -n '__fish_seen_subcommand_from app store; and __fish_seen_subcommand_from open; and test (count (commandline -opc)) -eq 3' -f -a '(tilo complete apps (commandline -ct))'
complete -c action -n '__fish_seen_subcommand_from run show; and test (count (commandline -opc)) -eq 2' -f -a '(tilo complete actions (commandline -ct))'
";

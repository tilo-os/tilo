//! `tilo open`, `tilo action …` and `tilo do`: the desktop's apps and
//! actions, through the same control protocol `tilo-control` speaks
//! ([`tilo_control::front`]).
//!
//! Which socket a request reaches is decided per process, never widened:
//!
//! - **The device owner's own login or ssh** reaches the admin socket, with
//!   their authority at the keyboard, through the same capability gate as
//!   the palette.
//! - **A terminal app, and everything run in it,** is turned away there by
//!   the desktop (the kernel says it is an app's process), and goes on to the
//!   session's agent door, which serves it with the terminal workspace's
//!   scope; anything beyond that is the person's to allow.
//! - **A development session** named by `$TILO_TEST_CONTROL` is reached
//!   there alone.

use std::io::Write;
use std::process::ExitCode;

use clap::Subcommand;
use clap_complete::Shell;
use tilo_control::client::{Door, desktop_present, doors_from_env};
use tilo_control::front::{self, Io, Spelling};

/// `tilo action …` and the standalone `action`.
#[derive(Debug, Subcommand)]
pub enum ActionCmd {
    /// List what this desktop can do, grouped, optionally only what matches.
    #[command(visible_alias = "ls")]
    List {
        /// Words to look for.
        filter: Vec<String>,
    },
    /// Do one thing this desktop can do.
    #[command(
        after_help = "Examples:\n  tilo do new-terminal-window\n  tilo do rename-note title=Groceries\n  tilo do set-volume 40\n\n\
                      Options are written name=value; a single value goes to the one the action needs."
    )]
    Run {
        /// The action's name, as `tilo actions` lists it, or words of its title.
        action: String,
        /// Its options, as name=value.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show what an action does and what it takes.
    Show {
        /// The action's name, as `tilo actions` lists it, or words of its title.
        action: String,
    },
}

/// The doors of this process.
pub fn doors() -> Vec<Door> {
    doors_from_env()
}

/// Whether a desktop session serves this process at all.
pub fn available() -> bool {
    desktop_present(&doors())
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

/// Run one `action` subcommand, with examples spelled for `spelling`.
pub fn run_action(cmd: ActionCmd, spelling: &Spelling) -> ExitCode {
    let doors = doors();
    with_io(|io| match cmd {
        ActionCmd::List { filter } => front::list(&doors, &filter.join(" "), spelling, io),
        ActionCmd::Run { action, args } => front::run(&doors, &action, &args, spelling, io),
        ActionCmd::Show { action } => front::show(&doors, &action, spelling, io),
    })
}

/// `tilo open APP [file|link…]` and `tilo app open …`.
pub fn run_open(app: &str, items: &[String], spelling: &Spelling) -> ExitCode {
    let doors = doors();
    let cwd = std::env::current_dir().unwrap_or_default();
    with_io(|io| front::open(&doors, app, items, &cwd, spelling, io))
}

/// What `tilo complete` completes.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Completing {
    /// Action names, matched on their titles.
    Actions,
    /// App names.
    Apps,
}

/// `tilo complete KIND [PREFIX]`: one candidate per line, for the shell
/// completions below. Prints nothing where there is no desktop.
pub fn complete(what: Completing, prefix: &str) -> ExitCode {
    let doors = doors();
    let names = match what {
        Completing::Actions => front::complete_actions(&doors, prefix),
        Completing::Apps => front::complete_apps(&doors, prefix),
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
/// run|show` and the standalone `action run|show`. Candidates are not
/// filtered by prefix here: the desktop matches words inside titles.
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

//! The shell's state, and how a caller runs text in it.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use brush_parser::ast;

use crate::host::Host;
use crate::io::{Input, Output, Stdio};
use crate::vars::Vars;

/// How a command ended other than by returning a status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flow {
    /// `break n`, with the loops still to leave.
    Break(u32),
    /// `continue n`.
    Continue(u32),
    /// `return`, from a function or a sourced file.
    Return(i32),
    /// `exit`, or a failure under `set -e`.
    Exit(i32),
    /// An error that ends what is running: a syntax error, an unsupported
    /// feature, `${x:?}`. A script stops; an interactive shell drops the
    /// line and prompts again.
    Fatal(i32),
    /// An error that fails the command it happened in, and only that: an
    /// arithmetic error. What follows runs, as in bash.
    Error(i32),
}

/// What running a command answers: its status, or a [`Flow`].
pub type Exec = Result<i32, Flow>;

/// The options `set` changes.
#[derive(Debug, Clone, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is one of `set`'s independent switches"
)]
pub struct Options {
    /// `-e`
    pub errexit: bool,
    /// `-u`
    pub nounset: bool,
    /// `-x`
    pub xtrace: bool,
    /// `-f`
    pub noglob: bool,
    /// `-C`
    pub noclobber: bool,
    /// `-o pipefail`
    pub pipefail: bool,
    /// `shopt -s nullglob`
    pub nullglob: bool,
    /// `shopt -s dotglob`
    pub dotglob: bool,
    /// `shopt -s failglob`
    pub failglob: bool,
    /// `shopt -s nocaseglob`, `nocasematch` are refused by name.
    /// `-a`
    pub allexport: bool,
}

/// One open descriptor of the shell's.
#[derive(Debug, Clone)]
pub enum Fd {
    In(Input),
    Out(Output),
}

/// The shell.
#[derive(Clone)]
pub struct Shell {
    pub(crate) host: Arc<dyn Host>,
    pub(crate) vars: Vars,
    pub(crate) functions: HashMap<String, Arc<ast::FunctionBody>>,
    pub(crate) aliases: BTreeMap<String, String>,
    pub(crate) positional: Vec<String>,
    pub(crate) arg0: String,
    pub(crate) cwd: PathBuf,
    pub(crate) last_status: i32,
    pub(crate) options: Options,
    pub(crate) fds: BTreeMap<u32, Fd>,
    pub(crate) traps: BTreeMap<String, String>,
    pub(crate) interactive: bool,
    pub(crate) random: u32,
    /// How deep in loops the running command is, for `break` and
    /// `continue`.
    pub(crate) loop_depth: u32,
    /// How deep in contexts that `set -e` ignores (an `if` condition, the
    /// left of `&&`): non-zero there.
    pub(crate) errexit_ignored: u32,
    /// The status of the last command substitution, which an assignment-only
    /// command answers.
    pub(crate) last_substitution: Option<i32>,
    /// Calls deep in functions and `source`, to stop runaway recursion.
    pub(crate) call_depth: u32,
    /// Who the output of Tilo's own commands is for.
    pub(crate) render: crate::commands::Render,
    /// The run the command running now is part of.
    pub(crate) run: Option<Arc<crate::host::Run>>,
    /// The line editor's state: the kill ring, the lines this shell ran,
    /// the person's history as answered so far.
    pub(crate) editor: Arc<std::sync::Mutex<crate::editor::State>>,
    /// The shell's jobs, shared with its copies.
    pub(crate) jobs: Arc<std::sync::Mutex<crate::jobs::Table>>,
    /// The number `$!` gives: the last background job's.
    pub(crate) last_background: Option<u32>,
    /// Terminal's run marks, when Terminal asked for them.
    pub(crate) marks: Option<Arc<crate::jobs::Marks>>,
}

/// How deep functions may call each other before the shell refuses.
pub(crate) const MAX_CALL_DEPTH: u32 = 1000;

impl Shell {
    /// A shell on `host`, with `env` as its exported variables and `cwd` its
    /// working directory, writing to and reading from `io`.
    pub fn new(
        host: Arc<dyn Host>,
        env: impl IntoIterator<Item = (String, String)>,
        cwd: PathBuf,
        io: Stdio,
    ) -> Self {
        let mut vars = Vars::default();
        for (name, value) in env {
            if crate::vars::valid_name(&name) && vars.set(&name, value).is_ok() {
                vars.declare(&name).exported = true;
            }
        }
        let seed =
            u32::try_from(host.now().rem_euclid(1 << 32)).unwrap_or(0) ^ host.pid().rotate_left(16);
        let pwd = cwd.to_string_lossy().into_owned();
        let mut shell = Self {
            host,
            vars,
            functions: HashMap::new(),
            aliases: BTreeMap::new(),
            positional: Vec::new(),
            arg0: String::from("ush"),
            cwd,
            last_status: 0,
            options: Options::default(),
            fds: BTreeMap::new(),
            traps: BTreeMap::new(),
            interactive: false,
            random: seed | 1,
            loop_depth: 0,
            errexit_ignored: 0,
            last_substitution: None,
            call_depth: 0,
            render: crate::commands::Render::Text,
            run: None,
            editor: Arc::default(),
            jobs: Arc::default(),
            last_background: None,
            marks: None,
        };
        shell.set_stdio(io);
        let _ = shell.vars.set("PWD", pwd);
        shell.vars.declare("PWD").exported = true;
        if !shell.vars.is_set("IFS") {
            let _ = shell.vars.set("IFS", " \t\n");
        }
        for (name, value) in [("PS1", "\\w \\$ "), ("PS2", "> "), ("PS4", "+ ")] {
            if !shell.vars.is_set(name) {
                let _ = shell.vars.set(name, value);
            }
        }
        let _ = shell.vars.set("OPTIND", "1");
        shell
    }

    /// Replace the standard streams.
    pub fn set_stdio(&mut self, io: Stdio) {
        self.fds.insert(0, Fd::In(io.stdin));
        self.fds.insert(1, Fd::Out(io.stdout));
        self.fds.insert(2, Fd::Out(io.stderr));
    }

    /// `$0` and the positional parameters.
    pub fn set_args(&mut self, arg0: impl Into<String>, positional: Vec<String>) {
        self.arg0 = arg0.into();
        self.positional = positional;
    }

    /// Render Tilo's own commands' output for `render`: an agent's run gets
    /// the agent rendering.
    pub fn set_render(&mut self, render: crate::commands::Render) {
        self.render = render;
    }

    /// The run what the shell runs next belongs to; `None` outside a run.
    pub fn set_run(&mut self, run: Option<crate::host::Run>) {
        self.run = run.map(Arc::new);
    }

    /// Mark the shell interactive: errors drop the line, not the shell.
    pub fn set_interactive(&mut self, interactive: bool) {
        self.interactive = interactive;
    }

    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    pub fn last_status(&self) -> i32 {
        self.last_status
    }

    /// The scalar value of the variable `name`.
    pub fn var(&self, name: &str) -> Option<String> {
        self.vars.scalar(name)
    }

    /// Set the variable `name`, as an assignment would.
    pub fn set_var(&mut self, name: &str, value: &str) {
        let _ = self.vars.set(name, value);
    }

    /// Take the variable `name` out of the shell, answering what it held:
    /// how a secret handed in the environment (the run marks' key) is kept
    /// from every program the shell runs.
    pub fn take_var(&mut self, name: &str) -> Option<String> {
        let value = self.vars.scalar(name);
        let _ = self.vars.unset(name);
        value
    }

    pub fn host(&self) -> &Arc<dyn Host> {
        &self.host
    }

    /// The standard streams as they are now.
    pub fn stdio(&self) -> Stdio {
        Stdio {
            stdin: self.input(0).unwrap_or_else(Input::null),
            stdout: self.output(1).unwrap_or_else(Output::null),
            stderr: self.output(2).unwrap_or_else(Output::null),
        }
    }

    pub(crate) fn input(&self, fd: u32) -> Option<Input> {
        match self.fds.get(&fd)? {
            Fd::In(input) => Some(input.clone()),
            Fd::Out(_) => None,
        }
    }

    pub(crate) fn output(&self, fd: u32) -> Option<Output> {
        match self.fds.get(&fd)? {
            Fd::Out(output) => Some(output.clone()),
            Fd::In(_) => None,
        }
    }

    /// Write to standard output.
    pub(crate) fn out(&self, text: &str) -> bool {
        self.out_bytes(text.as_bytes())
    }

    pub(crate) fn out_bytes(&self, bytes: &[u8]) -> bool {
        self.output(1).is_some_and(|o| o.write_all(bytes).is_ok())
    }

    /// Say something on standard error, as `ush: message`.
    pub(crate) fn error(&self, message: impl std::fmt::Display) {
        if let Some(err) = self.output(2) {
            let _ = err.write_str(&format!("ush: {message}\n"));
        }
    }

    /// Refuse a feature this shell does not have, by name. It ends what is
    /// running, so a script that relied on it stops instead of going on
    /// without it.
    pub(crate) fn unsupported(&self, what: &str) -> Flow {
        self.error(format!("{what} is not supported"));
        Flow::Fatal(2)
    }

    pub(crate) fn next_random(&mut self) -> u32 {
        // xorshift32: $RANDOM only needs to look random.
        let mut x = self.random;
        x ^= x.wrapping_shl(13);
        x ^= x.wrapping_shr(17);
        x ^= x.wrapping_shl(5);
        self.random = x;
        x % 32768
    }

    /// The parser's options: bash's grammar, without extended globs, which
    /// the matcher refuses rather than misreads.
    pub(crate) fn parser_options() -> brush_parser::ParserOptions {
        brush_parser::ParserOptions {
            enable_extended_globbing: false,
            ..brush_parser::ParserOptions::default()
        }
    }

    /// Parse `text` as a whole program.
    pub(crate) fn parse(text: &str) -> Result<ast::Program, brush_parser::ParseError> {
        let mut parser = brush_parser::Parser::new(
            std::io::Cursor::new(text.as_bytes()),
            &Self::parser_options(),
        );
        parser.parse_program()
    }

    /// Run `text` as a script: every command in it, in this shell. Answers
    /// the status of the last one, or the status `exit` gave.
    pub fn run_script(&mut self, text: &str) -> i32 {
        let status = match self.eval_text(text) {
            Ok(status)
            | Err(
                Flow::Exit(status)
                | Flow::Fatal(status)
                | Flow::Return(status)
                | Flow::Error(status),
            ) => status,
            Err(Flow::Break(_) | Flow::Continue(_)) => self.last_status,
        };
        self.last_status = status;
        status
    }

    /// Run `text` and then the `EXIT` trap: what a shell does once, as it
    /// ends.
    pub fn run_to_exit(&mut self, text: &str) -> i32 {
        let status = self.run_script(text);
        self.exit_trap(status)
    }

    /// Run the `EXIT` trap, if one is set, and answer the shell's final
    /// status.
    pub fn exit_trap(&mut self, status: i32) -> i32 {
        let Some(action) = self.traps.remove("EXIT") else {
            return status;
        };
        self.last_status = status;
        match self.eval_text(&action) {
            Err(Flow::Exit(code)) => code,
            _ => status,
        }
    }

    /// Parse and run `text` in this shell.
    pub(crate) fn eval_text(&mut self, text: &str) -> Exec {
        let program = match Self::parse(text) {
            Ok(program) => program,
            Err(error) => {
                self.error(format!("syntax error: {error}"));
                return Err(Flow::Fatal(2));
            }
        };
        self.run_program(&program)
    }
}

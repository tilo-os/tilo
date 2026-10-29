//! Running the parsed program: lists, pipelines, compound commands and
//! simple commands, with their redirections.

use std::sync::Arc;

use brush_parser::ast;

use crate::host::Program;
use crate::io::{Input, Output};
use crate::shell::{Exec, Fd, Flow, MAX_CALL_DEPTH, Shell};

/// What a simple command's name resolved to.
enum Resolved {
    Special(crate::builtins::Builtin),
    Function(Arc<ast::FunctionBody>),
    Builtin(crate::builtins::Builtin),
    Tier1(crate::commands::Command),
    FileAction,
    Other,
}

/// The descriptor a redirection names, or `default` when it names none.
fn fd_of(fd: Option<i32>, default: u32) -> u32 {
    fd.and_then(|n| u32::try_from(n).ok()).unwrap_or(default)
}

/// A simple command taken apart: the assignments before its name, its
/// words, its redirections, and the assignments among its arguments (with
/// where in `words` each stands).
struct SimpleParts<'a> {
    assignments: Vec<&'a ast::Assignment>,
    words: Vec<ast::Word>,
    redirects: Vec<ast::IoRedirect>,
    declaration_args: Vec<(usize, ast::Assignment, ast::Word)>,
}

impl Shell {
    pub(crate) fn run_program(&mut self, program: &ast::Program) -> Exec {
        let mut status = 0;
        for command in &program.complete_commands {
            status = self.run_list(command)?;
        }
        Ok(status)
    }

    pub(crate) fn run_list(&mut self, list: &ast::CompoundList) -> Exec {
        let mut status = self.last_status;
        for ast::CompoundListItem(and_or, separator) in &list.0 {
            if matches!(separator, ast::SeparatorOperator::Async) {
                status = self.background(and_or);
                continue;
            }
            status = self.run_and_or(and_or)?;
        }
        Ok(status)
    }

    /// Run `list` on a job's copy of the shell.
    pub(crate) fn run_and_or_owned(&mut self, list: &ast::AndOrList) -> Exec {
        self.run_and_or(list)
    }

    fn run_and_or(&mut self, list: &ast::AndOrList) -> Exec {
        let has_more = !list.additional.is_empty();
        if has_more {
            self.errexit_ignored = self.errexit_ignored.saturating_add(1);
        }
        let first = self.run_pipeline(&list.first);
        if has_more {
            self.errexit_ignored = self.errexit_ignored.saturating_sub(1);
        }
        let mut status = first?;
        let count = list.additional.len();
        for (i, next) in list.additional.iter().enumerate() {
            let (run, pipeline) = match next {
                ast::AndOr::And(p) => (status == 0, p),
                ast::AndOr::Or(p) => (status != 0, p),
            };
            if !run {
                continue;
            }
            let is_last = i.saturating_add(1) == count;
            if !is_last {
                self.errexit_ignored = self.errexit_ignored.saturating_add(1);
            }
            let result = self.run_pipeline(pipeline);
            if !is_last {
                self.errexit_ignored = self.errexit_ignored.saturating_sub(1);
            }
            status = result?;
        }
        Ok(status)
    }

    fn run_pipeline(&mut self, pipeline: &ast::Pipeline) -> Exec {
        if pipeline.timed.is_some() {
            return Err(self.unsupported("`time`"));
        }
        if self.host.interrupted() {
            return Err(Flow::Fatal(130));
        }
        if pipeline.bang {
            self.errexit_ignored = self.errexit_ignored.saturating_add(1);
        }
        let result = match pipeline.seq.as_slice() {
            [single] => self.run_command(single),
            stages => self.run_stages(stages),
        };
        // The command failed; the script goes on.
        let result = match result {
            Err(Flow::Error(status)) => Ok(status),
            other => other,
        };
        if pipeline.bang {
            self.errexit_ignored = self.errexit_ignored.saturating_sub(1);
        }
        let mut status = result?;
        if pipeline.bang {
            status = i32::from(status == 0);
        }
        self.last_status = status;
        if status != 0 && !pipeline.bang {
            self.check_errexit(status)?;
        }
        Ok(status)
    }

    /// Under `set -e`, a failure outside a condition ends the shell.
    pub(crate) fn check_errexit(&self, status: i32) -> Exec {
        if self.options.errexit && self.errexit_ignored == 0 && status != 0 {
            return Err(Flow::Exit(status));
        }
        Ok(status)
    }

    /// A pipeline of several commands: each runs in a subshell of its own
    /// (a copy of this shell), all at once, joined by pipes in memory.
    fn run_stages(&self, stages: &[ast::Command]) -> Exec {
        let statuses = self.stage_statuses(stages);
        let status = if self.options.pipefail {
            statuses
                .iter()
                .rev()
                .copied()
                .find(|s| *s != 0)
                .unwrap_or(0)
        } else {
            statuses.last().copied().unwrap_or(0)
        };
        if self.host.interrupted() {
            return Err(Flow::Fatal(130));
        }
        Ok(status)
    }

    #[cfg(not(target_family = "wasm"))]
    fn stage_statuses(&self, stages: &[ast::Command]) -> Vec<i32> {
        let count = stages.len();
        let mut shells = Vec::with_capacity(count);
        let mut next_input = self.input(0).unwrap_or_else(Input::null);
        for (i, _) in stages.iter().enumerate() {
            let mut sub = self.clone();
            sub.fds.insert(0, Fd::In(next_input.clone()));
            if i.saturating_add(1) < count {
                let (reader, writer) = crate::io::pipe();
                sub.fds.insert(1, Fd::Out(Output::new(writer)));
                next_input = Input::new(reader);
            }
            shells.push(sub);
        }
        drop(next_input);
        std::thread::scope(|scope| {
            #[expect(
                clippy::needless_collect,
                reason = "every stage must start before any is joined: they feed each other"
            )]
            let handles: Vec<_> = shells
                .into_iter()
                .zip(stages)
                .map(|(mut sub, command)| {
                    scope.spawn(move || {
                        let status = sub.run_subshell_command(command);
                        drop(sub);
                        status
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap_or(1)).collect()
        })
    }

    /// Without threads (WebAssembly in a page), each stage runs to its end
    /// and the next reads all of what it wrote.
    #[cfg(target_family = "wasm")]
    fn stage_statuses(&self, stages: &[ast::Command]) -> Vec<i32> {
        let count = stages.len();
        let mut input = self.input(0).unwrap_or_else(Input::null);
        let mut statuses = Vec::with_capacity(count);
        for (i, command) in stages.iter().enumerate() {
            let mut sub = self.clone();
            sub.fds.insert(0, Fd::In(input.clone()));
            let captured = if i.saturating_add(1) < count {
                let (out, captured) = Output::buffer();
                sub.fds.insert(1, Fd::Out(out));
                Some(captured)
            } else {
                None
            };
            statuses.push(sub.run_subshell_command(command));
            drop(sub);
            if let Some(captured) = captured {
                input = Input::bytes(captured.take());
            }
        }
        statuses
    }

    fn run_subshell_command(&mut self, command: &ast::Command) -> i32 {
        let status = match self.run_command(command) {
            Ok(status)
            | Err(
                Flow::Exit(status)
                | Flow::Fatal(status)
                | Flow::Return(status)
                | Flow::Error(status),
            ) => status,
            Err(Flow::Break(_) | Flow::Continue(_)) => self.last_status,
        };
        self.exit_trap(status)
    }

    pub(crate) fn run_command(&mut self, command: &ast::Command) -> Exec {
        match command {
            ast::Command::Simple(simple) => self.run_simple(simple),
            ast::Command::Compound(compound, redirects) => {
                let saved =
                    self.redirect(redirects.as_ref().map_or(&[][..], |r| r.0.as_slice()))?;
                let result = self.run_compound(compound);
                self.restore(saved);
                result
            }
            ast::Command::Function(def) => {
                let name = def.fname.value.clone();
                self.functions.insert(name, Arc::new(def.body.clone()));
                Ok(0)
            }
            ast::Command::ExtendedTest(test, redirects) => {
                let saved =
                    self.redirect(redirects.as_ref().map_or(&[][..], |r| r.0.as_slice()))?;
                let result = self.extended_test(&test.expr);
                self.restore(saved);
                result.map(|ok| i32::from(!ok))
            }
        }
    }

    fn run_compound(&mut self, compound: &ast::CompoundCommand) -> Exec {
        match compound {
            ast::CompoundCommand::BraceGroup(group) => self.run_list(&group.list),
            ast::CompoundCommand::Subshell(sub) => {
                let mut copy = self.clone();
                // A loop outside the subshell is not one `break` can leave.
                copy.loop_depth = 0;
                let status = match copy.run_list(&sub.list) {
                    Ok(status)
                    | Err(Flow::Exit(status) | Flow::Return(status) | Flow::Error(status)) => {
                        status
                    }
                    Err(Flow::Fatal(status)) => {
                        if self.interactive {
                            status
                        } else {
                            copy.last_status = status;
                            status
                        }
                    }
                    Err(Flow::Break(_) | Flow::Continue(_)) => copy.last_status,
                };
                let status = copy.exit_trap(status);
                drop(copy);
                Ok(status)
            }
            ast::CompoundCommand::IfClause(clause) => self.run_if(clause),
            ast::CompoundCommand::WhileClause(clause) => self.run_while(clause, false),
            ast::CompoundCommand::UntilClause(clause) => self.run_while(clause, true),
            ast::CompoundCommand::ForClause(clause) => self.run_for(clause),
            ast::CompoundCommand::ArithmeticForClause(clause) => self.run_arith_for(clause),
            ast::CompoundCommand::CaseClause(clause) => self.run_case(clause),
            ast::CompoundCommand::Arithmetic(arith) => match self.arith_text(&arith.expr.value) {
                Ok(value) => Ok(i32::from(value == 0)),
                // An error in `(( ))` fails the command; the script goes on.
                Err(Flow::Error(_)) => Ok(1),
                Err(other) => Err(other),
            },
            ast::CompoundCommand::Coprocess(_) => Err(self.unsupported("`coproc`")),
        }
    }

    /// Run a condition: `set -e` does not apply inside it.
    fn condition(&mut self, list: &ast::CompoundList) -> Exec {
        self.errexit_ignored = self.errexit_ignored.saturating_add(1);
        let result = self.run_list(list);
        self.errexit_ignored = self.errexit_ignored.saturating_sub(1);
        result
    }

    fn run_if(&mut self, clause: &ast::IfClauseCommand) -> Exec {
        if self.condition(&clause.condition)? == 0 {
            return self.run_list(&clause.then);
        }
        for branch in clause.elses.iter().flatten() {
            match &branch.condition {
                Some(cond) => {
                    if self.condition(cond)? == 0 {
                        return self.run_list(&branch.body);
                    }
                }
                None => return self.run_list(&branch.body),
            }
        }
        Ok(0)
    }

    /// One pass of a loop body. `Ok(None)` means go on; `Ok(Some(status))`
    /// means the loop ends with that status.
    fn loop_body(&mut self, body: &ast::CompoundList) -> Result<Option<i32>, Flow> {
        if self.host.interrupted() {
            return Err(Flow::Fatal(130));
        }
        match self.run_list(body) {
            Ok(status) => {
                self.last_status = status;
                Ok(None)
            }
            Err(Flow::Break(n)) => {
                if n > 1 {
                    return Err(Flow::Break(n.saturating_sub(1)));
                }
                Ok(Some(self.last_status))
            }
            Err(Flow::Continue(n)) => {
                if n > 1 {
                    return Err(Flow::Continue(n.saturating_sub(1)));
                }
                Ok(None)
            }
            Err(other) => Err(other),
        }
    }

    fn in_loop<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        self.loop_depth = self.loop_depth.saturating_add(1);
        let result = f(self);
        self.loop_depth = self.loop_depth.saturating_sub(1);
        result
    }

    fn run_while(&mut self, clause: &ast::WhileOrUntilClauseCommand, until: bool) -> Exec {
        let ast::WhileOrUntilClauseCommand(cond, body, _) = clause;
        self.in_loop(|shell| {
            let mut status = 0;
            loop {
                // `while break` leaves the loop from its condition.
                let c = match shell.condition(cond) {
                    Ok(c) => c,
                    Err(Flow::Break(n)) if n > 1 => return Err(Flow::Break(n.saturating_sub(1))),
                    Err(Flow::Break(_)) => break,
                    Err(Flow::Continue(n)) if n > 1 => {
                        return Err(Flow::Continue(n.saturating_sub(1)));
                    }
                    Err(Flow::Continue(_)) => continue,
                    Err(other) => return Err(other),
                };
                if (c == 0) == until {
                    break;
                }
                if let Some(end) = shell.loop_body(&body.list)? {
                    return Ok(end);
                }
                status = shell.last_status;
            }
            Ok(status)
        })
    }

    fn run_for(&mut self, clause: &ast::ForClauseCommand) -> Exec {
        let items = match &clause.values {
            Some(words) => self.expand_words(words)?,
            None => self.positional.clone(),
        };
        let name = clause.variable_name.clone();
        if !crate::vars::valid_name(&name) {
            self.error(format!("`{name}': not a valid identifier"));
            return Err(Flow::Fatal(1));
        }
        self.in_loop(|shell| {
            let mut status = 0;
            for item in items {
                shell.vars.set(&name, item).map_err(|e| {
                    shell.error(e);
                    Flow::Fatal(1)
                })?;
                if let Some(end) = shell.loop_body(&clause.body.list)? {
                    return Ok(end);
                }
                status = shell.last_status;
            }
            Ok(status)
        })
    }

    fn run_arith_for(&mut self, clause: &ast::ArithmeticForClauseCommand) -> Exec {
        if let Some(init) = &clause.initializer {
            self.arith_text(&init.value)?;
        }
        self.in_loop(|shell| {
            let mut status = 0;
            loop {
                if let Some(cond) = clause
                    .condition
                    .as_ref()
                    .filter(|c| !c.value.trim().is_empty())
                    && shell.arith_text(&cond.value)? == 0
                {
                    break;
                }
                if let Some(end) = shell.loop_body(&clause.body.list)? {
                    return Ok(end);
                }
                status = shell.last_status;
                if let Some(update) = &clause.updater {
                    shell.arith_text(&update.value)?;
                }
            }
            Ok(status)
        })
    }

    fn run_case(&mut self, clause: &ast::CaseClauseCommand) -> Exec {
        let value = self.expand_word(&clause.value.value)?;
        let mut status = 0;
        let mut fall_through = false;
        for (i, item) in clause.cases.iter().enumerate() {
            let mut matched = fall_through;
            if !matched {
                for pattern in &item.patterns {
                    if self.expand_pattern(&pattern.value)?.matches(&value) {
                        matched = true;
                        break;
                    }
                }
            }
            if !matched {
                continue;
            }
            status = match &item.cmd {
                Some(list) => self.run_list(list)?,
                None => 0,
            };
            match item.post_action {
                ast::CaseItemPostAction::ExitCase => return Ok(status),
                ast::CaseItemPostAction::UnconditionallyExecuteNextCaseItem => fall_through = true,
                ast::CaseItemPostAction::ContinueEvaluatingCases => {
                    fall_through = false;
                    let _ = i;
                }
            }
        }
        Ok(status)
    }

    fn run_simple(&mut self, simple: &ast::SimpleCommand) -> Exec {
        self.last_substitution = None;
        if let Some(rewritten) = self.alias_for(simple) {
            if self.call_depth >= MAX_CALL_DEPTH {
                self.error("alias expansion does not end");
                return Err(Flow::Fatal(1));
            }
            self.call_depth = self.call_depth.saturating_add(1);
            let result = self.eval_text(&rewritten);
            self.call_depth = self.call_depth.saturating_sub(1);
            return result;
        }
        let SimpleParts {
            assignments,
            words,
            redirects,
            declaration_args,
        } = self.simple_parts(simple)?;

        let declaration = simple
            .word_or_name
            .as_ref()
            .filter(|w| {
                crate::builtins::is_declaration(&w.value) && !self.functions.contains_key(&w.value)
            })
            .map(|w| w.value.clone());
        if let Some(name) = declaration {
            // `local x=$y`, `declare -a a=(1 2)`: the assignments are the
            // builtin's to carry out, after it has made them local.
            let mut args = Vec::new();
            for (i, w) in words.iter().enumerate().skip(1) {
                if let Some((_, a, _)) = declaration_args.iter().find(|(at, _, _)| *at == i) {
                    args.push(crate::builtins::DeclArg::Assign(a.clone()));
                } else {
                    for field in self.expand_words(std::slice::from_ref(w))? {
                        args.push(crate::builtins::DeclArg::Word(field));
                    }
                }
            }
            for a in &assignments {
                self.assign(a)?;
            }
            let saved = self.redirect(&redirects)?;
            let result = crate::builtins::declaration(self, &name, args);
            self.restore(saved);
            return result;
        }
        let argv = self.expand_words(&words)?;

        let Some(name) = argv.first().cloned() else {
            // Only assignments and redirections.
            for a in &assignments {
                self.assign(a)?;
            }
            let saved = self.redirect(&redirects)?;
            self.restore(saved);
            return Ok(self.last_substitution.unwrap_or(0));
        };

        self.trace(&argv);

        let resolved = self.resolve(&name);
        let operands = argv.get(1..).unwrap_or(&[]);
        if let Resolved::Special(builtin) = resolved {
            // A special built-in's assignments stay, as POSIX says.
            for a in &assignments {
                self.assign(a)?;
            }
            let saved = self.redirect(&redirects)?;
            let result = builtin(self, operands);
            if name == "exec" && operands.is_empty() {
                drop(saved);
            } else {
                self.restore(saved);
            }
            return result;
        }

        // Every other command's assignments are for it alone.
        let saved_vars = self.temporary_assignments(&assignments)?;
        let saved = match self.redirect(&redirects) {
            Ok(saved) => saved,
            Err(flow) => {
                self.restore_assignments(saved_vars);
                return Err(flow);
            }
        };
        let result = match resolved {
            Resolved::Special(_) => Ok(0),
            Resolved::Function(body) => self.call_function(&name, &body, operands),
            Resolved::Builtin(builtin) => builtin(self, operands),
            Resolved::Tier1(command) => Ok(self.run_tier1(&name, command, operands)),
            Resolved::FileAction => {
                let io = self.stdio();
                Ok(self.host.file_action(&crate::host::FileCall {
                    name: &name,
                    args: operands,
                    cwd: &self.cwd,
                    io: &io,
                    run: self.run.as_deref(),
                }))
            }
            Resolved::Other => Ok(self.run_other(&argv)),
        };
        self.restore(saved);
        self.restore_assignments(saved_vars);
        result
    }

    /// Under `set -x`, the command about to run, after `$PS4`.
    fn trace(&self, argv: &[String]) {
        if !self.options.xtrace {
            return;
        }
        let ps4 = self.vars.scalar("PS4").unwrap_or_default();
        let line: Vec<String> = argv.iter().map(|a| crate::expand::quote(a)).collect();
        if let Some(err) = self.output(2) {
            let _ = err.write_str(&format!("{ps4}{}\n", line.join(" ")));
        }
    }

    /// A simple command's assignments, words and redirections, in order.
    fn simple_parts<'a>(&self, simple: &'a ast::SimpleCommand) -> Result<SimpleParts<'a>, Flow> {
        let mut assignments = Vec::new();
        let mut words = Vec::new();
        let mut redirects = Vec::new();
        for item in simple.prefix.iter().flat_map(|p| p.0.iter()) {
            match item {
                ast::CommandPrefixOrSuffixItem::AssignmentWord(a, _) => assignments.push(a),
                ast::CommandPrefixOrSuffixItem::IoRedirect(r) => redirects.push(r.clone()),
                ast::CommandPrefixOrSuffixItem::Word(w) => words.push(w.clone()),
                ast::CommandPrefixOrSuffixItem::ProcessSubstitution(..) => {
                    return Err(self.unsupported("process substitution (`<(…)`, `>(…)`)"));
                }
            }
        }
        if let Some(name) = &simple.word_or_name {
            words.push(name.clone());
        }
        let mut declaration_args = Vec::new();
        for item in simple.suffix.iter().flat_map(|s| s.0.iter()) {
            match item {
                ast::CommandPrefixOrSuffixItem::Word(w) => words.push(w.clone()),
                ast::CommandPrefixOrSuffixItem::IoRedirect(r) => redirects.push(r.clone()),
                ast::CommandPrefixOrSuffixItem::AssignmentWord(a, w) => {
                    // `declare a=(1 2)`, `local x=$y`: an assignment as an
                    // argument of a declaration builtin.
                    declaration_args.push((words.len(), a.clone(), w.clone()));
                    words.push(w.clone());
                }
                ast::CommandPrefixOrSuffixItem::ProcessSubstitution(..) => {
                    return Err(self.unsupported("process substitution (`<(…)`, `>(…)`)"));
                }
            }
        }
        Ok(SimpleParts {
            assignments,
            words,
            redirects,
            declaration_args,
        })
    }

    fn resolve(&self, name: &str) -> Resolved {
        if let Some(b) = crate::builtins::special(name) {
            return Resolved::Special(b);
        }
        if let Some(body) = self.functions.get(name) {
            return Resolved::Function(Arc::clone(body));
        }
        if let Some(b) = crate::builtins::regular(name) {
            return Resolved::Builtin(b);
        }
        if crate::commands::FILE_ACTIONS.contains(&name) {
            return Resolved::FileAction;
        }
        if let Some(c) = crate::commands::lookup(name) {
            return Resolved::Tier1(c);
        }
        Resolved::Other
    }

    /// How `type` and `command -v` describe `name`.
    pub(crate) fn describe(&self, name: &str) -> Option<&'static str> {
        if self.aliases.contains_key(name) {
            return Some("alias");
        }
        if crate::builtins::is_keyword(name) {
            return Some("keyword");
        }
        Some(match self.resolve(name) {
            Resolved::Function(_) => "function",
            Resolved::Special(_) | Resolved::Builtin(_) => "builtin",
            Resolved::Tier1(_) | Resolved::FileAction => "tilo",
            Resolved::Other => return None,
        })
    }

    pub(crate) fn run_tier1(
        &self,
        name: &str,
        command: crate::commands::Command,
        args: &[String],
    ) -> i32 {
        let io = self.stdio();
        let env = self.vars.exported();
        let dispatch = crate::dispatch::ShellDispatch::new(self);
        let ctx = crate::commands::Ctx {
            host: self.host.as_ref(),
            cwd: &self.cwd,
            io: &io,
            env: &env,
            name,
            dispatch: &dispatch,
            render: self.render(),
            run: self.run.as_deref(),
        };
        command(&ctx, args)
    }

    /// Who output is for: an agent when the caller said so, or when
    /// `USH_RENDER=agent` asks for it.
    pub(crate) fn render(&self) -> crate::commands::Render {
        match self.vars.scalar("USH_RENDER").as_deref() {
            Some("agent") => crate::commands::Render::Agent,
            Some("text") => crate::commands::Render::Text,
            _ => self.render,
        }
    }

    /// A command that is none of the shell's own: the host's, if it can run
    /// it; else a script the shell can read itself; else the plain answer of
    /// which tier it needs.
    pub(crate) fn run_other(&self, argv: &[String]) -> i32 {
        let Some(name) = argv.first() else {
            return 0;
        };
        let io = self.stdio();
        let env = self.vars.exported();
        match self.host.program(argv, &env, &self.cwd, &io) {
            Program::Exited(status) => {
                if status == 130 {
                    self.host.clear_interrupt();
                }
                status
            }
            Program::NotFound => {
                if name.contains('/')
                    && let Some(status) = self.run_script_file(argv)
                {
                    return status;
                }
                self.error(format!("{name}: command not found"));
                127
            }
            Program::NeedsTier => {
                if name.contains('/')
                    && let Some(status) = self.run_script_file(argv)
                {
                    return status;
                }
                self.error(format!(
                    "{name}: not one of Tilo's own commands. Running it needs a WebAssembly \
                     tool (tier 2) or a Linux place (tier 3), and neither is available here yet"
                ));
                127
            }
        }
    }

    /// `./script.sh` where the host runs no programs: a shell script the
    /// shell reads itself, in a subshell, with its arguments.
    fn run_script_file(&self, argv: &[String]) -> Option<i32> {
        let name = argv.first()?;
        let path = crate::paths::join(&self.cwd, name);
        let Ok(mut reader) = self.host.open_read(&path) else {
            return None;
        };
        let mut bytes = Vec::new();
        if std::io::Read::read_to_end(&mut reader, &mut bytes).is_err() {
            return None;
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let first = text.lines().next().unwrap_or("");
        let shell_script = !first.starts_with("#!")
            || ["/sh", "/bash", "/ush", "env sh", "env bash"]
                .iter()
                .any(|s| first.trim_end().ends_with(s));
        if !shell_script {
            return None;
        }
        let mut sub = self.clone();
        sub.functions.clear();
        sub.set_args(name.clone(), argv.get(1..).unwrap_or(&[]).to_vec());
        sub.interactive = false;
        let status = sub.run_to_exit(&text);
        drop(sub);
        Some(status)
    }

    pub(crate) fn call_function(
        &mut self,
        name: &str,
        body: &ast::FunctionBody,
        args: &[String],
    ) -> Exec {
        if self.call_depth >= MAX_CALL_DEPTH {
            self.error(format!("{name}: maximum function nesting level exceeded"));
            return Err(Flow::Fatal(1));
        }
        let saved_args = std::mem::replace(&mut self.positional, args.to_vec());
        self.vars.push_frame();
        self.call_depth = self.call_depth.saturating_add(1);
        let saved_loop = std::mem::replace(&mut self.loop_depth, 0);
        let ast::FunctionBody(compound, redirects) = body;
        let result = match self.redirect(redirects.as_ref().map_or(&[][..], |r| r.0.as_slice())) {
            Ok(saved) => {
                let r = self.run_compound(compound);
                self.restore(saved);
                r
            }
            Err(flow) => Err(flow),
        };
        self.loop_depth = saved_loop;
        self.call_depth = self.call_depth.saturating_sub(1);
        self.vars.pop_frame();
        self.positional = saved_args;
        match result {
            Err(Flow::Return(status)) => Ok(status),
            other => other,
        }
    }

    /// An alias's text in place of the command's name, re-read with the
    /// rest of the command.
    fn alias_for(&self, simple: &ast::SimpleCommand) -> Option<String> {
        if self.aliases.is_empty() || simple.prefix.is_some() {
            return None;
        }
        let name = simple.word_or_name.as_ref()?;
        let value = self.aliases.get(&name.value)?;
        let rest = simple
            .suffix
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        // An alias that names itself is not expanded again.
        let first = value.split_whitespace().next().unwrap_or("");
        if first == name.value {
            return Some(format!("\\{value} {rest}"));
        }
        Some(format!("{value} {rest}"))
    }

    /// Carry out one assignment.
    pub(crate) fn assign(&mut self, a: &ast::Assignment) -> Exec {
        let result = match (&a.name, &a.value) {
            (ast::AssignmentName::VariableName(name), ast::AssignmentValue::Scalar(v)) => {
                let mut value = self.expand_assignment_value(&v.value)?;
                if self.vars.get(name).is_some_and(|var| var.integer) {
                    value = self.arith_text(&value)?.to_string();
                }
                if a.append {
                    let old = self.vars.scalar(name).unwrap_or_default();
                    value = format!("{old}{value}");
                }
                let r = self.vars.set(name, value);
                if self.options.allexport {
                    self.vars.declare(name).exported = true;
                }
                r
            }
            (
                ast::AssignmentName::ArrayElementName(name, index),
                ast::AssignmentValue::Scalar(v),
            ) => {
                let value = self.expand_word(&v.value)?;
                let assoc = matches!(
                    self.vars.get(name).and_then(|v| v.value.as_ref()),
                    Some(crate::vars::Value::Assoc(_))
                );
                if assoc {
                    let key = self.expand_word(index)?;
                    let value = if a.append {
                        let old = self.vars.get(name).and_then(|v| match &v.value {
                            Some(crate::vars::Value::Assoc(m)) => m.get(&key).cloned(),
                            _ => None,
                        });
                        format!("{}{value}", old.unwrap_or_default())
                    } else {
                        value
                    };
                    self.vars.set_key(name, key, value)
                } else {
                    let i = self.arith_text(index)?;
                    let i = self.index_from_end(name, i);
                    if i < 0 {
                        self.error(format!("{name}[{index}]: bad array subscript"));
                        return Err(Flow::Fatal(1));
                    }
                    let value = if a.append {
                        let old = self.vars.get(name).and_then(|v| match &v.value {
                            Some(crate::vars::Value::Indexed(m)) => m.get(&i).cloned(),
                            Some(crate::vars::Value::Scalar(s)) if i == 0 => Some(s.clone()),
                            _ => None,
                        });
                        format!("{}{value}", old.unwrap_or_default())
                    } else {
                        value
                    };
                    self.vars.set_index(name, i, value)
                }
            }
            (ast::AssignmentName::VariableName(name), ast::AssignmentValue::Array(items)) => {
                self.assign_array(name, items, a.append)
            }
            (ast::AssignmentName::ArrayElementName(name, _), ast::AssignmentValue::Array(_)) => {
                self.error(format!("{name}: cannot assign a list to an array member"));
                return Err(Flow::Error(1));
            }
        };
        result.map_err(|e| {
            self.error(e);
            if self.interactive {
                Flow::Fatal(1)
            } else {
                Flow::Exit(1)
            }
        })?;
        Ok(0)
    }

    fn assign_array(
        &mut self,
        name: &str,
        items: &[(Option<ast::Word>, ast::Word)],
        append: bool,
    ) -> Result<(), crate::vars::ReadOnly> {
        let assoc = matches!(
            self.vars.get(name).and_then(|v| v.value.as_ref()),
            Some(crate::vars::Value::Assoc(_))
        );
        if assoc {
            let mut map = match (append, self.vars.get(name).and_then(|v| v.value.clone())) {
                (true, Some(crate::vars::Value::Assoc(m))) => m,
                _ => std::collections::BTreeMap::new(),
            };
            for (key, value) in items {
                let Some(key) = key else { continue };
                let (Ok(k), Ok(v)) = (self.expand_word(&key.value), self.expand_word(&value.value))
                else {
                    continue;
                };
                map.insert(k, v);
            }
            return self.vars.set_value(name, crate::vars::Value::Assoc(map));
        }
        let mut map = match (append, self.vars.get(name).and_then(|v| v.value.clone())) {
            (true, Some(crate::vars::Value::Indexed(m))) => m,
            (true, Some(crate::vars::Value::Scalar(s))) => std::iter::once((0, s)).collect(),
            _ => std::collections::BTreeMap::new(),
        };
        let mut next = map.keys().next_back().map_or(0, |k| k.saturating_add(1));
        for (key, value) in items {
            if let Some(key) = key {
                let Ok(i) = self.arith_text(&key.value) else {
                    continue;
                };
                let Ok(v) = self.expand_word(&value.value) else {
                    continue;
                };
                map.insert(i, v);
                next = i.saturating_add(1);
            } else {
                let Ok(values) = self.expand_words(std::slice::from_ref(value)) else {
                    continue;
                };
                for v in values {
                    map.insert(next, v);
                    next = next.saturating_add(1);
                }
            }
        }
        self.vars.set_value(name, crate::vars::Value::Indexed(map))
    }

    fn temporary_assignments(
        &mut self,
        assignments: &[&ast::Assignment],
    ) -> Result<Vec<(String, Option<crate::vars::Var>)>, Flow> {
        let mut saved = Vec::new();
        for a in assignments {
            let name = match &a.name {
                ast::AssignmentName::VariableName(n)
                | ast::AssignmentName::ArrayElementName(n, _) => n.clone(),
            };
            if !saved.iter().any(|(n, _)| *n == name) {
                saved.push((name.clone(), self.vars.get(&name).cloned()));
            }
            self.assign(a)?;
            self.vars.declare(&name).exported = true;
        }
        Ok(saved)
    }

    fn restore_assignments(&mut self, saved: Vec<(String, Option<crate::vars::Var>)>) {
        for (name, old) in saved.into_iter().rev() {
            match old {
                Some(var) => {
                    let _ = self.vars.unset(&name);
                    *self.vars.declare(&name) = var;
                }
                None => {
                    let _ = self.vars.unset(&name);
                }
            }
        }
    }

    /// Apply redirections, answering what they replaced so [`Self::restore`]
    /// can put it back.
    pub(crate) fn redirect(
        &mut self,
        redirects: &[ast::IoRedirect],
    ) -> Result<Vec<(u32, Option<Fd>)>, Flow> {
        let mut saved: Vec<(u32, Option<Fd>)> = Vec::new();
        for r in redirects {
            match self.redirect_one(r) {
                Ok(changes) => {
                    for (fd, new) in changes {
                        if !saved.iter().any(|(s, _)| *s == fd) {
                            saved.push((fd, self.fds.get(&fd).cloned()));
                        }
                        match new {
                            Some(new) => {
                                self.fds.insert(fd, new);
                            }
                            None => {
                                self.fds.remove(&fd);
                            }
                        }
                    }
                }
                Err(flow) => {
                    self.restore(saved);
                    return Err(flow);
                }
            }
        }
        Ok(saved)
    }

    pub(crate) fn restore(&mut self, saved: Vec<(u32, Option<Fd>)>) {
        for (fd, old) in saved.into_iter().rev() {
            match old {
                Some(old) => {
                    self.fds.insert(fd, old);
                }
                None => {
                    self.fds.remove(&fd);
                }
            }
        }
    }

    fn redirect_target(&mut self, word: &ast::Word) -> Result<String, Flow> {
        let fields = self.expand_words(std::slice::from_ref(word))?;
        if let [one] = fields.as_slice() {
            Ok(one.clone())
        } else {
            self.error(format!("{}: ambiguous redirect", word.value));
            Err(Flow::Fatal(1))
        }
    }

    fn open_out(&self, target: &str, mode: crate::host::WriteMode) -> Result<Output, Flow> {
        let path = crate::paths::join(&self.cwd, target);
        if target == "/dev/null" {
            return Ok(Output::null());
        }
        if target == "/dev/stdout" {
            return Ok(self.output(1).unwrap_or_else(Output::null));
        }
        if target == "/dev/stderr" {
            return Ok(self.output(2).unwrap_or_else(Output::null));
        }
        match self.host.open_write(&path, mode, self.run.as_deref()) {
            Ok(writer) => Ok(Output::new(writer)),
            Err(e) => {
                self.error(format!("{target}: {}", crate::commands::describe(&e)));
                Err(Flow::Error(1))
            }
        }
    }

    /// `<`, `>`, `>>`, `>|`, `<&` and `>&` with a file or a descriptor.
    fn redirect_file(
        &mut self,
        fd: Option<i32>,
        kind: &ast::IoFileRedirectKind,
        target: &ast::IoFileRedirectTarget,
    ) -> Result<Vec<(u32, Option<Fd>)>, Flow> {
        use crate::host::WriteMode;
        use ast::IoFileRedirectKind as K;
        let target_word = match target {
            ast::IoFileRedirectTarget::Filename(w) | ast::IoFileRedirectTarget::Duplicate(w) => {
                w.clone()
            }
            ast::IoFileRedirectTarget::Fd(n) => ast::Word::new(&n.to_string()),
            ast::IoFileRedirectTarget::ProcessSubstitution(..) => {
                return Err(self.unsupported("process substitution (`<(…)`, `>(…)`)"));
            }
        };
        let target = self.redirect_target(&target_word)?;
        match kind {
            K::Read => {
                let fd = fd_of(fd, 0);
                if target == "/dev/null" {
                    return Ok(vec![(fd, Some(Fd::In(Input::null())))]);
                }
                if target == "/dev/stdin" {
                    return Ok(vec![(fd, self.fds.get(&0).cloned())]);
                }
                let path = crate::paths::join(&self.cwd, &target);
                match self.host.open_read(&path) {
                    Ok(reader) => Ok(vec![(fd, Some(Fd::In(Input::new(reader))))]),
                    Err(e) => {
                        self.error(format!("{target}: {}", crate::commands::describe(&e)));
                        Err(Flow::Error(1))
                    }
                }
            }
            K::Write | K::Clobber | K::Append => {
                let fd = fd_of(fd, 1);
                let mode = match kind {
                    K::Append => WriteMode::Append,
                    K::Write if self.options.noclobber => WriteMode::CreateNew,
                    _ => WriteMode::Truncate,
                };
                let out = self.open_out(&target, mode)?;
                Ok(vec![(fd, Some(Fd::Out(out)))])
            }
            K::ReadAndWrite => {
                Err(self.unsupported("opening a file for reading and writing (`<>`)"))
            }
            K::DuplicateInput | K::DuplicateOutput => {
                let default = match kind {
                    K::DuplicateInput => 0,
                    _ => 1,
                };
                let unnumbered = fd.is_none();
                let fd = fd_of(fd, default);
                if target == "-" {
                    return Ok(vec![(fd, None)]);
                }
                let (source, close) = match target.strip_suffix('-') {
                    Some(n) => (n.to_string(), true),
                    None => (target.clone(), false),
                };
                if let Ok(n) = source.parse::<u32>() {
                    let Some(existing) = self.fds.get(&n).cloned() else {
                        self.error(format!("{n}: bad file descriptor"));
                        return Err(Flow::Fatal(1));
                    };
                    let mut changes = vec![(fd, Some(existing))];
                    if close {
                        changes.push((n, None));
                    }
                    return Ok(changes);
                }
                if matches!(kind, K::DuplicateOutput) && unnumbered {
                    // `>& file`: both outputs to the file.
                    let out = self.open_out(&target, WriteMode::Truncate)?;
                    return Ok(vec![
                        (1, Some(Fd::Out(out.clone()))),
                        (2, Some(Fd::Out(out))),
                    ]);
                }
                self.error(format!("{target}: ambiguous redirect"));
                Err(Flow::Fatal(1))
            }
        }
    }

    fn redirect_one(&mut self, r: &ast::IoRedirect) -> Result<Vec<(u32, Option<Fd>)>, Flow> {
        match r {
            ast::IoRedirect::File(fd, kind, target) => self.redirect_file(*fd, kind, target),
            ast::IoRedirect::HereDocument(fd, doc) => {
                let fd = fd_of(*fd, 0);
                let body = if doc.requires_expansion {
                    self.expand_string(&doc.doc.value)?
                } else {
                    doc.doc.value.clone()
                };
                Ok(vec![(fd, Some(Fd::In(Input::bytes(body.into_bytes()))))])
            }
            ast::IoRedirect::HereString(fd, word) => {
                let fd = fd_of(*fd, 0);
                let mut text = self.expand_word(&word.value)?;
                text.push('\n');
                Ok(vec![(fd, Some(Fd::In(Input::bytes(text.into_bytes()))))])
            }
            ast::IoRedirect::OutputAndError(word, append) => {
                let target = self.redirect_target(word)?;
                let mode = if *append {
                    crate::host::WriteMode::Append
                } else {
                    crate::host::WriteMode::Truncate
                };
                let out = self.open_out(&target, mode)?;
                Ok(vec![
                    (1, Some(Fd::Out(out.clone()))),
                    (2, Some(Fd::Out(out))),
                ])
            }
        }
    }
}

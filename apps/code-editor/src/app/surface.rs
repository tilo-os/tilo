//! What the editor does for the view it is laid out in: the effects it
//! answers, the rows it publishes beside the project's files, and the view
//! state a screenshot scenario stages.

use std::path::PathBuf;

use ui::framework::{EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow};
use ui::tilo_resource::Effect as _;

use super::{AutoCommit, CodeApp, Command, PanelTab, Staging, TreeId};
use crate::actions;
use crate::effects::CodeEditorEffect;
use crate::project::Project;

/// How many files, symbols and search hits the published tree carries; the
/// rest resolve by id.
pub(super) const ROW_LIMIT: usize = actions::EAGER_RESOURCE_SNAPSHOT_LIMIT;

/// How long the terminal scenario lets the shell answer before the capture.
const SHELL_ANSWER: ui::time::Duration = ui::time::Duration::from_millis(1500);

impl CodeApp {
    /// Follow the project the view's document holds, with the host's
    /// dispatcher, waker and preferences; a capture opens the sample project
    /// its fixture wrote.
    pub(super) fn follow(&mut self, cx: &ui::framework::ViewCx<Project>) {
        self.set_highlight_waker(cx.waker());
        self.attach_framework(
            cx.dispatcher(),
            cx.mode().is_live(),
            cx.settings(),
            cx.live().clone(),
        );
        #[cfg(feature = "samples")]
        if cx.mode() == ui::framework::RunMode::Capture
            && let Some(sample) = cx.fixture::<crate::resource::SampleProject>()
        {
            self.open_sample(&sample.0);
        }
    }

    /// What the editor shares with the view: what it publishes, and what a
    /// capture staged.
    pub(super) fn store(&self) -> super::view::Shared {
        std::rc::Rc::clone(&self.store)
    }

    /// Show the project the document holds, and its terminal session, when
    /// it changed since the editor last showed it. Shared file records are
    /// adopted only: an unpublished local tree is never wiped.
    pub(super) fn follow_document(&mut self) {
        let Some(shown) = self
            .live_state
            .as_ref()
            .and_then(|live| live.changed_since(&mut self.followed))
        else {
            return;
        };
        self.adopt_resource_state(&shown);
        self.present_terminal(shown.terminal_panel.as_ref());
    }

    /// Stage the scenarios a capture asked for since the editor last looked.
    pub(super) fn follow_store(&mut self) {
        let staged = std::mem::take(&mut self.store.borrow_mut().staged);
        for scenario in staged {
            self.stage_scenario(scenario.as_deref());
        }
    }

    fn answer_text_read(effect: &CodeEditorEffect, cx: &EffectCx<Project>) -> Option<EffectAnswer> {
        let answer = || match effect {
            CodeEditorEffect::ReadText {
                file,
                start_chars,
                max_chars,
            } => {
                let document = cx.document().ok_or("current document is unavailable")?;
                crate::text_operations::read(&document, file, *start_chars, *max_chars)
                    .map(Effected::value)
            }
            _ => Err("effect is not a text read".into()),
        };
        matches!(effect, CodeEditorEffect::ReadText { .. }).then(answer)
    }

    pub(super) fn answer_effect(
        &mut self,
        effect: CodeEditorEffect,
        cx: &mut EffectCx<Project>,
    ) -> EffectAnswer {
        if let Some(answer) = Self::answer_text_read(&effect, cx) {
            return answer;
        }
        if let Some(command) = effect.command() {
            return self.answer_command(command, cx);
        }
        match effect {
            CodeEditorEffect::ShowFile { file } => {
                let node = self.file_node(&file)?;
                self.open_node(node);
            }
            reveal @ CodeEditorEffect::Reveal { .. } => return self.answer_reveal(reveal, cx),
            CodeEditorEffect::OpenFile { path: Some(path) } => {
                self.ensure_no_file_effect()?;
                self.notice = None;
                if self.open_disk_file(&path).transpose()?.is_none() {
                    self.file_reply = Some(cx.reply());
                    return Ok(Effected::Pending);
                }
            }
            CodeEditorEffect::OpenFolder { path: Some(path) } => {
                self.ensure_no_file_effect()?;
                self.notice = None;
                if self.open_project_dir(&path).transpose()?.is_none() {
                    self.file_reply = Some(cx.reply());
                    return Ok(Effected::Pending);
                }
            }
            CodeEditorEffect::Save { file, path } => {
                self.ensure_no_file_effect()?;
                let node = match file {
                    Some(file) => self.file_node(&file)?,
                    None => self
                        .session
                        .active_buffer()
                        .map(|buffer| buffer.node)
                        .ok_or_else(|| "no file is open".to_string())?,
                };
                let has_destination = path.is_some()
                    || self
                        .session
                        .buffer_of(node)
                        .is_some_and(|buffer| buffer.path.is_some());
                if has_destination {
                    self.pending_save = Some((
                        node,
                        path.map(ui::tilo_resource::AbsolutePath::into_path_buf),
                    ));
                } else {
                    self.request_save_as(node);
                }
                self.file_reply = Some(cx.reply());
                return Ok(Effected::Pending);
            }
            CodeEditorEffect::GoToLine { value: Some(line) } => {
                let line = line
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| format!("{line:?} is not a line number"))?;
                self.go_to_line(line.max(1));
            }
            CodeEditorEffect::ToggleLanguageServers => self.toggle_setting(
                |settings| &mut settings.language_servers,
                ("Language servers on", "Language servers off"),
            )?,
            CodeEditorEffect::ToggleFormatOnSave => self.toggle_setting(
                |settings| &mut settings.format_on_save,
                ("Format on save on", "Format on save off"),
            )?,
            other => return Err(format!("`{}` is not handled here", other.id())),
        }
        Ok(Effected::done())
    }

    /// Show the location a [`CodeEditorEffect::Reveal`] names. A path in a
    /// folder the tree has not read is answered once it is read.
    fn answer_reveal(
        &mut self,
        effect: CodeEditorEffect,
        cx: &mut EffectCx<Project>,
    ) -> EffectAnswer {
        let CodeEditorEffect::Reveal {
            file,
            path,
            line,
            column,
            results,
        } = effect
        else {
            return Err("effect is not a reveal".into());
        };
        let reveal = move |app: &mut Self, target: TreeId| {
            app.explorer.show(&app.session.project);
            app.open_node(target);
            app.pending_jump = Some((line, column));
            if results {
                app.panel_visible = true;
                app.panel.set_tab(PanelTab::Search);
            }
        };
        if let Some(target) = file
            .as_deref()
            .and_then(|file| self.session.project.node_for_resource(file))
        {
            reveal(self, target);
            return Ok(Effected::done());
        }
        let path = path.ok_or_else(|| "Code location no longer exists".to_string())?;
        self.ensure_no_file_effect()?;
        let answered = std::rc::Rc::new(std::cell::Cell::new(None));
        let noted = std::rc::Rc::clone(&answered);
        self.reveal_on_disk(path.into_path_buf(), move |app, target| {
            let answer = target
                .map(|target| reveal(app, target))
                .ok_or_else(|| "Code location no longer exists".to_string());
            // Answered by the effect itself when the reveal waited on nothing.
            if app.file_reply.is_some() {
                app.finish_file_effect(answer);
            } else {
                noted.set(Some(answer));
            }
        });
        if let Some(answer) = answered.take() {
            answer?;
            return Ok(Effected::done());
        }
        self.file_reply = Some(cx.reply());
        Ok(Effected::Pending)
    }

    /// Flip one of the person's preferences and say what it is now.
    fn toggle_setting(
        &mut self,
        field: fn(&mut crate::settings::CodeSettings) -> &mut bool,
        (on, off): (&'static str, &'static str),
    ) -> Result<(), String> {
        let settings = self
            .settings
            .as_ref()
            .ok_or_else(|| "preferences are not available here".to_string())?;
        settings
            .update(|settings| {
                let value = field(settings);
                *value = !*value;
            })
            .map_err(|error| error.to_string())?;
        let now = *field(&mut settings.get());
        self.note(if now { on } else { off }, false);
        Ok(())
    }

    /// Run one of the editor's commands. Those that need the document run on the
    /// next sync; those that ask the user for a file answer once they did.
    fn answer_command(&mut self, command: Command, cx: &mut EffectCx<Project>) -> EffectAnswer {
        if command == Command::NextTheme {
            return Err("Appearance is controlled by the operating system; choose it in Appearance settings".into());
        }
        match command {
            Command::OpenFolder | Command::OpenFile | Command::SaveAs => {
                self.ensure_no_file_effect()?;
                match command {
                    Command::OpenFolder => self.request_open_folder(),
                    Command::OpenFile => self.request_open_file(),
                    _ => {
                        let node = self
                            .session
                            .active_buffer()
                            .map(|buffer| buffer.node)
                            .ok_or_else(|| "no file is open".to_string())?;
                        self.request_save_as(node);
                    }
                }
                self.notice = None;
                self.file_reply = Some(cx.reply());
                Ok(Effected::Pending)
            }
            Command::Save => self.answer_effect(
                CodeEditorEffect::Save {
                    file: None,
                    path: None,
                },
                cx,
            ),
            Command::GoToLine => {
                self.ask_for_line();
                Ok(Effected::done())
            }
            command => {
                self.pending_command = Some(command);
                Ok(Effected::done())
            }
        }
    }

    fn file_node(&self, file: &str) -> Result<TreeId, String> {
        self.session
            .project
            .node_for_resource(file)
            .ok_or_else(|| "Code file no longer exists".to_string())
    }

    /// One file operation at a time: the picker and the conflict prompt are
    /// one question each.
    fn ensure_no_file_effect(&self) -> Result<(), String> {
        if self.file_reply.is_some() || self.portal.is_some() {
            return Err("another file operation is already running".into());
        }
        Ok(())
    }

    /// Write `node` to `target` (a new destination), or to its own file.
    pub(super) fn save_to(
        &mut self,
        doc: &AutoCommit,
        node: TreeId,
        target: Option<PathBuf>,
        then: super::file_workflow::AfterSave,
    ) {
        if let Some(path) = target {
            let name = path.file_name().map_or_else(
                || "untitled".to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            self.session.project.set_file_path(node, &path, &name);
            if let Some(buffer) = self.session.buffer_of_mut(node) {
                buffer.path = Some(path);
                buffer.name = name;
                buffer.stamp = None;
            }
            self.invalidate_search_projection();
        }
        self.save_buffer_then(doc, node, true, then);
    }

    /// What the file effects can do right now.
    fn effect_statuses(&self) -> Vec<EffectStatus> {
        let mut statuses = vec![EffectStatus::unavailable(
            CodeEditorEffect::NEXT_THEME,
            "code_editor.os_appearance",
            "Appearance is controlled by the OS; choose a theme in Theme Studio",
        )];
        if self.file_reply.is_some() || self.portal.is_some() {
            const CODE: &str = "code_editor.busy";
            const REASON: &str = "Another file operation is still running";
            statuses.extend([
                EffectStatus::unavailable(CodeEditorEffect::OPEN_FILE, CODE, REASON),
                EffectStatus::unavailable(CodeEditorEffect::OPEN_FOLDER, CODE, REASON),
                EffectStatus::unavailable(CodeEditorEffect::SAVE, CODE, REASON),
                EffectStatus::unavailable(CodeEditorEffect::SAVE_AS, CODE, REASON),
            ]);
        }
        if self.session.active_buffer().is_none() {
            const CODE: &str = "code_editor.no_file";
            const REASON: &str = "No file is open";
            // What acts on the open file (its text, its lines, the caret's
            // place) has nothing to act on.
            statuses.extend([
                EffectStatus::unavailable(CodeEditorEffect::REVERT_FILE, CODE, REASON),
                EffectStatus::unavailable(CodeEditorEffect::SAVE_AS, CODE, REASON),
                EffectStatus::unavailable(CodeEditorEffect::FORMAT_FILE, CODE, REASON),
                EffectStatus::unavailable(CodeEditorEffect::FIND, CODE, REASON),
                EffectStatus::unavailable(CodeEditorEffect::REPLACE, CODE, REASON),
                EffectStatus::unavailable(CodeEditorEffect::GO_TO_LINE, CODE, REASON),
                EffectStatus::unavailable(CodeEditorEffect::DUPLICATE_LINE, CODE, REASON),
                EffectStatus::unavailable(CodeEditorEffect::MOVE_LINE_UP, CODE, REASON),
                EffectStatus::unavailable(CodeEditorEffect::MOVE_LINE_DOWN, CODE, REASON),
                EffectStatus::unavailable(CodeEditorEffect::DELETE_LINE, CODE, REASON),
                EffectStatus::unavailable(CodeEditorEffect::TOGGLE_COMMENT, CODE, REASON),
            ]);
        }
        statuses
    }

    /// The row of the file at `node`, by its resource id: open it and read
    /// it. What the open file adds (saving it) is the view's
    /// ([`super::view::Shown`]).
    fn file_row(&self, node: TreeId) -> Option<(String, LiveRow)> {
        let item = self.session.project.get(node)?;
        if item.is_folder() {
            return None;
        }
        let row = LiveRow::new(
            actions::file_resource_id(&item.resource_id),
            crate::actions::FILE_KIND,
            item.name.clone(),
        )
        .effect(&CodeEditorEffect::ShowFile {
            file: item.resource_id.clone(),
        })
        .operation(
            CodeEditorEffect::ReadText {
                file: item.resource_id.clone(),
                start_chars: 0,
                max_chars: 1024,
            }
            .bound_asking(&["start_chars", "max_chars"]),
        )
        .operation(crate::text_operations::snapshot_spec(
            item.resource_id.clone(),
        ));
        Some((item.resource_id.clone(), row))
    }

    /// Keep what the editor shows where the view publishes it: the project's
    /// files, symbols and search hits once they were projected again, the
    /// file on screen and what the effects can do every time.
    pub(super) fn publish_shown(&mut self) {
        let projected = std::mem::take(&mut self.shown_stale).then(|| {
            let files: Vec<(String, LiveRow)> = self
                .session
                .project
                .nodes
                .keys()
                .filter_map(|node| self.file_row(*node))
                .collect();
            (
                std::rc::Rc::new(files),
                std::rc::Rc::new(self.search_symbols.clone()),
                std::rc::Rc::new(self.projected_search_results.clone()),
                std::rc::Rc::new(self.search_entries.clone()),
            )
        });
        let open = self.session.active_buffer().and_then(|buffer| {
            let file = self.session.project.get(buffer.node)?;
            Some((file.resource_id.clone(), buffer.path.is_some()))
        });
        let statuses = self.effect_statuses();
        let mut store = self.store.borrow_mut();
        let shown = &mut store.shown;
        if let Some((files, symbols, results, entries)) = projected {
            shown.files = files;
            shown.symbols = symbols;
            shown.results = results;
            shown.entries = entries;
        }
        shown.open = open;
        shown.statuses = statuses;
    }

    /// Open the sample project a capture's fixture wrote, on its main file.
    #[cfg(feature = "samples")]
    pub(crate) fn open_sample(&mut self, root: &std::path::Path) {
        if matches!(self.open_project_dir(root), Some(Ok(()))) {
            let main = crate::disk::canonical(&root.join("src").join("main.rs"));
            if let Some(node) = self.session.project.reveal_path(&main) {
                self.explorer.show(&self.session.project);
                self.open_node(node);
            }
        }
        self.notice = None;
    }

    /// Stage `scenario`: what it shows is set once the file it is about is
    /// open and bound ([`Self::continue_staging`]).
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        if scenario == Some("terminal") {
            self.shots_open_terminal();
        }
        self.staging = Some(Staging {
            scenario: scenario.map(str::to_owned),
        });
        #[cfg(feature = "samples")]
        if scenario == Some("hardware")
            && let Some(root) = self.project_root()
        {
            let path = crate::disk::canonical(&root.join(crate::sample_project::HARDWARE_FILE));
            if let Some(node) = self.session.project.reveal_path(&path) {
                self.explorer.show(&self.session.project);
                self.open_node(node);
            }
        }
    }

    /// Set the staged scenario's view state once the editor holds its file.
    pub(super) fn continue_staging(&mut self, doc: &AutoCommit) {
        let Some(Staging { scenario }) = self.staging.clone() else {
            return;
        };
        if let Some(until) = self.staging_until {
            if ui::time::now() >= until {
                self.staging = None;
                self.staging_until = None;
            }
            return;
        }
        if !self.editor.is_bound() {
            return;
        }
        match scenario.as_deref() {
            // A missing fixture must leave capture staging incomplete instead
            // of silently capturing the default Rust file.
            Some("hardware")
                if self
                    .session
                    .active_buffer()
                    .is_none_or(|buffer| buffer.name != "signed_mac.sv") =>
            {
                return;
            }
            Some("terminal") => {
                self.shots_terminal_input(terminal::demo_session_input());
                self.staging_until = Some(ui::time::now() + SHELL_ANSWER);
                return;
            }
            Some("selection") => self.shots_select_words(),
            Some("completion") => self.shots_completion_demo(doc),
            Some("search") => self.shots_project_search("fn main"),
            Some("conflict") => self.shots_conflict(),
            Some("indexing") => self.shots_indexing(),
            Some("close-prompt") => self.shots_close_prompt(),
            Some("status") => self.shots_status(),
            Some("narrow-sidebar") => self.shots_snap_sidebar(true),
            _ => {}
        }
        self.staging = None;
    }
}

/// A declaration the symbol scan found, opening where it is declared.
pub(super) fn symbol_row(symbol: &actions::SymbolRef) -> LiveRow {
    LiveRow::new(
        actions::symbol_resource_id(symbol),
        "code_editor.symbol",
        symbol.name.clone(),
    )
    .effect(&CodeEditorEffect::Reveal {
        file: Some(symbol.file_id.clone()),
        path: None,
        line: symbol.line,
        column: symbol.col,
        results: false,
    })
}

/// One hit of the project search, revealed beside the results.
pub(super) fn result_row(result: &actions::SearchResultRef) -> LiveRow {
    LiveRow::new(
        actions::result_resource_id(result),
        "code_editor.search-result",
        result.preview.clone(),
    )
    .effect(&CodeEditorEffect::Reveal {
        file: result.file_id.clone(),
        path: result
            .disk_path
            .as_deref()
            .and_then(|path| ui::tilo_resource::AbsolutePath::new(path).ok()),
        line: result.line,
        column: result.col,
        results: true,
    })
}

#[cfg(test)]
impl CodeApp {
    pub(crate) fn active_buffer_name(&self) -> Option<String> {
        self.session
            .active_buffer()
            .map(|buffer| buffer.name.clone())
    }

    pub(crate) fn search_symbols_for_test(&self) -> Vec<actions::SymbolRef> {
        self.search_symbols.clone()
    }

    pub(crate) fn caret_for_test(&self) -> (usize, usize) {
        self.editor.primary_caret()
    }

    pub(crate) fn results_shown_for_test(&self) -> bool {
        self.panel_visible && self.panel.active_tab() == PanelTab::Search
    }

    pub(crate) fn is_staging_for_test(&self) -> bool {
        self.staging.is_some()
    }

    pub(crate) fn search_hit_count_for_test(&self) -> usize {
        self.search_hits.len()
    }

    pub(crate) fn read_buffers_for_test(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.presented.buffers.keys().cloned().collect();
        ids.sort();
        ids
    }

    pub(crate) fn lsp_enabled_for_test(&self) -> bool {
        self.lsp_enabled()
    }
}

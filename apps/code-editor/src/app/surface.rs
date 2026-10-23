//! The editor as the surface over its project resource: the effects it
//! answers, the live rows it publishes beside the project's files, and the
//! view state a screenshot scenario stages.

use std::path::{Path, PathBuf};

use ui::framework::{EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow};
use ui::tilo_resource::Effect as _;

use super::{AutoCommit, CodeApp, Command, PanelTab, Staging, TreeId};
use crate::actions;
use crate::effects::CodeEditorEffect;
use crate::project::Project;

/// How many files, symbols and search hits the published tree carries; the
/// rest resolve by id.
const ROW_LIMIT: usize = actions::EAGER_RESOURCE_SNAPSHOT_LIMIT;

/// How long the terminal scenario lets the shell answer before the capture.
const SHELL_ANSWER: ui::time::Duration = ui::time::Duration::from_millis(1500);

impl CodeApp {
    fn answer_text_read(
        effect: &CodeEditorEffect,
        cx: &EffectCx<Project, Self>,
    ) -> Option<EffectAnswer> {
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

    pub(crate) fn answer_effect(
        &mut self,
        effect: CodeEditorEffect,
        cx: &mut EffectCx<Project, Self>,
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
                if self.open_disk_file(Path::new(&path)).transpose()?.is_none() {
                    self.file_reply = Some(cx.reply());
                    return Ok(Effected::Pending);
                }
            }
            CodeEditorEffect::OpenFolder { path: Some(path) } => {
                self.ensure_no_file_effect()?;
                self.notice = None;
                if self
                    .open_project_dir(Path::new(&path))
                    .transpose()?
                    .is_none()
                {
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
                    self.pending_save = Some((node, path.map(PathBuf::from)));
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
        cx: &mut EffectCx<Project, Self>,
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
        self.reveal_on_disk(PathBuf::from(path), move |app, target| {
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
    fn answer_command(
        &mut self,
        command: Command,
        cx: &mut EffectCx<Project, Self>,
    ) -> EffectAnswer {
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
                Self::ask_for_line();
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
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
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
            let nothing_open = |effect| {
                EffectStatus::unavailable(effect, "code_editor.no_file", "No file is open")
            };
            statuses.extend([nothing_open(CodeEditorEffect::REVERT_FILE)]);
        }
        statuses
    }

    /// The project's files as the editor can open and save them, then the
    /// symbols it found and the hits of the project search. A file row shares
    /// its id with the file the project publishes, so its effects join that
    /// file.
    pub(crate) fn live_rows(&self) -> Vec<LiveRow> {
        let open: Vec<TreeId> = self
            .session
            .buffer
            .iter()
            .map(|buffer| buffer.node)
            .collect();
        let files = open
            .iter()
            .copied()
            .chain(
                self.session
                    .project
                    .nodes
                    .keys()
                    .copied()
                    .filter(|node| !open.contains(node)),
            )
            .filter_map(|node| self.file_row(node))
            .take(ROW_LIMIT);
        let symbols = self.search_symbols.iter().take(ROW_LIMIT).map(symbol_row);
        let results = self
            .search_result_refs()
            .into_iter()
            .take(ROW_LIMIT)
            .map(|result| result_row(&result));
        files.chain(symbols).chain(results).collect()
    }

    /// The live row `id`, beyond the ones [`Self::live_rows`] publishes.
    pub(crate) fn live_row(&self, id: &str) -> Option<LiveRow> {
        if let Some((_, file)) =
            ui::tilo_resource::items::item_of_resource::<crate::project::Project>(id)
        {
            return self
                .session
                .project
                .node_for_resource(file.as_str())
                .and_then(|node| self.file_row(node));
        }
        if let Some(symbol) = self
            .search_symbols
            .iter()
            .find(|symbol| actions::symbol_resource_id(symbol) == id)
        {
            return Some(symbol_row(symbol));
        }
        self.search_result_refs()
            .iter()
            .find(|result| actions::result_resource_id(result) == id)
            .map(result_row)
    }

    /// The row of the file at `node`: open it, and save it when it has a file
    /// on disk to save to.
    fn file_row(&self, node: TreeId) -> Option<LiveRow> {
        let item = self.session.project.get(node)?;
        if item.is_folder() {
            return None;
        }
        let mut row = LiveRow::new(
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
        );
        row = row.operation(crate::text_operations::snapshot_spec(
            item.resource_id.clone(),
        ));
        if self
            .session
            .buffer_of(node)
            .is_some_and(|buffer| buffer.path.is_some())
        {
            row = row.effect(&CodeEditorEffect::Save {
                file: Some(item.resource_id.clone()),
                path: None,
            });
        }
        Some(row)
    }

    /// Open the sample project a capture's fixture wrote, on its main file.
    pub(crate) fn open_sample(&mut self, root: &Path) {
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
fn symbol_row(symbol: &actions::SymbolRef) -> LiveRow {
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
fn result_row(result: &actions::SearchResultRef) -> LiveRow {
    LiveRow::new(
        actions::result_resource_id(result),
        "code_editor.search-result",
        result.preview.clone(),
    )
    .effect(&CodeEditorEffect::Reveal {
        file: result.file_id.clone(),
        path: result.disk_path.clone(),
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

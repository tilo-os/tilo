use super::{AutoCommit, CodeApp, EditorDoc, LspAsk, LspStatus, TreeId};

impl CodeApp {
    /// The `file://` URI for a project node.
    ///
    /// For a disk-backed node this is the real file's URI, which is the whole
    /// point: a server told about `file:///project-name/src/main.rs` when the
    /// file is at `/home/me/tilo/crates/session/shell/src/main.rs` has been told about a
    /// document that does not exist, and answers accordingly. The synthetic
    /// form survives only for the in-memory demo tree, which has no path.
    pub(super) fn node_uri(&self, node: TreeId) -> String {
        match self.session.project.disk_path_of(node) {
            Some(path) => crate::lsp::path_to_uri(&path),
            None => format!("file:///{}", self.session.project.path_of(node)),
        }
    }

    /// LSP `languageId` string for a language (the server's expected token).
    pub(super) fn language_id(lang: crate::model::Language) -> &'static str {
        use crate::model::Language::{
            C, Cpp, Css, Go, Html, JavaScript, Json, Markdown, PlainText, Python, Rust, Shell,
            SystemVerilog, Toml, TypeScript, Verilog, Yaml,
        };
        match lang {
            Rust => "rust",
            Python => "python",
            TypeScript => "typescript",
            JavaScript => "javascript",
            Go => "go",
            Verilog => "verilog",
            SystemVerilog => "systemverilog",
            C => "c",
            Cpp => "cpp",
            Toml => "toml",
            Json => "json",
            Yaml => "yaml",
            Html => "html",
            Css => "css",
            Markdown => "markdown",
            Shell => "shellscript",
            PlainText => "plaintext",
        }
    }

    /// Whether language servers are wanted: in a window someone uses (a
    /// capture or a past version reaches nothing outside the process), when
    /// the person has not turned them off, for a project that is a real
    /// directory to root them at.
    pub(super) fn lsp_enabled(&self) -> bool {
        self.live
            && self
                .settings
                .as_ref()
                .is_some_and(|settings| settings.get().language_servers)
            && self.session.project.is_disk_backed()
    }

    /// Start (or restart) a language server for the active buffer's language,
    /// rooted at the real project directory. A missing binary is reported once
    /// and not retried every frame.
    pub(super) fn maybe_start_lsp(&mut self, doc: &AutoCommit) {
        if !self.lsp_enabled() {
            if self.lsp.is_some() {
                self.reset_language_server();
            }
            return;
        }
        tilo_process::tools::read_login_path();
        let Some(buf) = self.session.active_buffer() else {
            return;
        };
        let (node, lang) = (buf.node, buf.language);
        if self.lsp.is_some() && self.lsp_language == Some(lang) {
            self.open_on_language_server(doc, node, lang);
            return;
        }
        if self.lsp_launching {
            return;
        }
        if crate::lsp::LspClient::server_command(lang).is_none() {
            self.lsp_status = LspStatus::None;
            return; // no server mapping for this language
        }
        if self.lsp_failed.contains(&lang) {
            return; // already reported; do not respawn a missing binary
        }
        let Some(root) = self.session.project.root_path() else {
            return;
        };
        // Launching a server spawns a process and looks at the project
        // directory: on the frame loop that happens on a background thread.
        self.lsp_launching = true;
        let tools = self.lsp_tools.clone();
        self.on_disk(
            "code-language-server",
            move || {
                let tools = tools.unwrap_or_else(tilo_process::tools::ToolDirs::current);
                let started = crate::lsp::LspClient::start_with(lang, &root, &tools);
                (root, started)
            },
            move |app, (root, started)| app.language_server_started(lang, &root, started),
        );
        if self.lsp.is_some() && self.lsp_language == Some(lang) {
            self.open_on_language_server(doc, node, lang);
        }
    }

    /// Make sure the language server serving `lang` has `node` open.
    fn open_on_language_server(
        &mut self,
        doc: &AutoCommit,
        node: TreeId,
        lang: crate::model::Language,
    ) {
        let uri = self.node_uri(node);
        if self.lsp_uri.as_deref() == Some(uri.as_str()) {
            return;
        }
        let text = EditorDoc::read_buffer_key(doc, &self.buffer_key(node)).unwrap_or_default();
        self.lsp_version += 1;
        let version = self.lsp_version;
        if let Some(c) = self.lsp.as_mut() {
            c.did_open(&uri, Self::language_id(lang), version, &text);
        }
        self.lsp_sent_text = text;
        self.lsp_uri = Some(uri);
    }

    /// Fold in launching the server for `lang`. The file it serves is opened
    /// on it at the next sync ([`Self::open_on_language_server`]).
    fn language_server_started(
        &mut self,
        lang: crate::model::Language,
        root: &std::path::Path,
        started: Result<crate::lsp::LspClient, crate::lsp::LspFailure>,
    ) {
        self.lsp_launching = false;
        match started {
            Ok(client) => {
                // Turned off, or another project opened, while it launched.
                if !self.lsp_enabled() || self.session.project.root_path().as_deref() != Some(root)
                {
                    return;
                }
                let server = client.server_name().to_string();
                tracing::info!(
                    "started language server {server} for {} at {}",
                    lang.label(),
                    root.display()
                );
                self.lsp_version = 0;
                self.lsp = Some(client);
                self.lsp_language = Some(lang);
                self.lsp_uri = None;
                self.lsp_status = LspStatus::Starting { server };
            }
            // A server only the login shell's PATH knows is found once that
            // PATH is read; until then the project reads as loading, which
            // keeps frames, and so this lookup, coming.
            Err(crate::lsp::LspFailure::NotInstalled { server })
                if self.lsp_tools.is_none() && !tilo_process::tools::login_path_read() =>
            {
                self.lsp_status = LspStatus::Starting { server };
            }
            Err(e) => {
                self.lsp = None;
                self.lsp_language = None;
                self.lsp_failed.insert(lang);
                self.lsp_status = LspStatus::Failed(e);
            }
        }
    }

    /// Drain events the LSP reader thread produced and fold them into the editor
    /// (diagnostics → panel + squiggles; completion/hover/definition → editor).
    pub(super) fn poll_lsp(&mut self) {
        let events = match self.lsp.as_mut() {
            Some(c) => c.poll(),
            None => return,
        };
        for ev in events {
            match ev {
                crate::lsp::LspEvent::Diagnostics { uri, diagnostics } => {
                    self.lsp_diagnostics.insert(uri, diagnostics);
                    self.apply_diagnostics_to_editor();
                }
                crate::lsp::LspEvent::Completion(items) => {
                    self.editor.show_completions(items);
                }
                crate::lsp::LspEvent::Hover(text) => {
                    self.editor.set_hover(text);
                }
                crate::lsp::LspEvent::Definition(locs) => {
                    self.apply_definition(locs);
                }
                crate::lsp::LspEvent::Formatted(edits) => {
                    if self.formatting.is_some() {
                        self.formatted = Some(edits);
                    }
                }
                crate::lsp::LspEvent::Progress(p) => {
                    let server = self.lsp_server_name();
                    // Folding through the client's table is what keeps the task
                    // label stable: a `report` carries no title, only the token.
                    let shown = self.lsp.as_mut().and_then(|c| c.note_progress(&p));
                    self.lsp_status = match shown {
                        Some(p) => LspStatus::Busy {
                            server,
                            title: p.title,
                            detail: p.message,
                            percentage: p.percentage,
                        },
                        None => LspStatus::Ready { server },
                    };
                }
                crate::lsp::LspEvent::Initialized => {
                    // Only claim ready if nothing is still in flight; the
                    // initialize response usually lands before indexing starts.
                    if !self
                        .lsp
                        .as_ref()
                        .is_some_and(crate::lsp::LspClient::is_busy)
                    {
                        self.lsp_status = LspStatus::Ready {
                            server: self.lsp_server_name(),
                        };
                    }
                }
                crate::lsp::LspEvent::Exited => {
                    let server = self.lsp_server_name();
                    let detail = self
                        .lsp
                        .as_mut()
                        .and_then(crate::lsp::LspClient::exit_detail);
                    self.lsp = None;
                    self.lsp_language = None;
                    self.lsp_status =
                        LspStatus::Failed(crate::lsp::LspFailure::Stopped { server, detail });
                }
            }
        }
    }

    pub(super) fn lsp_server_name(&self) -> String {
        self.lsp.as_ref().map_or_else(
            || "Language server".to_string(),
            |c| c.server_name().to_string(),
        )
    }

    /// Push the active buffer's LSP diagnostics into the editor's inline model
    /// and merge them with the local linter for the Problems panel.
    pub(super) fn apply_diagnostics_to_editor(&mut self) {
        let uri = self.lsp_uri.clone();
        let lsp_diags = uri
            .as_ref()
            .and_then(|u| self.lsp_diagnostics.get(u))
            .cloned()
            .unwrap_or_default();
        self.editor.set_diagnostics(&lsp_diags);
    }

    /// Jump to a definition result.
    ///
    /// A same-file result just moves the caret. A result in another file — the
    /// common case once the server is rooted at a real workspace, since most
    /// definitions live in another module or another crate — opens that file
    /// first. Without this, go-to-definition silently does nothing for exactly
    /// the jumps people use it for.
    pub(super) fn apply_definition(&mut self, locs: Vec<crate::lsp::Location>) {
        let Some(loc) = locs.into_iter().next() else {
            return;
        };
        let (line, col) = (
            loc.range.start.line as usize,
            loc.range.start.character as usize,
        );
        if self.lsp_uri.as_deref() == Some(loc.uri.as_str()) {
            self.editor.move_caret_to(line, col);
            return;
        }
        // Another file: resolve the URI back to a path through the same strict
        // mapping the portal uses, open it, and land the caret there.
        let Ok(url) = url::Url::parse(&loc.uri) else {
            return;
        };
        let Some(path) = ui::portal::to_local_path(&url) else {
            self.note(
                format!("Definition is outside this machine: {}", loc.uri),
                true,
            );
            return;
        };
        let _ = self.open_disk_file(&path);
        self.pending_jump = Some((line, col));
    }

    /// The LSP position of the editor's primary caret.
    pub(super) fn caret_lsp_pos(&self) -> crate::lsp::Position {
        let (line, col) = self.editor.primary_caret();
        crate::lsp::Position {
            line: line as u32,
            character: col as u32,
        }
    }

    /// Issue the language-server request the editor asked for, at the caret.
    /// A no-op when no server is live (opt-in).
    pub(super) fn dispatch_lsp_ask(&mut self, ask: LspAsk) {
        let Some(uri) = self.lsp_uri.clone() else {
            return;
        };
        let pos = self.caret_lsp_pos();
        if let Some(c) = self.lsp.as_mut() {
            match ask {
                LspAsk::Completion => c.request_completion(&uri, pos),
                LspAsk::Hover => c.request_hover(&uri, pos),
                LspAsk::Definition => c.request_definition(&uri, pos),
            }
        }
    }
}

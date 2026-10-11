//! The notes app's answers to [`NotesEffect`]: moving between notes, files
//! through the host's chooser, links and the clipboard.

use ui::framework::{EffectAnswer, EffectCx, EffectStatus, Effected};
use ui::portal::{FileFilter, PickRequest};

use super::NotesApp;
use crate::actions;
use crate::effect::NotesEffect;
use crate::model::{PageId, TagId, Workspace};
use crate::resource::Notebook;

type Cx = EffectCx<Notebook, NotesApp>;

impl NotesApp {
    /// Answer `effect`, however it was invoked.
    pub(crate) fn answer_effect(&mut self, effect: NotesEffect, cx: &mut Cx) -> EffectAnswer {
        if effect == NotesEffect::InspectEditor {
            let document = cx.document().ok_or("the editor document is unavailable")?;
            return Ok(Effected::value(self.editor_observation(&document)));
        }
        if let NotesEffect::FindText { text, page } = &effect {
            let document = cx.document().ok_or("the notebook is unavailable")?;
            return Ok(Effected::value(actions::find_text(&document, text, *page)?));
        }
        let state = cx.live().get();
        match effect {
            NotesEffect::InspectEditor | NotesEffect::FindText { .. } => {
                Err("the editor document is unavailable".into())
            }
            NotesEffect::FindPage { title } => {
                Ok(Effected::value(actions::find_pages(state.pages(), &title)))
            }
            NotesEffect::ImportMarkdown => Ok(Self::choose_markdown(cx)),
            NotesEffect::ImportMarkdownContent { title, body } => {
                let title = title.trim();
                let title = if title.is_empty() {
                    "Imported note"
                } else {
                    title
                };
                let body = crate::markdown::strip_matching_title_heading(&body, title);
                let page = self.import_note(title, &body, cx)?;
                Ok(Effected::value(serde_json::json!({
                    "resource": actions::page_resource_id(page),
                })))
            }
            NotesEffect::ExportMarkdown { page } => self.export_markdown(&state, page, cx),
            NotesEffect::InsertImage { page } => {
                state.get(page).ok_or("the note no longer exists")?;
                Ok(Self::choose_image(page, cx))
            }
            NotesEffect::OpenLink { url } => Self::open_link(url, cx),
            NotesEffect::CopyLink { page } => {
                self.copy_link(&state, page).map(|()| Effected::done())
            }
            NotesEffect::Back => self.step(ui::NavAction::Back, "no previous note"),
            NotesEffect::Forward => self.step(ui::NavAction::Forward, "no next note"),
            NotesEffect::Open { page, block } => self
                .open_block(&state, page, block)
                .map(|()| Effected::done()),
            NotesEffect::RevealBlockRange {
                page,
                block,
                start,
                end,
                unit,
            } => self
                .open_block_range(&state, page, block, (start, end, &unit))
                .map(|()| Effected::done()),
            NotesEffect::OpenTag { tag } => self.open_tag(&state, tag).map(|()| Effected::done()),
        }
    }

    /// Back and forward, offered only where they lead.
    pub(crate) fn navigation_statuses(&self) -> Vec<EffectStatus> {
        let mut statuses = Vec::new();
        if !self.route_history.can_go_back() {
            statuses.push(EffectStatus::unavailable(
                NotesEffect::BACK,
                "notes.no_previous_note",
                "There is no previous note",
            ));
        }
        if !self.route_history.can_go_forward() {
            statuses.push(EffectStatus::unavailable(
                NotesEffect::FORWARD,
                "notes.no_next_note",
                "There is no next note",
            ));
        }
        statuses
    }

    /// Ask for `effect` as the window's own controls do: through the
    /// notebook's dispatcher, so it is answered like any other invocation.
    pub(super) fn request(&self, effect: &NotesEffect) {
        let Some(dispatcher) = &self.dispatcher else {
            tracing::warn!(?effect, "notes: this notebook is open to view only");
            return;
        };
        match effect {
            NotesEffect::ExportMarkdown { page }
            | NotesEffect::InsertImage { page }
            | NotesEffect::CopyLink { page } => {
                dispatcher.effect_on(&actions::page_resource_id(*page), effect);
            }
            _ => dispatcher.effect(effect),
        }
    }

    /// Show note `page`.
    pub(super) fn open_note(&mut self, page: PageId) -> Result<(), String> {
        self.workspace
            .get(page)
            .ok_or("the note no longer exists")?;
        self.open_page(page);
        Ok(())
    }

    /// Show note `page`, revealing `block` in it when one is named.
    pub(super) fn open_block(
        &mut self,
        state: &Workspace,
        page: PageId,
        block: Option<String>,
    ) -> Result<(), String> {
        if let Some(block) = &block
            && !state
                .bodies
                .get(&page)
                .is_some_and(|blocks| blocks.iter().any(|stored| stored.id == *block))
        {
            return Err("the block no longer exists".into());
        }
        state.get(page).ok_or("the note no longer exists")?;
        self.open_page(page);
        self.pending_block_range = None;
        if let Some(block) = block {
            self.pending_block_reveal = Some((page, block));
        }
        Ok(())
    }

    /// Validate offsets before navigation; select only once the same text is bound.
    pub(super) fn open_block_range(
        &mut self,
        state: &Workspace,
        page: PageId,
        block: String,
        range: (usize, usize, &str),
    ) -> Result<(), String> {
        let (start, end, unit) = range;
        let text = &state
            .bodies
            .get(&page)
            .and_then(|blocks| blocks.iter().find(|stored| stored.id == block))
            .ok_or("the block no longer exists")?
            .text;
        if start > end {
            return Err("backwards block range".into());
        }
        let offset = |at| match unit {
            "Bytes" => (at <= text.len() && text.is_char_boundary(at)).then_some(at),
            "Chars" => text
                .char_indices()
                .map(|(byte, _)| byte)
                .chain(std::iter::once(text.len()))
                .nth(at),
            _ => None,
        };
        let start = offset(start).ok_or("block range unit or start is invalid")?;
        let end = offset(end).ok_or("block range unit or end is invalid")?;
        self.open_block(state, page, Some(block.clone()))?;
        self.pending_block_range = Some((page, block, text.clone(), start, end));
        Ok(())
    }

    /// Browse the notes tagged `tag`, keeping the note shown when it is one
    /// of them and showing the first of them otherwise.
    pub(super) fn open_tag(&mut self, state: &Workspace, tag: TagId) -> Result<(), String> {
        state.get_tag(tag).ok_or("the tag no longer exists")?;
        self.active_tag_filter = Some(tag);
        let pages = state.pages_for_tag(tag);
        if let Some(first) = pages.first().copied()
            && !self
                .workspace
                .selected()
                .is_some_and(|page| pages.contains(&page))
        {
            self.navigate_to(first);
        }
        self.self_dirty = true;
        Ok(())
    }

    fn step(&mut self, action: ui::NavAction, nowhere: &str) -> EffectAnswer {
        if self.navigate_history(action) {
            Ok(Effected::done())
        } else {
            Err(nowhere.into())
        }
    }

    fn copy_link(&mut self, state: &Workspace, page: PageId) -> Result<(), String> {
        let title = state
            .get(page)
            .ok_or("the note no longer exists")?
            .title
            .trim();
        let label = if title.is_empty() { "Untitled" } else { title };
        if !ui::try_clipboard_copy(&format!("[[{label}]]")) {
            return Err("the clipboard is unavailable".into());
        }
        self.transfer_toasts.push(
            format!("Copied link to {label}"),
            ui::widget::toast::ToastKind::Success,
        );
        Ok(())
    }

    fn open_link(url: String, cx: &mut Cx) -> EffectAnswer {
        if !crate::markdown::safe_external_url(&url) {
            return Err("This link is not safe to open".into());
        }
        Ok(cx.thread(
            move |_| ui::widget::try_open_external_url(&url).map_err(|error| error.to_string()),
            |app: &mut Self, opened, _| {
                if let Err(error) = &opened {
                    tracing::warn!(%error, "notes: the link did not open");
                    app.transfer_toasts
                        .push("Could not open link", ui::widget::toast::ToastKind::Error);
                }
                opened.map(|()| Effected::done())
            },
        ))
    }

    fn choose_markdown(cx: &mut Cx) -> Effected {
        let request = PickRequest::open("Import Markdown", "Import")
            .filter(FileFilter::new("Markdown").glob("*.md"))
            .thread_name("notes-import-markdown");
        cx.choose_file(request, |app: &mut Self, chosen, cx| {
            let Some(file) = chosen else {
                return Ok(Effected::done());
            };
            let markdown = String::from_utf8(file.bytes)
                .map_err(|_| "The Markdown file is not UTF-8".to_string());
            let title = std::path::Path::new(&file.name)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|title| !title.trim().is_empty())
                .unwrap_or("Imported note")
                .to_owned();
            match markdown {
                Ok(markdown) => app
                    .import_note(&title, &markdown, cx)
                    .map(|_| Effected::done()),
                Err(error) => {
                    app.import_failed(&error);
                    Err(error)
                }
            }
        })
    }

    /// Add `markdown` to the notebook as a note titled `title`, and show it.
    fn import_note(&mut self, title: &str, markdown: &str, cx: &Cx) -> Result<PageId, String> {
        if let Err(error) = actions::check_markdown_import(markdown) {
            self.import_failed(&error);
            return Err(error);
        }
        let (markdown, assets) = super::adopt_inline_images(markdown);
        let page = PageId::new_v4();
        cx.apply(&actions::import_markdown_action(
            page, title, &markdown, assets,
        ));
        self.open_when_present(page);
        self.transfer_toasts.push(
            format!("Imported {title}"),
            ui::widget::toast::ToastKind::Success,
        );
        Ok(page)
    }

    fn import_failed(&mut self, error: &str) {
        self.transfer_toasts.push(
            format!("Import failed: {error}"),
            ui::widget::toast::ToastKind::Error,
        );
    }

    fn export_markdown(&self, state: &Workspace, page: PageId, cx: &mut Cx) -> EffectAnswer {
        let title = state
            .get(page)
            .ok_or("the note no longer exists")?
            .title
            .trim();
        let markdown = actions::page_markdown(state, page);
        let assets = self
            .canonical_assets
            .read()
            .map(|assets| assets.clone())
            .unwrap_or_default();
        let portable = super::inline_images(&markdown, &assets);
        let request = PickRequest::save("Export Markdown", "Export", &export_name(title))
            .filter(FileFilter::new("Markdown").glob("*.md"))
            .thread_name("notes-export-markdown");
        Ok(cx.save_file(
            request,
            portable.into_bytes(),
            |app: &mut Self, saved, _| {
                if saved {
                    app.transfer_toasts
                        .push("Note exported", ui::widget::toast::ToastKind::Success);
                }
                Ok(Effected::done())
            },
        ))
    }

    fn choose_image(page: PageId, cx: &mut Cx) -> Effected {
        let request = PickRequest::open("Insert image", "Insert")
            .filter(
                FileFilter::new("Images")
                    .glob("*.png")
                    .glob("*.jpg")
                    .glob("*.jpeg")
                    .glob("*.webp"),
            )
            .thread_name("notes-insert-image");
        cx.choose_file(request, move |app: &mut Self, chosen, cx| {
            let Some(file) = chosen else {
                return Ok(Effected::done());
            };
            if let Err(error) = super::check_image(&file.bytes) {
                app.transfer_toasts.push(
                    format!("Image import failed: {error}"),
                    ui::widget::toast::ToastKind::Error,
                );
                return Err(error);
            }
            let alt = std::path::Path::new(&file.name)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("Image")
                .replace([']', '[', '(', ')'], " ");
            cx.apply(&app.image_insertion(page, alt.trim(), file.bytes));
            app.bound_page = None;
            app.self_dirty = true;
            app.transfer_toasts
                .push("Image inserted", ui::widget::toast::ToastKind::Success);
            Ok(Effected::done())
        })
    }

    /// The action adding image `bytes` to `page`, after the block the
    /// caret is in when the note is the one shown, else at its end.
    pub(super) fn image_insertion(
        &self,
        page: PageId,
        alt: &str,
        bytes: Vec<u8>,
    ) -> actions::NotesAction {
        let digest = blake3::hash(&bytes).to_hex().to_string();
        let after = (self.main.page_id() == Some(page))
            .then(|| {
                let editor = self.main.editor();
                editor
                    .focused_index()
                    .and_then(|index| editor.blocks().get(index))
                    .or_else(|| editor.blocks().last())
                    .map(|block| block.id.clone())
            })
            .flatten();
        let text = format!("![{alt}](asset:{digest})");
        let edit = match after {
            Some(after) => tilo_document::RichTextAction::InsertParagraph { after, text },
            None => tilo_document::RichTextAction::AppendParagraph { text },
        };
        actions::add_image_asset_action(page, bytes, edit)
    }
}

/// The file name a note titled `title` is offered to be saved as.
fn export_name(title: &str) -> String {
    let safe = title
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    let stem = safe.trim_matches('-').to_lowercase();
    if stem.is_empty() {
        "note.md".to_owned()
    } else {
        format!("{stem}.md")
    }
}

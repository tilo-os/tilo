//! The notes app's answers to [`NotesEffect`]: moving between notes, files
//! through the host's chooser, links and the clipboard.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{EffectAnswer, EffectCx, EffectStatus, Effected};
use ui::portal::{FileFilter, PickRequest};
use ui::widget::toast::ToastKind;

use super::NotesApp;
use crate::actions;
use crate::effect::NotesEffect;
use crate::model::{PageId, TagId, Workspace};
use crate::resource::Notebook;

type Cx = EffectCx<Notebook>;

/// What an effect's completion leaves for the window: a completion runs
/// after the call that was lent the window (a file chosen, a link
/// opened), so the window does it on its next sync.
#[derive(Clone, Default)]
pub(crate) struct Completions(Rc<RefCell<Vec<Completed>>>);

/// One thing a completion left for the window.
pub(crate) enum Completed {
    /// Say this in the window's bar.
    Notify(String, ToastKind),
    /// Open the note once the notebook holds it.
    Open(PageId),
    /// Bind the open note's editor again: its blocks changed under it.
    Rebind,
}

impl Completions {
    fn push(&self, completed: Completed) {
        self.0.borrow_mut().push(completed);
    }

    fn extend(&self, completed: impl IntoIterator<Item = Completed>) {
        self.0.borrow_mut().extend(completed);
    }

    /// What was left since the last sync.
    pub(crate) fn take(&self) -> Vec<Completed> {
        std::mem::take(&mut self.0.borrow_mut())
    }
}

/// Back and forward, offered only where they lead: `steps` says whether
/// each does.
pub(crate) fn navigation_statuses((back, forward): (bool, bool)) -> Vec<EffectStatus> {
    let mut statuses = Vec::new();
    if !back {
        statuses.push(EffectStatus::unavailable(
            NotesEffect::BACK,
            "notes.no_previous_note",
            ui::i18n!("notes-no-previous"),
        ));
    }
    if !forward {
        statuses.push(EffectStatus::unavailable(
            NotesEffect::FORWARD,
            "notes.no_next_note",
            ui::i18n!("notes-no-next"),
        ));
    }
    statuses
}

impl NotesApp {
    /// Answer `effect`, however it was invoked.
    pub(crate) fn answer_effect(&mut self, effect: NotesEffect, cx: &mut Cx) -> EffectAnswer {
        let answer = self.answer(effect, cx);
        self.note_steps();
        answer
    }

    fn answer(&mut self, effect: NotesEffect, cx: &mut Cx) -> EffectAnswer {
        if effect == NotesEffect::InspectEditor {
            let document = cx.document().ok_or("the editor document is unavailable")?;
            return Ok(Effected::value(self.editor_observation(&document)));
        }
        if let NotesEffect::FindText { text, page } = &effect {
            let document = cx.document().ok_or("the notebook is unavailable")?;
            return Ok(Effected::value(actions::find_text(&document, text, *page)?));
        }
        let state = Workspace::of(&cx.live().get(), &self.workspace);
        match effect {
            NotesEffect::InspectEditor | NotesEffect::FindText { .. } => {
                Err("the editor document is unavailable".into())
            }
            NotesEffect::FindPage { title } => Ok(Effected::value(actions::find_pages(
                &state.data.pages,
                &title,
            ))),
            NotesEffect::ImportMarkdownFile => Ok(self.choose_markdown(cx)),
            NotesEffect::ImportNotionExport => Ok(self.choose_notion_export(cx)),
            NotesEffect::ImportMarkdownContent { title, body } => {
                let title = title.trim();
                let fallback = ui::i18n!("notes-imported-untitled");
                let title = if title.is_empty() { &fallback } else { title };
                let body = crate::markdown::strip_matching_title_heading(&body, title);
                let mut done = Vec::new();
                let page = import_note(title, &body, cx, &mut done);
                self.complete(done);
                let page = page?;
                Ok(Effected::value(serde_json::json!({
                    "resource": actions::page_resource_id(page),
                })))
            }
            NotesEffect::ExportMarkdown { page } => self.export_markdown(&state, page, cx),
            NotesEffect::ExportMarkdownContent { page, title } => self
                .export_markdown_content(&state, page, title.as_deref())
                .map(Effected::value),
            NotesEffect::InsertImage { page } => {
                state.get(page).ok_or("the note no longer exists")?;
                Ok(self.choose_image(page, cx))
            }
            NotesEffect::OpenLink { url } => self.open_link(url, cx),
            NotesEffect::OpenResource { href } => Self::open_resource(&href, cx),
            NotesEffect::CopyLink { page } => {
                self.copy_link(&state, page).map(|()| Effected::done())
            }
            NotesEffect::Back => self.step(ui::NavAction::Back, "no previous note"),
            NotesEffect::Forward => self.step(ui::NavAction::Forward, "no next note"),
            NotesEffect::OpenNote { page, block } => self
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

    /// Say whether Back and Forward lead anywhere now, for the effects'
    /// statuses.
    pub(super) fn note_steps(&self) {
        self.steps.set((
            self.route_history.can_go_back(),
            self.route_history.can_go_forward(),
        ));
    }

    /// Do what effects' completions left for the window.
    pub(super) fn complete(&mut self, completed: Vec<Completed>) {
        for completed in completed {
            match completed {
                Completed::Notify(text, kind) => self.notify(text, kind),
                Completed::Open(page) => self.open_when_present(page),
                Completed::Rebind => {
                    self.bound_page = None;
                    self.self_dirty = true;
                }
            }
        }
    }

    /// Ask for `effect` as the window's own controls do: through the
    /// notebook's dispatcher, so it is answered like any other invocation.
    pub(super) fn request(&self, effect: &NotesEffect) {
        let Some(dispatcher) = &self.dispatcher else {
            tracing::warn!(?effect, "notes: this notebook is open to view only");
            return;
        };
        // What acts on one note is offered on that note.
        match effect {
            NotesEffect::CopyLink { page }
            | NotesEffect::ExportMarkdown { page }
            | NotesEffect::InsertImage { page } => {
                dispatcher.effect_on(&crate::actions::page_resource_id(*page), effect);
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
        if !self
            .link_copy
            .watch(ui::try_clipboard_copy(&format!("[[{label}]]")))
        {
            return Err("the clipboard is unavailable".into());
        }
        self.notify(
            ui::i18n::I18nManager::format(
                "notes-link-copied",
                &[("title", ui::i18n::ArgValue::from(label.to_string()))],
            ),
            ui::widget::toast::ToastKind::Success,
        );
        Ok(())
    }

    fn open_link(&self, url: String, cx: &mut Cx) -> EffectAnswer {
        if !crate::markdown::safe_external_url(&url) {
            return Err(ui::i18n!("notes-link-unsafe"));
        }
        let completions = self.completions.clone();
        Ok(cx.thread(
            move |_| ui::widget::try_open_external_url(&url).map_err(|error| error.to_string()),
            move |(), opened, _| {
                if let Err(error) = &opened {
                    tracing::warn!(%error, "notes: the link did not open");
                    completions.push(Completed::Notify(
                        ui::i18n!("notes-link-open-failed"),
                        ToastKind::Error,
                    ));
                }
                opened.map(|()| Effected::done())
            },
        ))
    }

    /// Open what a link in a note names, through the address it carries: the
    /// app that keeps that resource shows it, started first when no window
    /// does. The same door a chip uses, so a link written by hand and one an
    /// app put in a note land in the same place.
    fn open_resource(href: &str, cx: &Cx) -> EffectAnswer {
        if actions::resource_address_in(href).is_none() {
            return Err(ui::i18n!("notes-resource-link-invalid"));
        }
        cx.dispatcher().open_link(href).map_err(|reason| {
            tracing::warn!(%reason, "notes: the linked resource could not be opened");
            ui::i18n!("notes-resource-link-open-failed")
        })?;
        Ok(Effected::done())
    }

    /// Add every page of the Notion export the person chooses, as one
    /// action: one undo takes the whole import back.
    fn choose_notion_export(&self, cx: &mut Cx) -> Effected {
        let request = PickRequest::open(
            &ui::i18n!("notes-notion-import-title"),
            &ui::i18n!("notes-import-accept"),
        )
        .filter(FileFilter::new(&ui::i18n!("notes-notion-files")).glob("*.zip"))
        .filter(FileFilter::new(&ui::i18n!("notes-notion-json-files")).glob("*.json"))
        .thread_name("notes-import-notion");
        let completions = self.completions.clone();
        cx.choose_file(request, move |(), chosen, cx| {
            let Some(file) = chosen else {
                return Ok(Effected::done());
            };
            let pages = crate::notion::pages(&file.bytes)?;
            let imported = pages.len();
            cx.submit(crate::notion::import_action(&pages));
            completions.push(Completed::Notify(
                ui::i18n::I18nManager::format(
                    "notes-notion-imported",
                    &[("pages", ui::i18n::ArgValue::from(imported.to_string()))],
                ),
                ToastKind::Success,
            ));
            Ok(Effected::done())
        })
    }

    fn choose_markdown(&self, cx: &mut Cx) -> Effected {
        let request = PickRequest::open(
            &ui::i18n!("notes-import-title"),
            &ui::i18n!("notes-import-accept"),
        )
        .filter(FileFilter::new(&ui::i18n!("notes-markdown-files")).glob("*.md"))
        .thread_name("notes-import-markdown");
        let completions = self.completions.clone();
        cx.choose_file(request, move |(), chosen, cx| {
            let Some(file) = chosen else {
                return Ok(Effected::done());
            };
            let markdown =
                String::from_utf8(file.bytes).map_err(|_| ui::i18n!("notes-import-not-utf8"));
            let title = std::path::Path::new(&file.name)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|title| !title.trim().is_empty())
                .map_or_else(|| ui::i18n!("notes-imported-untitled"), str::to_owned);
            let mut done = Vec::new();
            let imported = match markdown {
                Ok(markdown) => import_note(&title, &markdown, cx, &mut done),
                Err(error) => {
                    done.push(import_failed(&error));
                    Err(error)
                }
            };
            completions.extend(done);
            imported.map(|_| Effected::done())
        })
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
        let request = PickRequest::save(
            &ui::i18n!("notes-export-title"),
            &ui::i18n!("notes-export-accept"),
            &export_name(title),
        )
        .filter(FileFilter::new(&ui::i18n!("notes-markdown-files")).glob("*.md"))
        .thread_name("notes-export-markdown");
        Ok(cx.save_file(request, portable.into_bytes(), |(), _, _| {
            Ok(Effected::done())
        }))
    }

    /// A note, or every note in notebook order, as one portable Markdown
    /// file kept in the window's content store: its address, title, how
    /// many notes it holds and its size.
    fn export_markdown_content(
        &self,
        state: &Workspace,
        page: Option<PageId>,
        title: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let chosen = match (page, title.map(str::trim).filter(|title| !title.is_empty())) {
            (Some(page), _) => Some(state.get(page).ok_or("the note no longer exists")?.id),
            (None, Some(title)) => Some(actions::page_titled(&state.data.pages, title)?),
            (None, None) => None,
        };
        let (name, markdown, notes) = match chosen {
            Some(page) => {
                let title = state.get(page).map_or("", |meta| meta.title.trim());
                let body = actions::page_markdown(state, page);
                // The file starts with the note's title, as the notebook's
                // sections do, unless its body already opens with it.
                let titled = if title.is_empty()
                    || body.trim_start().lines().next() == Some(format!("# {title}").as_str())
                {
                    body
                } else if body.trim().is_empty() {
                    format!("# {title}\n")
                } else {
                    format!("# {title}\n\n{body}")
                };
                (title.to_owned(), titled, 1)
            }
            None => (
                ui::i18n!("notes-title"),
                actions::notebook_markdown(state),
                state.data.pages.len(),
            ),
        };
        let assets = self
            .canonical_assets
            .read()
            .map(|assets| assets.clone())
            .unwrap_or_default();
        let portable = super::inline_images(&markdown, &assets);
        let store = ui::tilo_resource::content::current()
            .ok_or("this window keeps no content to export into")?;
        let hash = store
            .put(portable.as_bytes())
            .map_err(|error| format!("the export could not be kept: {error}"))?;
        let content =
            tilo_actions::content_resource(&hash).ok_or("the export has no content address")?;
        Ok(serde_json::json!({
            "content": content,
            "name": export_name(&name),
            "title": name,
            "notes": notes,
            "bytes": portable.len(),
            "format": "markdown",
        }))
    }

    fn choose_image(&self, page: PageId, cx: &mut Cx) -> Effected {
        let request = PickRequest::open(
            &ui::i18n!("notes-insert-image-title"),
            &ui::i18n!("notes-insert-image-accept"),
        )
        .filter(
            FileFilter::new(&ui::i18n!("notes-image-files"))
                .glob("*.png")
                .glob("*.jpg")
                .glob("*.jpeg")
                .glob("*.webp"),
        )
        .thread_name("notes-insert-image");
        // The picture goes where the caret is as it is asked for.
        let after = self.image_anchor(page);
        let completions = self.completions.clone();
        cx.choose_file(request, move |(), chosen, cx| {
            let Some(file) = chosen else {
                return Ok(Effected::done());
            };
            if let Err(error) = super::check_image(&file.bytes) {
                completions.push(Completed::Notify(
                    format!("Image import failed: {error}"),
                    ToastKind::Error,
                ));
                return Err(error);
            }
            let alt = std::path::Path::new(&file.name)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map_or_else(|| ui::i18n!("notes-image-default-alt"), str::to_owned)
                .replace([']', '[', '(', ')'], " ");
            cx.submit(image_insertion(page, after, alt.trim(), file.bytes));
            completions.extend([
                Completed::Rebind,
                Completed::Notify(ui::i18n!("notes-image-inserted"), ToastKind::Success),
            ]);
            Ok(Effected::done())
        })
    }

    /// The block a picture added to `page` goes after: the one the caret
    /// is in when the note is the one shown, else its last; `None` puts it
    /// at the end of a note not shown.
    pub(super) fn image_anchor(&self, page: PageId) -> Option<String> {
        (self.main.page_id() == Some(page))
            .then(|| {
                let editor = self.main.editor();
                editor
                    .focused_index()
                    .and_then(|index| editor.blocks().get(index))
                    .or_else(|| editor.blocks().last())
                    .map(|block| block.id.clone())
            })
            .flatten()
    }
}

/// The change adding image `bytes` to `page`, after block `after`, else at
/// its end ([`NotesApp::image_anchor`]).
pub(super) fn image_insertion(
    page: PageId,
    after: Option<String>,
    alt: &str,
    bytes: Vec<u8>,
) -> ui::tilo_resource::Invoke<crate::notebook::Notebook> {
    let digest = blake3::hash(&bytes).to_hex().to_string();
    let text = format!("![{alt}](asset:{digest})");
    let edit = match after {
        Some(after) => tilo_document::RichTextAction::InsertParagraph { after, text },
        None => tilo_document::RichTextAction::AppendParagraph { text },
    };
    actions::show_image(page, bytes, edit)
}

/// Add `markdown` to the notebook as a note titled `title`, leaving in
/// `done` that the window shows it and says so, or that it was refused.
fn import_note(
    title: &str,
    markdown: &str,
    cx: &Cx,
    done: &mut Vec<Completed>,
) -> Result<PageId, String> {
    if let Err(error) = crate::notebook::check_markdown_import(markdown) {
        done.push(import_failed(&error));
        return Err(error);
    }
    let (markdown, assets) = super::adopt_inline_images(markdown);
    let page = PageId::new_v4();
    cx.submit(
        Notebook::IMPORT_MARKDOWN.with(crate::notebook::notebook::ImportMarkdown {
            title: title.to_owned(),
            markdown,
            images: assets.into_iter().map(|(_, bytes)| bytes).collect(),
            note: Some(crate::notebook::page_id(page)),
        }),
    );
    done.push(Completed::Open(page));
    done.push(Completed::Notify(
        ui::i18n::I18nManager::format(
            "notes-imported",
            &[("title", ui::i18n::ArgValue::from(title.to_string()))],
        ),
        ToastKind::Success,
    ));
    Ok(page)
}

/// The window's word that an import failed with `error`.
fn import_failed(error: &str) -> Completed {
    Completed::Notify(
        ui::i18n::I18nManager::format(
            "notes-import-failed",
            &[("error", ui::i18n::ArgValue::from(error.to_string()))],
        ),
        ToastKind::Error,
    )
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

//! Images in the document: inserting them from a file or a chooser, and
//! reading the selected one back.

use crate::i18n::t;
use crate::word_processor::Caret;
use automerge::AutoCommit;
use tilo_doc::snapshot::{AttrVal, BlockView};
use ui::portal;
use ui::widget::toast::ToastKind;

use super::Editor;
use super::input::is_dnd_spool_path;

impl Editor {
    pub(super) fn refresh_images(
        blobs: &mut std::collections::BTreeMap<String, Vec<u8>>,
        image_cache: &mut std::collections::HashMap<String, Option<ui::widget::Image>>,
        content: &tilo_content::ContentStore,
        state: &AutoCommit,
        c: &tilo_doc::Containers,
    ) {
        for hash in tilo_doc::model::blob_hashes(state, c) {
            if blobs.contains_key(&hash) {
                continue;
            }
            if let Some(bytes) = content.read(&hash) {
                // A cached "not decodable yet" result must not outlive the bytes' arrival.
                image_cache.remove(&hash);
                blobs.insert(hash, bytes);
            }
        }
    }

    pub(super) fn insert_image_from_path(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        path: &std::path::Path,
    ) -> bool {
        self.insert_image_edits(state, path)
    }

    fn insert_image_edits(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        path: &std::path::Path,
    ) -> bool {
        let file = tilo_files::file_at(path);
        match file
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|(dir, name)| tilo_files::blocking::read(dir, name))
        {
            Ok(bytes) => {
                if !self.insert_image_bytes(state, &path.display().to_string(), &bytes) {
                    return false;
                }
                if is_dnd_spool_path(path)
                    && let Ok((dir, name)) = &file
                {
                    let _ = tilo_files::blocking::remove(dir, name);
                }
                true
            }
            Err(e) => {
                eprintln!(
                    "word-processor: could not read image {}: {e}",
                    path.display()
                );
                false
            }
        }
    }

    fn insert_image_bytes(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        name: &str,
        bytes: &[u8],
    ) -> bool {
        let hash = match self.inner.content_store().put(bytes) {
            Ok(hash) => hash,
            Err(error) => {
                eprintln!("word-processor: could not keep image {name}: {error}");
                return false;
            }
        };
        if self.inner.has_selection() {
            self.insert_text_edits(state, "");
        }
        let caret = self.inner.caret().clone();
        let before: std::collections::HashSet<String> =
            self.inner.ordered_blocks(state).into_iter().collect();
        let mut image = crate::actions::ImageInput::of(hash, bytes);
        if let Ok(decoded) = ui::widget::Image::from_encoded(bytes) {
            let (width, height) = decoded.intrinsic_size();
            image.width = Some(i64::from(width));
            image.height = Some(i64::from(height));
        }
        self.perform_edits(
            state,
            &crate::actions::insert_image_action(&caret.block, caret.offset, image),
        );
        if let Some(after) = self
            .inner
            .ordered_blocks(state)
            .into_iter()
            .rfind(|id| !before.contains(id))
        {
            self.inner.set_caret(Caret {
                block: after,
                offset: 0,
            });
            self.inner.clear_anchor();
        }
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
        true
    }

    pub(super) fn drain_image_choice(&mut self, state: &mut ui::widget::Edits<'_>) {
        let Some(rx) = self.portal_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_rx = None;
                match outcome {
                    portal::ContentsOutcome::Picked(files) => {
                        if let Some(file) = files.into_iter().next() {
                            self.insert_image_bytes(state, &file.name, &file.bytes);
                        }
                    }
                    portal::ContentsOutcome::Failed(reason) => {
                        eprintln!("word-processor: portal error: {reason}");
                        self.toast
                            .show(t("toast-file-dialog-failed"), ToastKind::Error);
                    }
                    portal::ContentsOutcome::Cancelled => {}
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.portal_rx = None,
        }
    }

    /// Ask the host's file chooser for an image and insert it at the caret. The
    /// answer is the file's contents, not a path, so a browser page works too;
    /// a second request while one is in flight is ignored.
    pub(super) fn request_image_insert(&mut self, _state: &mut ui::widget::Edits<'_>) {
        if self.portal_rx.is_some() {
            return;
        }
        self.portal_rx = Some(
            portal::PickRequest::open("Insert image", "Insert")
                .filter(
                    portal::FileFilter::new("Images")
                        .glob("*.png")
                        .glob("*.jpg")
                        .glob("*.jpeg")
                        .glob("*.gif")
                        .glob("*.webp")
                        .glob("*.bmp")
                        .glob("*.svg"),
                )
                .send_contents(),
        );
    }
}

impl Editor {
    pub(super) fn selected_image_id(&self) -> Option<String> {
        self.blocks
            .iter()
            .find(|b| b.id == self.inner.caret.block && b.kind == "image")
            .map(|b| b.id.clone())
    }

    pub(crate) fn shared_image(&self) -> Option<String> {
        let images: Vec<&BlockView> = self.blocks.iter().filter(|b| b.kind == "image").collect();
        let chosen =
            images
                .iter()
                .find(|b| b.id == self.inner.caret.block)
                .or(match images.as_slice() {
                    [only] => Some(only),
                    _ => None,
                })?;
        match chosen.attrs.get("blob") {
            Some(AttrVal::Str(hash)) if !hash.is_empty() => Some(hash.clone()),
            _ => None,
        }
    }

    pub(super) fn selected_image_dims(&self) -> Option<(f64, f64)> {
        let id = self.selected_image_id()?;
        self.laid
            .iter()
            .find(|l| l.id == id && l.kind == "image")
            .map(|l| (l.width, l.height))
    }
}

//! Pictures pasted or dropped into a document: the host keeps their bytes and answers the address
//! they are at, and the editor writes them in at the caret as one step of the document's history.

use winit::event::WindowEvent;

use ui::widget::EventCtx;

use crate::block_editor::{
    BlockContent, BlockEditor, BlockKind, DocumentPosition, DocumentSelection, IMAGE_WIDTH_RANGE,
    ImagePasteTarget, PictureWrap, picture_of, with_picture,
};

impl BlockEditor {
    /// The picture the clipboard holds, kept by the host: where its bytes are, or nothing when
    /// the host keeps no pictures, which takes the paste as a request instead.
    pub(crate) fn keep_pasted_picture(
        &mut self,
        image: &ui::clipboard::ClipboardImage,
    ) -> Option<Result<ui::pictures::KeptPicture, ui::pictures::PictureRefusal>> {
        let name = pasted_picture_name(image.mime);
        let inserter = self.image_inserter.as_mut()?;
        Some(inserter(PictureIn::Bytes {
            name: &name,
            media: image.mime,
            bytes: &image.bytes,
        }))
    }

    /// Where a picture pasted or dropped into the document goes: the host keeps its bytes and
    /// answers the address they are at ([`ui::pictures::keep_picture`]), and the editor writes
    /// the picture in at the caret, as one step of the document's own history. Unset, the
    /// picture is left for the host as an [`ImagePasteRequest`], which places it itself.
    pub fn set_image_inserter(&mut self, inserter: Option<ImageInserter>) {
        self.image_inserter = inserter;
    }

    /// The view's `image` event, which the editor fires when a picture it wrote into the
    /// document is also to be named there (an app keeping the name a picture came in as, beside
    /// the address its bytes are at).
    pub fn set_image_event(&mut self, event: ui::component::Event) {
        self.image_event = Some(event);
    }

    /// The picture file dropped on the editor, when it is one and the host keeps pictures: the
    /// file's bytes are the host's to read, and the picture goes in at the caret. A file that is
    /// no picture is left for whatever else takes the drop (the app's window may import it), and
    /// one that is a picture the host could not keep says why where the person can see it.
    pub(crate) fn drop_picture(&mut self, path: &std::path::Path) -> bool {
        // A drop names no place, so the editor the pointer rests on takes it — or the one the
        // keyboard is in, where a window that laid it out has not been pointed at yet.
        if !self.rect.contains(self.last_mouse_pos) && !self.is_focused {
            return false;
        }
        // A code fence or a table shows its words as they are written: a picture goes into prose.
        let in_prose = self
            .focused
            .and_then(|index| self.blocks.get(index))
            .is_some_and(|block| {
                !matches!(
                    block.kind,
                    BlockKind::CodeBlock { .. } | BlockKind::Table { .. }
                )
            });
        if !in_prose {
            return false;
        }
        let Some(target) = self.image_paste_target() else {
            return false;
        };
        let kept = self
            .image_inserter
            .as_mut()
            .map(|inserter| inserter(PictureIn::Path(path)));
        match kept {
            // No host keeps pictures, or the file is no picture: another taker may have a use for
            // it.
            None | Some(Err(ui::pictures::PictureRefusal::NotAPicture)) => false,
            Some(Ok(kept)) => self.place_picture(&kept, target),
            Some(Err(refusal)) => {
                ui::control_notice::say_in_window(&refusal.words());
                true
            }
        }
    }

    /// Answer a view's ask to write a picture in at the caret, as a toolbar's Insert image does
    /// after it chose the file: the ask names the address, then a count the view changes every
    /// time it asks (`{address}:1`), so the same picture chosen twice is two pictures. Each ask
    /// is answered once.
    pub fn insert_asked(&mut self, asked: Option<String>) -> bool {
        if asked == self.insert_ask {
            return false;
        }
        self.insert_ask.clone_from(&asked);
        let Some(asked) = asked else {
            return false;
        };
        let address = match asked.rsplit_once(':') {
            Some((address, count))
                if !count.is_empty() && count.chars().all(|c| c.is_ascii_digit()) =>
            {
                address
            }
            _ => asked.as_str(),
        };
        self.insert_picture(address)
    }

    /// Write the picture whose bytes are at `address` in where the caret is: after the block it
    /// is in, or over the selection, as a paste puts one. The address is a content address a view
    /// hands over after the person chose a file (`file.choose`), which keeps a toolbar's Insert
    /// image landing where they are writing rather than at the end of the document.
    pub fn insert_picture(&mut self, address: &str) -> bool {
        if address.trim().is_empty() || self.read_only {
            return false;
        }
        let Some(target) = self.image_paste_target() else {
            return false;
        };
        let kept = ui::pictures::KeptPicture {
            name: "Image".to_owned(),
            address: address.to_owned(),
        };
        self.place_picture(&kept, target)
    }

    /// Write `picture` into the document where `target` says: after the block the caret is in, or
    /// over the selection it was made across. One step of the document's history, as a paste is.
    pub(crate) fn place_picture(
        &mut self,
        picture: &ui::pictures::KeptPicture,
        target: ImagePasteTarget,
    ) -> bool {
        let markdown = format!("![]({})", picture.address);
        let (selection, own_line) = match target {
            ImagePasteTarget::After(id) => {
                let Some(index) = self.blocks.iter().position(|block| block.id == id) else {
                    return false;
                };
                let Some(block) = self.blocks.get(index) else {
                    return false;
                };
                let end = block.text().len();
                let own_line = !block.text().trim().is_empty();
                (
                    DocumentSelection {
                        anchor: DocumentPosition {
                            block: index,
                            byte: end,
                        },
                        cursor: DocumentPosition {
                            block: index,
                            byte: end,
                        },
                    },
                    own_line,
                )
            }
            ImagePasteTarget::Replace {
                blocks,
                start_byte,
                end_byte,
            } => {
                let Some(start) = self
                    .blocks
                    .iter()
                    .position(|held| held.content() == blocks[0])
                else {
                    return false;
                };
                (
                    DocumentSelection {
                        anchor: DocumentPosition {
                            block: start,
                            byte: start_byte,
                        },
                        cursor: DocumentPosition {
                            block: start + blocks.len().saturating_sub(1),
                            byte: end_byte,
                        },
                    },
                    false,
                )
            }
        };
        self.document_selection = Some(selection);
        // A picture at the end of a line of text takes a line of its own: the line it was typed
        // on keeps its words, and the picture stands under them. A selection it was pasted or
        // dropped over becomes the picture.
        let mut blocks = Vec::new();
        if own_line {
            blocks.push(BlockContent::paragraph(
                uuid::Uuid::new_v4().to_string(),
                String::new(),
            ));
        }
        blocks.push(BlockContent::paragraph(
            uuid::Uuid::new_v4().to_string(),
            markdown,
        ));
        let placed = self.replace_document_selection_with_blocks(blocks);
        if placed {
            self.dirty = true;
            // The picture's name and the document's edit are one act of the person's: the
            // app's naming action and the body's change share a gesture, so one undo takes
            // both back.
            let gesture = tilo_actions::ActionId::new();
            if let Some(body) = &mut self.body {
                body.set_gesture(gesture);
            }
            if let Some(event) = &self.image_event {
                event.fire_in_gesture(
                    Some(ui::component::Value::List(vec![
                        ui::component::Value::Text(picture.name.clone()),
                        ui::component::Value::Text(picture.address.clone()),
                    ])),
                    Some(gesture),
                );
            }
        }
        placed
    }
    pub(crate) fn try_paste_image_key(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
    ) -> bool {
        use winit::event::{ElementState, KeyEvent};
        use winit::keyboard::Key;

        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Character(key),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        let mods = ctx.modifiers.state();
        let command = mods.control_key() || mods.super_key();
        if !self.is_focused
            || !command
            || mods.alt_key()
            || !key.eq_ignore_ascii_case("v")
            || !self.paste_image()
        {
            return false;
        }
        *ctx.handled = true;
        true
    }
}

/// A picture the person pasted or dropped into the document: its bytes under the name and media
/// type it came in as, or the file they dropped.
#[derive(Debug, Clone)]
pub enum PictureIn<'a> {
    Bytes {
        name: &'a str,
        media: &'a str,
        bytes: &'a [u8],
    },
    Path(&'a std::path::Path),
}

/// Where a picture the person pasted or dropped goes: the host keeps it and answers where its
/// bytes are, or refuses it with the reason, which leaves the editor's document as it was.
pub type ImageInserter = Box<
    dyn for<'a> FnMut(
        PictureIn<'a>,
    ) -> Result<ui::pictures::KeptPicture, ui::pictures::PictureRefusal>,
>;

/// What a picture copied to the clipboard is called: the name a document writes beside its
/// bytes, from the type it was copied as (`Pasted image.png`).
pub(crate) fn pasted_picture_name(media: &str) -> String {
    let extension = match media {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/svg+xml" => "svg",
        _ => "png",
    };
    format!("Pasted image.{extension}")
}

impl BlockEditor {
    /// A formatting ask that sets the picture the caret is in, as the image inspector's buttons
    /// do: `size:down`, `size:up`, `flip:h`, `flip:v`, `crop:in`, `crop:reset`, `place:inline`,
    /// `place:free`, `wrap:left`, `wrap:right` or `wrap:none`. The picture's Markdown carries the
    /// setting, so it is one recorded edit of the document, and travels with it.
    pub(crate) fn picture_asked(&mut self, op: &str) -> bool {
        if self.read_only {
            return false;
        }
        let Some(index) = self
            .focused
            .or_else(|| self.document_selection.map(|selection| selection.ordered().0.block))
        else {
            return false;
        };
        let Some(block) = self.blocks.get(index) else {
            return false;
        };
        let Some((_, _, width, mut set)) = picture_of(block.text()) else {
            return false;
        };
        let was = set;
        let mut sized = width;
        match op {
            "size:down" | "size:up" => {
                let current = width.map_or(320.0, f64::from);
                let factor = if op == "size:down" { 0.9 } else { 1.1 };
                let next = (current * factor).round();
                let next = next.clamp(
                    f64::from(*IMAGE_WIDTH_RANGE.start()),
                    f64::from(*IMAGE_WIDTH_RANGE.end()),
                );
                sized = Some(ui::num::u32_of_i64(ui::num::i64_of_f64(next)));
            }
            "flip:h" => set.flip_h = !set.flip_h,
            "flip:v" => set.flip_v = !set.flip_v,
            "crop:in" => {
                let (left, top, right, bottom) = set.crop;
                let (left, top) = (left.saturating_add(5), top.saturating_add(5));
                let (right, bottom) = (right.saturating_add(5), bottom.saturating_add(5));
                if u16::from(left) + u16::from(right) > 95
                    || u16::from(top) + u16::from(bottom) > 95
                {
                    return false;
                }
                set.crop = (left, top, right, bottom);
            }
            "crop:reset" => set.crop = (0, 0, 0, 0),
            "place:inline" => set.free = false,
            "place:free" => set.free = true,
            "wrap:left" => set.wrap = Some(PictureWrap::Left),
            "wrap:right" => set.wrap = Some(PictureWrap::Right),
            "wrap:none" => set.wrap = None,
            _ => return false,
        }
        if sized == width && set == was {
            return false;
        }
        let Some(text) = with_picture(block.text(), sized, set) else {
            return false;
        };
        if text == block.text() {
            return false;
        }
        let block = &mut self.blocks[index];
        block.line.set_text(&text);
        if let Some(image) = self.image_attachments.get_mut(&block.id) {
            image.source.clone_from(&text);
        }
        self.edit_pending = true;
        self.commit_pending = true;
        self.structure_dirty = true;
        self.dirty = true;
        true
    }
}

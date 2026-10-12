//! The file desk: carries out what a person asks of Files (open an artwork
//! or an SVG, save the artwork, save a copy, export a picture) through the
//! system's file chooser, and says how it went beside the title.
//!
//! It wraps the window's content and asks nothing of it: the controls set
//! [`Tools::files`], the desk takes the request on its next frame, and
//! what a file brings in lands as one of the artwork's actions.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use ui::tilo_resource::Invoke;

use ui::framework::{Dispatcher, Live, Setting};
use ui::portal::{FileFilter, PickRequest, PortalOutcome};
use ui::prelude::*;
use ui::vello::Scene;
use ui::vello::kurbo::Affine;
use ui::widget::{AppModel, EventCtx, ModelPath, Widget};
use ui::winit::event::WindowEvent;
use ui::{PresenceCtx, TextRenderer, Theme};

use crate::actions::{NewArtwork, OpenArtwork};
use crate::document::{self, ArtworkFile, EXTENSION, Recent, RecentArtworks};
use crate::model::Artwork;
use crate::panels::export::{self, Settings};
use crate::tools::{FileRequest, Sheet, Tools, ViewRequest};

/// What a chooser was opened for.
enum Pending {
    Open,
    /// Save the artwork there and keep working on it there (`true`), or
    /// save a copy and keep working where it is.
    Save {
        follow: bool,
    },
    /// Write these bytes there.
    Export(Vec<u8>),
}

/// The window's content with the file desk round it.
pub struct FileDesk {
    content: Element,
    tools: Tools,
    recents: Setting<RecentArtworks>,
    live: Live<Artwork>,
    dispatch: Dispatcher<Artwork>,
    chooser: Option<(Pending, Receiver<PortalOutcome>)>,
    text: Option<TextRenderer>,
}

impl FileDesk {
    pub fn new(
        content: Element,
        tools: Tools,
        recents: Setting<RecentArtworks>,
        live: Live<Artwork>,
        dispatch: Dispatcher<Artwork>,
    ) -> Self {
        Self {
            content,
            tools,
            recents,
            live,
            dispatch,
            chooser: None,
            text: None,
        }
    }

    /// Say how a request went, in a line short enough to sit beside the
    /// title; the whole of it is what a screen reader hears.
    fn say(&self, id: &str, args: &[(&str, ui::i18n::ArgValue)]) {
        self.tools
            .notice
            .set(Some(ui::i18n::I18nManager::format(id, args)));
    }

    fn name_of(path: &Path) -> String {
        path.file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    }

    /// Carry out what was chosen from the artwork's menu.
    fn take_menu(&self) {
        let chosen = self.tools.menu.get();
        if chosen.is_empty() {
            return;
        }
        self.tools.menu.set(String::new());
        let (sheet, files) = (&self.tools.sheet, &self.tools.files);
        match chosen.as_str() {
            "new" => sheet.set(Some(Sheet::NewArtwork)),
            "open" => files.set(Some(FileRequest::Open)),
            "save" => files.set(Some(FileRequest::Save)),
            "save-as" => files.set(Some(FileRequest::SaveAs)),
            "duplicate" => files.set(Some(FileRequest::Duplicate)),
            "export" => sheet.set(Some(Sheet::Export)),
            "keys" => sheet.set(Some(Sheet::Shortcuts)),
            other => {
                if let Some(path) = other.strip_prefix(crate::panels::header::RECENT) {
                    files.set(Some(FileRequest::OpenRecent(path.to_owned())));
                }
            }
        }
    }

    /// Take a request the controls left, if any, and carry it out.
    fn take(&mut self) {
        self.take_menu();
        let Some(request) = self.tools.files.with(Clone::clone) else {
            return;
        };
        self.tools.files.set(None);
        if self.chooser.is_some() {
            // One chooser at a time; the open one answers first.
            return;
        }
        let lookup = ui::i18n::I18nManager::lookup_plain;
        let art = self.live.get();
        match request {
            FileRequest::Open => {
                let request =
                    PickRequest::open(&lookup("canvas-open-title"), &lookup("canvas-command-open"))
                        .filter(
                            FileFilter::new(&lookup("canvas-filter-artwork"))
                                .glob(&format!("*.{EXTENSION}"))
                                .mimetype(document::MEDIA_TYPE),
                        )
                        .filter(
                            FileFilter::new(&lookup("canvas-filter-svg"))
                                .glob("*.svg")
                                .mimetype("image/svg+xml"),
                        )
                        .thread_name("canvas-open");
                self.chooser = Some((Pending::Open, request.send()));
            }
            FileRequest::OpenRecent(path) => self.open(&PathBuf::from(path)),
            FileRequest::Save => match self.tools.file.get() {
                Some(path) => self.save_to(&PathBuf::from(path), true),
                None => self.choose_save(&art, true),
            },
            FileRequest::SaveAs => self.choose_save(&art, true),
            FileRequest::Duplicate => self.choose_save(&art, false),
            FileRequest::Export(settings) => self.export(&art, settings),
        }
    }

    fn choose_save(&mut self, art: &Artwork, follow: bool) {
        let lookup = ui::i18n::I18nManager::lookup_plain;
        let title = if follow {
            art.title.clone()
        } else {
            ui::i18n::I18nManager::format(
                "canvas-copy-of",
                &[(
                    "title",
                    if art.title.is_empty() {
                        lookup("canvas-untitled")
                    } else {
                        art.title.clone()
                    }
                    .into(),
                )],
            )
        };
        let heading = if follow {
            "canvas-save-title"
        } else {
            "canvas-duplicate-title"
        };
        let request = PickRequest::save(
            &lookup(heading),
            &lookup("canvas-save"),
            &document::file_name(&title, EXTENSION),
        )
        .filter(
            FileFilter::new(&lookup("canvas-filter-artwork"))
                .glob(&format!("*.{EXTENSION}"))
                .mimetype(document::MEDIA_TYPE),
        )
        .thread_name("canvas-save");
        self.chooser = Some((Pending::Save { follow }, request.send()));
    }

    fn export(&mut self, art: &Artwork, settings: Settings) {
        let lookup = ui::i18n::I18nManager::lookup_plain;
        let view = self.tools.visible.get();
        let selection = self.tools.selection.get();
        let bytes =
            export::part(art, settings.area, view, &selection).and_then(|(part, region)| {
                let text = self.text.get_or_insert_with(TextRenderer::new);
                let mut renderer = self.tools.renderer.borrow_mut();
                // An export shows every picture: it waits for those still
                // decoding.
                renderer.pictures.patient = true;
                let encoded = export::encode(&mut renderer, text, &part, region, settings);
                renderer.pictures.patient = false;
                encoded
            });
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(why) => {
                self.say("canvas-export-failed", &[("why", why.into())]);
                return;
            }
        };
        let extension = settings.format.extension();
        let request = PickRequest::save(
            &lookup("canvas-export-title"),
            &lookup("canvas-export"),
            &document::file_name(&export_stem(art), extension),
        )
        .filter(
            FileFilter::new(settings.format.label())
                .glob(&format!("*.{extension}"))
                .mimetype(match settings.format {
                    export::Format::Png => "image/png",
                    export::Format::Jpeg => "image/jpeg",
                    export::Format::Webp => "image/webp",
                    export::Format::Svg => "image/svg+xml",
                }),
        )
        .thread_name("canvas-export");
        self.chooser = Some((Pending::Export(bytes), request.send()));
    }

    /// Hear back from an open chooser.
    fn poll(&mut self) {
        let Some((_, channel)) = &self.chooser else {
            return;
        };
        let outcome = match channel.try_recv() {
            Err(TryRecvError::Empty) => return,
            Ok(outcome) => outcome,
            Err(TryRecvError::Disconnected) => {
                PortalOutcome::Failed(ui::i18n::I18nManager::lookup_plain("canvas-chooser-gone"))
            }
        };
        let Some((pending, _)) = self.chooser.take() else {
            return;
        };
        let path = match outcome {
            PortalOutcome::Opened(path) => path,
            PortalOutcome::Cancelled => return,
            PortalOutcome::Failed(why) => {
                self.say("canvas-files-failed", &[("why", why.into())]);
                return;
            }
        };
        match pending {
            Pending::Open => self.open(&path),
            Pending::Save { follow } => {
                let path = with_extension(path, EXTENSION);
                self.save_to(&path, follow);
            }
            Pending::Export(bytes) => match document::write(&path, &bytes) {
                Ok(()) => self.say("canvas-exported", &[("name", Self::name_of(&path).into())]),
                Err(why) => self.say("canvas-export-failed", &[("why", why.into())]),
            },
        }
    }

    /// Open the artwork or SVG at `path` in place of this one.
    fn open(&self, path: &Path) {
        let svg = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"));
        let opened = if svg {
            open_svg(path).map(|actions| (actions, None))
        } else {
            document::read(path).map(|artwork| {
                let action = Artwork::OPEN_ARTWORK.with(OpenArtwork {
                    file: String::from_utf8_lossy(&artwork.to_json()).into_owned(),
                });
                (vec![action], Some(artwork))
            })
        };
        let (actions, file) = match opened {
            Ok(opened) => opened,
            Err(why) => {
                if !path.exists() {
                    let gone = path.display().to_string();
                    self.keep_recents(|recents| recents.forget(&gone));
                }
                self.say("canvas-open-failed", &[("why", why.into())]);
                return;
            }
        };
        self.dispatch.group(|| {
            for action in &actions {
                self.dispatch.submit(action.clone());
            }
        });
        self.tools.selection.set(Vec::new());
        self.tools.layer.set(String::new());
        self.tools.preview.set(None);
        self.tools.welcomed.set(true);
        self.tools.view.set(Some(ViewRequest::Fit));
        // An SVG is brought in, not worked on in place: saving asks where
        // the artwork goes.
        match file {
            Some(file) => {
                self.tools.file.set(Some(path.display().to_string()));
                self.remember(path, &file);
            }
            None => self.tools.file.set(None),
        }
        self.say("canvas-opened", &[("name", Self::name_of(path).into())]);
    }

    /// Save the artwork to `path`; keep working there when `follow`.
    fn save_to(&self, path: &Path, follow: bool) {
        let file = ArtworkFile::of(&self.live.get());
        let packed = document::pack(&file, &crate::content::store())
            .and_then(|(bytes, _missing)| document::write(path, &bytes));
        match packed {
            Ok(()) => {
                if follow {
                    self.tools.file.set(Some(path.display().to_string()));
                }
                self.remember(path, &file);
                let id = if follow {
                    "canvas-saved"
                } else {
                    "canvas-duplicated"
                };
                self.say(id, &[("name", Self::name_of(path).into())]);
            }
            Err(why) => self.say("canvas-save-failed", &[("why", why.into())]),
        }
    }

    fn remember(&self, path: &Path, file: &ArtworkFile) {
        let at_ms = ui::time::unix_now_ms();
        let recent = Recent {
            path: path.display().to_string(),
            title: file.title.clone(),
            width: file.width,
            height: file.height,
            at_ms,
            thumbnail: None,
        };
        self.keep_recents(|recents| recents.remember(recent));
    }

    /// Change the recent artworks. Best effort: a device that keeps no
    /// settings for the app simply shows no recent artworks.
    fn keep_recents(&self, change: impl FnOnce(&mut RecentArtworks)) {
        if let Err(error) = self.recents.update(change) {
            tracing::warn!(%error, "could not remember the recent artworks");
        }
    }
}

/// What an export is named: the artwork's title, or for an artwork made
/// from a picture (titled by the picture's file) the title marked as
/// edited, so the photo it came from is not the name offered to write over.
fn export_stem(art: &Artwork) -> String {
    let from_picture = !art.title.is_empty()
        && art.marks.iter().any(|mark| {
            matches!(mark.shape, crate::model::Shape::Image { .. }) && mark.name == art.title
        });
    if from_picture {
        ui::i18n::I18nManager::format(
            "canvas-export-edited",
            &[("title", art.title.clone().into())],
        )
    } else {
        art.title.clone()
    }
}

/// The named keys that act on the canvas's selection wherever the focus
/// is, unless what has the focus takes them itself.
fn canvas_key(named: ui::winit::keyboard::NamedKey) -> bool {
    use ui::winit::keyboard::NamedKey;
    matches!(
        named,
        NamedKey::Escape
            | NamedKey::Delete
            | NamedKey::Backspace
            | NamedKey::Enter
            | NamedKey::ArrowLeft
            | NamedKey::ArrowRight
            | NamedKey::ArrowUp
            | NamedKey::ArrowDown
    )
}

/// Carry out a key of the whole window: a tool's letter, the brush's size,
/// `?`, and opening, saving and exporting. Whether it was one.
pub fn window_key(
    tools: &Tools,
    key: &str,
    modifiers: ui::winit::keyboard::ModifiersState,
) -> bool {
    use crate::tools::Tool;
    let command = modifiers.control_key() || modifiers.super_key();
    let shift = modifiers.shift_key();
    if matches!(key, "+" | "=" | "-" | "_") && !modifiers.alt_key() {
        tools.view.set(Some(if matches!(key, "+" | "=") {
            crate::tools::ViewRequest::ZoomIn
        } else {
            crate::tools::ViewRequest::ZoomOut
        }));
        return true;
    }
    if command && matches!(key, "0" | "1") {
        tools.view.set(Some(if key == "0" {
            crate::tools::ViewRequest::Fit
        } else {
            crate::tools::ViewRequest::Actual
        }));
        return true;
    }
    let request = match (key, command, shift) {
        // A keyboard may report the slash with Shift held rather than `?`.
        ("?", false, _) | ("/", false, true) => {
            tools.sheet.set(Some(Sheet::Shortcuts));
            return true;
        }
        ("o", true, false) => FileRequest::Open,
        ("s", true, false) => FileRequest::Save,
        ("s", true, true) => FileRequest::SaveAs,
        ("e", true, true) => {
            tools.sheet.set(Some(Sheet::Export));
            return true;
        }
        (size @ ("[" | "]"), false, _) => return tools.size_key(size),
        (letter, false, false) if !modifiers.alt_key() => {
            let Some(tool) = Tool::for_key(letter) else {
                return false;
            };
            tools.tool.set(tool);
            return true;
        }
        _ => return false,
    };
    tools.files.set(Some(request));
    true
}

/// `path`, ending in `.extension` (a chooser may answer the name as typed).
fn with_extension(path: PathBuf, extension: &str) -> PathBuf {
    if path
        .extension()
        .and_then(|have| have.to_str())
        .is_some_and(|have| have.eq_ignore_ascii_case(extension))
    {
        path
    } else {
        let mut name = path.as_os_str().to_owned();
        name.push(".");
        name.push(extension);
        PathBuf::from(name)
    }
}

/// The SVG at `path` as a new artwork its size, with the SVG drawn on it.
fn open_svg(path: &Path) -> Result<Vec<Invoke<Artwork>>, String> {
    let markup = crate::svg::source(&path.display().to_string())?;
    let tree = usvg::Tree::from_str(&markup, &usvg::Options::default())
        .map_err(|error| format!("not an SVG this can read: {error}"))?;
    let extent = tree.size();
    let side = |value: f32| f64::from(value).round().clamp(16.0, crate::model::MAX_SIDE);
    let title = path
        .file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    let new = Artwork::NEW_ARTWORK.with(NewArtwork {
        title: Some(title),
        width: Some(side(extent.width())),
        height: Some(side(extent.height())),
        start: Some("white".into()),
        ..NewArtwork::default()
    });
    let draw = crate::svg::import(&crate::svg::ImportSvg {
        svg: markup,
        ..crate::svg::ImportSvg::default()
    })?;
    Ok(vec![new, Artwork::DRAW.with(draw)])
}

impl ui::widget::Decorator for FileDesk {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn concrete_type_name(&self) -> &'static str {
        "FileDesk"
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.take();
        self.poll();
        self.content.sync(state, path);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.take();
        self.poll();
        self.content
            .draw(scene, transform, text_renderer, theme, presence);
    }

    /// The window's keys work wherever the focus is (a tool just clicked
    /// in the rail, a panel's button), unless what has it takes the key
    /// itself (a text field typing it).
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.content.handle_event(event, ctx);
        if *ctx.handled {
            return;
        }
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return;
        };
        if key.state != ui::winit::event::ElementState::Pressed {
            return;
        }
        let modifiers = ctx.modifiers.state();
        let typed = match &key.logical_key {
            ui::winit::keyboard::Key::Character(typed) => typed.to_lowercase(),
            ui::winit::keyboard::Key::Named(named) if canvas_key(*named) => {
                // A button or a swatch just clicked keeps the focus; the
                // keys it leaves alone are still the canvas's.
                self.tools.keys.update(|keys| {
                    keys.push(crate::tools::CanvasKey {
                        key: key.logical_key.clone(),
                        text: None,
                        modifiers,
                    });
                });
                return;
            }
            _ => return,
        };
        if window_key(&self.tools, &typed, modifiers) {
            *ctx.handled = true;
        }
    }

    fn needs_redraw(&self) -> bool {
        self.content.needs_redraw()
            || self.tools.files.with(Option::is_some)
            || self.tools.menu.with(|chosen| !chosen.is_empty())
    }

    fn needs_animation_frame(&self) -> bool {
        self.content.needs_animation_frame() || self.chooser.is_some()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test fixtures")]

    use super::*;

    /// A photo opened as an artwork is titled by its file: exporting it
    /// does not offer that file's name to write over.
    #[test]
    fn an_artwork_made_from_a_picture_is_exported_under_a_name_of_its_own() {
        use base64::Engine as _;
        crate::locales::install();
        // The window's content store, which the opened picture is kept in.
        let window = ui::rt::wake::WakeScope::new();
        ui::tilo_resource::content::register(
            &window.handle(),
            tilo_content::ContentStore::memory(),
        );
        let _entered = window.enter();
        let mut doc = ui::tilo_resource::Doc::new();
        let mut png = Vec::new();
        image::RgbaImage::new(10, 10)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let open = Artwork::OPEN_PICTURE.with(crate::actions::Open {
            path: format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&png)
            ),
            title: Some("labrador".into()),
        });
        Artwork::apply_to(&mut doc, open);
        let art = Artwork::from_doc(&doc).unwrap();
        assert_eq!(art.title, "labrador");
        let stem = export_stem(&art);
        assert_eq!(stem, "labrador edited");
        let retitled = Artwork {
            title: "Bruno".into(),
            ..art
        };
        assert_eq!(export_stem(&retitled), "Bruno");
    }
}

//! The file desk: carries out what a person asks of Files (open an artwork
//! or an SVG, save the artwork, save a copy, export a picture) through the
//! system's file chooser, and says how it went beside the title.
//!
//! It wraps the window's content and asks nothing of it: the controls set
//! [`Tools::files`], the desk takes the request on its next frame, and
//! what a file brings in lands as one of the artwork's actions.
//!
//! Every chooser works by contents, so Files works where files have no
//! paths (a browser page): an opened file arrives as its bytes, and a save
//! or export goes to a destination the host keeps when there is no path to
//! write. Where the chosen file has a path, the artwork keeps working there.

use std::path::{Path, PathBuf};
use std::sync::mpsc::TryRecvError;
use ui::tilo_resource::Invoke;

use ui::framework::{Dispatcher, Live, RecordEnv, Setting};
use ui::portal::{
    ContentsChannel, ContentsOutcome, DestinationChannel, DestinationOutcome, FileFilter,
    PickRequest, PickedContents, SaveChannel, SaveOutcome,
};
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

/// A chooser, or a write it led to, that has not answered yet.
enum Pending {
    /// The file to open, read by the host.
    Open(ContentsChannel),
    /// Where to save the artwork: keep working on it there (`follow`), or
    /// save a copy and keep working where it is.
    Save {
        follow: bool,
        channel: DestinationChannel,
    },
    /// The artwork, or a copy, being written where the host keeps it;
    /// `announce` names the place when saying so is news.
    Saving {
        follow: bool,
        announce: Option<String>,
        channel: SaveChannel,
    },
    /// An exported picture being saved under `name`.
    Export { name: String, channel: SaveChannel },
}

/// An export whose pictures are being rendered: the save chooser opens
/// once they are there.
struct Exporting {
    drawn: Drawing,
    settings: Settings,
    stem: String,
}

/// What an export waits for.
enum Drawing {
    Raster(ui::offscreen::pixels::PixelsRequest),
    Svg(Box<export::SvgDrawing>),
}

/// The window's content with the file desk round it.
pub struct FileDesk {
    content: Element,
    tools: Tools,
    recents: Setting<RecentArtworks>,
    live: Live<Artwork>,
    dispatch: Dispatcher<Artwork>,
    /// Where the window opens another of the workspace's artworks.
    env: RecordEnv<Artwork>,
    chooser: Option<Pending>,
    exporting: Option<Exporting>,
    /// An artwork of the workspace asked to open in this window, until the
    /// host answers.
    reopening: Option<ui::application::ReplacementOutcome>,
    text: Option<TextRenderer>,
}

impl FileDesk {
    pub fn new(
        content: Element,
        tools: Tools,
        recents: Setting<RecentArtworks>,
        live: Live<Artwork>,
        env: RecordEnv<Artwork>,
    ) -> Self {
        Self {
            content,
            tools,
            recents,
            live,
            dispatch: env.dispatcher(),
            env,
            chooser: None,
            exporting: None,
            reopening: None,
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
        if self.chooser.is_some() || self.exporting.is_some() {
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
                        .max_contents_bytes(document::MAX_FILE_BYTES);
                self.chooser = Some(Pending::Open(request.send_contents()));
            }
            FileRequest::OpenRecent(path) => self.open(&PathBuf::from(path)),
            FileRequest::OpenWorkspace(uri) => match self.env.open_here(&[uri]) {
                Ok(outcome) => self.reopening = Some(outcome),
                Err(why) => self.say("canvas-open-failed", &[("why", why.into())]),
            },
            FileRequest::Save => match self.tools.file.get() {
                Some(path) => self.save_to(&PathBuf::from(path), true),
                None => match self.tools.kept.with(Clone::clone) {
                    Some(kept) => self.write_to(&kept, true, None),
                    None => self.choose_save(&art, true),
                },
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
        self.chooser = Some(Pending::Save {
            follow,
            channel: request.send_save_destination(),
        });
    }

    fn export(&mut self, art: &Artwork, settings: Settings) {
        let view = self.tools.visible.get();
        let selection = self.tools.selection.get();
        let drawn =
            export::part(art, settings.area, view, &selection).and_then(|(part, region)| {
                let text = self.text.get_or_insert_with(TextRenderer::new);
                let mut renderer = self.tools.renderer.borrow_mut();
                // An export shows every picture: it waits for those still
                // decoding.
                renderer.pictures.patient = true;
                let drawn = export::draw(&mut renderer, text, &part, region, settings);
                renderer.pictures.patient = false;
                drawn
            });
        // Rendered between frames; `finish_export` takes it from there.
        let drawn = match drawn {
            Ok(export::Drawn::Svg(drawing)) => Drawing::Svg(drawing),
            Ok(export::Drawn::Raster(drawing)) => Drawing::Raster(drawing.request()),
            Err(why) => return self.say("canvas-export-failed", &[("why", why.into())]),
        };
        self.exporting = Some(Exporting {
            drawn,
            settings,
            stem: export_stem(art),
        });
        self.finish_export();
    }

    /// Encode the export whose pixels came, and ask where it goes.
    fn finish_export(&mut self) {
        let ready = self
            .exporting
            .as_ref()
            .is_some_and(|exporting| match &exporting.drawn {
                Drawing::Raster(pixels) => !pixels.is_pending(),
                Drawing::Svg(drawing) => drawing.ready(),
            });
        if !ready {
            return;
        }
        let Some(Exporting {
            drawn,
            settings,
            stem,
        }) = self.exporting.take()
        else {
            return;
        };
        let bytes = match drawn {
            Drawing::Svg(drawing) => Ok(drawing.finish().into_bytes()),
            Drawing::Raster(pixels) => {
                crate::view_ops::rgba_of(pixels.take()).and_then(|(rgba, width, height)| {
                    export::encode_raster(&rgba, width, height, settings.format)
                })
            }
        };
        match bytes {
            Ok(bytes) => self.choose_export(&stem, settings, bytes),
            Err(why) => self.say("canvas-export-failed", &[("why", why.into())]),
        }
    }

    /// Ask where the export's `bytes` go.
    fn choose_export(&mut self, stem: &str, settings: Settings, bytes: Vec<u8>) {
        let lookup = ui::i18n::I18nManager::lookup_plain;
        let extension = settings.format.extension();
        let name = document::file_name(stem, extension);
        let request = PickRequest::save(
            &lookup("canvas-export-title"),
            &lookup("canvas-export"),
            &name,
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
        self.chooser = Some(Pending::Export {
            name,
            channel: request.send_save_contents(bytes),
        });
    }

    /// Hear back from an open chooser, or a write it led to.
    fn poll(&mut self) {
        self.finish_export();
        // Once opened, the artwork takes the window with a view of its own;
        // what is left to say is why it did not.
        if let Some(answer) = self
            .reopening
            .as_ref()
            .and_then(ui::application::ReplacementOutcome::get)
        {
            self.reopening = None;
            if let Err(why) = answer {
                self.say("canvas-open-failed", &[("why", why.into())]);
            }
        }
        let Some(pending) = self.chooser.take() else {
            return;
        };
        let gone = || ui::i18n::I18nManager::lookup_plain("canvas-chooser-gone");
        match pending {
            Pending::Open(channel) => match channel.try_recv() {
                Err(TryRecvError::Empty) => self.chooser = Some(Pending::Open(channel)),
                Ok(ContentsOutcome::Picked(files)) => {
                    if let Some(file) = files.into_iter().next() {
                        self.open_chosen(&file);
                    }
                }
                Ok(ContentsOutcome::Cancelled) => {}
                Ok(ContentsOutcome::Failed(why)) => {
                    self.say("canvas-files-failed", &[("why", why.into())]);
                }
                Err(TryRecvError::Disconnected) => {
                    self.say("canvas-files-failed", &[("why", gone().into())]);
                }
            },
            Pending::Save { follow, channel } => match channel.try_recv() {
                Err(TryRecvError::Empty) => self.chooser = Some(Pending::Save { follow, channel }),
                Ok(DestinationOutcome::Chosen(destination)) => {
                    self.save_to_destination(destination, follow);
                }
                Ok(DestinationOutcome::Cancelled) => {}
                Ok(DestinationOutcome::Failed(why)) => {
                    self.say("canvas-files-failed", &[("why", why.into())]);
                }
                Err(TryRecvError::Disconnected) => {
                    self.say("canvas-files-failed", &[("why", gone().into())]);
                }
            },
            Pending::Saving {
                follow,
                announce,
                channel,
            } => match channel.try_recv() {
                Err(TryRecvError::Empty) => {
                    self.chooser = Some(Pending::Saving {
                        follow,
                        announce,
                        channel,
                    });
                }
                Ok(SaveOutcome::Saved) => {
                    // The host keeps it where no path reaches; the artwork
                    // lives there now, and the first save says where.
                    if follow {
                        self.tools.file.set(None);
                    }
                    if let Some(name) = announce {
                        self.say("canvas-duplicated", &[("name", name.into())]);
                    }
                }
                Ok(SaveOutcome::Cancelled) => {}
                Ok(SaveOutcome::Failed(why)) => {
                    self.say("canvas-save-failed", &[("why", why.into())]);
                }
                Err(TryRecvError::Disconnected) => {
                    self.say("canvas-save-failed", &[("why", gone().into())]);
                }
            },
            Pending::Export { name, channel } => match channel.try_recv() {
                Err(TryRecvError::Empty) => self.chooser = Some(Pending::Export { name, channel }),
                Ok(SaveOutcome::Saved) => self.say("canvas-exported", &[("name", name.into())]),
                Ok(SaveOutcome::Cancelled) => {}
                Ok(SaveOutcome::Failed(why)) => {
                    self.say("canvas-export-failed", &[("why", why.into())]);
                }
                Err(TryRecvError::Disconnected) => {
                    self.say("canvas-export-failed", &[("why", gone().into())]);
                }
            },
        }
    }

    /// Save the artwork where the person chose: in place where that has a
    /// path on this device, else through the host, which keeps it.
    fn save_to_destination(&mut self, destination: ui::portal::SaveDestination, follow: bool) {
        if let Some(path) = destination.location() {
            let path = with_extension(path.to_path_buf(), EXTENSION);
            self.save_to(&path, follow);
            return;
        }
        let name = destination.name().to_owned();
        self.write_to(&destination, follow, Some(name));
        if follow {
            // Kept: the next Save writes here without asking.
            self.tools.kept.set(Some(destination));
        }
    }

    /// Write the artwork to `destination`, which has no path here.
    fn write_to(
        &mut self,
        destination: &ui::portal::SaveDestination,
        follow: bool,
        announce: Option<String>,
    ) {
        let file = ArtworkFile::of(&self.live.get());
        match document::pack(&file, &crate::content::store()) {
            Ok((bytes, _missing)) => {
                self.chooser = Some(Pending::Saving {
                    follow,
                    announce,
                    channel: destination.write(bytes),
                });
            }
            Err(why) => self.say("canvas-save-failed", &[("why", why.into())]),
        }
    }

    /// Open the artwork or SVG at `path` in place of this one.
    fn open(&self, path: &Path) {
        let read =
            crate::resource::read_file(&path.display().to_string(), document::MAX_FILE_BYTES);
        match read {
            Ok(bytes) => self.open_bytes(&Self::name_of(path), &bytes, Some(path)),
            Err(why) => {
                if !path.exists() {
                    let gone = path.display().to_string();
                    self.keep_recents(|recents| recents.forget(&gone));
                }
                self.say("canvas-open-failed", &[("why", why.into())]);
            }
        }
    }

    /// Open a file the chooser handed over: worked on where it is when it
    /// has a path here, else brought in, so Save asks where it goes.
    fn open_chosen(&self, file: &PickedContents) {
        self.open_bytes(&file.name, &file.bytes, file.location.as_deref());
    }

    /// Open the artwork or SVG named `name`, whose contents are `bytes`, in
    /// place of this one; `path` is where it is on this device, if anywhere.
    fn open_bytes(&self, name: &str, bytes: &[u8], path: Option<&Path>) {
        let svg = Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"));
        let opened = if svg {
            open_svg(name, bytes).map(|actions| (actions, None))
        } else {
            document::unpack(bytes, &crate::content::store()).map(|artwork| {
                let action = Artwork::OPEN_ARTWORK.with(OpenArtwork {
                    file: String::from_utf8_lossy(&artwork.to_json()).into_owned(),
                });
                (vec![action], Some(artwork))
            })
        };
        let (actions, file) = match opened {
            Ok(opened) => opened,
            Err(why) => {
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
        // An SVG is brought in, not worked on in place, and so is a file
        // with no path here: saving asks where the artwork goes.
        // Wherever the last artwork was saved is not this one's place.
        self.tools.kept.set(None);
        match (file, path) {
            (Some(file), Some(path)) => {
                self.tools.file.set(Some(path.display().to_string()));
                self.remember(path, &file);
            }
            _ => self.tools.file.set(None),
        }
        self.say("canvas-opened", &[("name", name.into())]);
    }

    /// Save the artwork to `path`; keep working there when `follow`.
    fn save_to(&self, path: &Path, follow: bool) {
        let file = ArtworkFile::of(&self.live.get());
        let packed = document::pack(&file, &crate::content::store())
            .and_then(|(bytes, _missing)| document::write(path, &bytes));
        match packed {
            Ok(()) => {
                self.remember(path, &file);
                // Saving where the artwork lives is the system's job and says
                // nothing (the title names the file); a copy went somewhere
                // out of sight, so say where.
                if follow {
                    self.tools.file.set(Some(path.display().to_string()));
                    self.tools.kept.set(None);
                } else {
                    self.say("canvas-duplicated", &[("name", Self::name_of(path).into())]);
                }
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

/// The SVG file `name`, whose contents are `bytes`, as a new artwork its
/// size, with the SVG drawn on it.
fn open_svg(name: &str, bytes: &[u8]) -> Result<Vec<Invoke<Artwork>>, String> {
    if bytes.len() > crate::svg::MAX_SVG_BYTES {
        return Err(format!(
            "the SVG is over {} bytes",
            crate::svg::MAX_SVG_BYTES
        ));
    }
    let markup = String::from_utf8(bytes.to_vec())
        .map_err(|_| "not an SVG this can read: it is not UTF-8 text".to_owned())?;
    let tree = usvg::Tree::from_str(&markup, &usvg::Options::default())
        .map_err(|error| format!("not an SVG this can read: {error}"))?;
    let extent = tree.size();
    let side = |value: f32| f64::from(value).round().clamp(16.0, crate::model::MAX_SIDE);
    let title = Path::new(name)
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
        self.content.needs_animation_frame() || self.chooser.is_some() || self.exporting.is_some()
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    /// A photo opened as an artwork is titled by its file: exporting it
    /// does not offer that file's name to write over.
    #[test]
    fn an_artwork_made_from_a_picture_is_exported_under_a_name_of_its_own() {
        use base64::Engine as _;
        crate::locales::CATALOG.ensure();
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

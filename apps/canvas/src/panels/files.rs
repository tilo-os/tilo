//! The file desk: carries out what a person asks of Files (open an artwork
//! or an SVG, save the artwork, save a copy, export a picture) through the
//! system's file chooser, and says how it went beside the title.
//!
//! It wraps the window's content and asks nothing of it: the controls set
//! [`Tools::files`], the desk takes the request on its next frame, and
//! what a file brings in lands as one of the artwork's actions.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

use ui::framework::{Dispatcher, Live};
use ui::portal::{FileFilter, PickRequest, PortalOutcome};
use ui::prelude::*;
use ui::vello::Scene;
use ui::vello::kurbo::Affine;
use ui::widget::{AppModel, EventCtx, ModelPath, Widget};
use ui::winit::event::WindowEvent;
use ui::{PresenceCtx, TextRenderer, Theme};

use crate::actions::{CanvasAction, NewArtwork, OpenArtwork};
use crate::document::{self, ArtworkFile, EXTENSION, Recent, recents};
use crate::model::Artwork;
use crate::panels::export::{self, Settings};
use crate::resource::ArtworkResource;
use crate::tools::{FileRequest, Tools, ViewRequest};

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
    live: Live<ArtworkResource>,
    dispatch: Dispatcher<ArtworkResource>,
    chooser: Option<(Pending, Receiver<PortalOutcome>)>,
    text: Option<TextRenderer>,
}

impl FileDesk {
    pub fn new(
        content: Element,
        tools: Tools,
        live: Live<ArtworkResource>,
        dispatch: Dispatcher<ArtworkResource>,
    ) -> Self {
        tools.recents.set(recents::read());
        Self {
            content,
            tools,
            live,
            dispatch,
            chooser: None,
            text: None,
        }
    }

    fn say(&self, id: &str, args: &[(&str, ui::i18n::ArgValue)]) {
        self.tools
            .notice
            .set(Some(ui::i18n::I18nManager::format(id, args)));
    }

    fn name_of(path: &Path) -> String {
        path.file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    }

    /// Take a request the controls left, if any, and carry it out.
    fn take(&mut self) {
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
                    PickRequest::open(&lookup("canvas-open-title"), &lookup("canvas-open"))
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
                export::encode(
                    &mut self.tools.renderer.borrow_mut(),
                    text,
                    &part,
                    region,
                    settings,
                )
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
            &document::file_name(&art.title, extension),
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
    fn open(&mut self, path: &Path) {
        let svg = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"));
        let opened = if svg {
            open_svg(path).map(|actions| (actions, None))
        } else {
            document::read(path).map(|artwork| {
                let action = CanvasAction::OpenArtwork(Box::new(OpenArtwork {
                    artwork: artwork.clone(),
                }));
                (vec![action], Some(artwork))
            })
        };
        let (actions, file) = match opened {
            Ok(opened) => opened,
            Err(why) => {
                if !path.exists() {
                    self.tools
                        .recents
                        .set(recents::forget(&path.display().to_string()));
                }
                self.say("canvas-open-failed", &[("why", why.into())]);
                return;
            }
        };
        self.dispatch.group(|| {
            for action in &actions {
                self.dispatch.apply(action);
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
    fn save_to(&mut self, path: &Path, follow: bool) {
        let file = ArtworkFile::of(&self.live.get());
        match document::write(path, &file.to_bytes()) {
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
        let at_ms = ui::time::wall()
            .duration_since(ui::time::SystemTime::UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            });
        self.tools.recents.set(recents::remember(Recent {
            path: path.display().to_string(),
            title: file.title.clone(),
            width: file.width,
            height: file.height,
            at_ms,
            thumbnail: None,
        }));
    }
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
fn open_svg(path: &Path) -> Result<Vec<CanvasAction>, String> {
    let markup = crate::svg::source(&path.display().to_string())?;
    let tree = usvg::Tree::from_str(&markup, &usvg::Options::default())
        .map_err(|error| format!("not an SVG this can read: {error}"))?;
    let size = tree.size();
    let side = |value: f32| f64::from(value).round().clamp(16.0, crate::model::MAX_SIDE);
    let title = path
        .file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    let new = CanvasAction::NewArtwork(NewArtwork {
        title: Some(title),
        width: Some(side(size.width())),
        height: Some(side(size.height())),
        start: Some("white".into()),
        ..NewArtwork::default()
    });
    let draw = crate::svg::import(&crate::svg::ImportSvg {
        svg: markup,
        ..crate::svg::ImportSvg::default()
    })?;
    Ok(vec![new, CanvasAction::Draw(draw)])
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

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.content.handle_event(event, ctx);
    }

    fn needs_redraw(&self) -> bool {
        self.content.needs_redraw() || self.tools.files.with(Option::is_some)
    }

    fn needs_animation_frame(&self) -> bool {
        self.content.needs_animation_frame() || self.chooser.is_some()
    }
}

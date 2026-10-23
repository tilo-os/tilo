//! What the canvas answers views and exports with: from the current artwork
//! or a named version, at once where its pictures render at once, and
//! through the effect's reply once the frame loop rendered them in a
//! browser page.

use serde_json::{Value, json};
use ui::TextRenderer;
use ui::tilo_actions::{self, ActionOutput};

use super::{CanvasSurface, MAX_INLINE_SVG, Reading, saved};
use crate::author::{ColourQuery, ColourSearch};
use crate::model::{Artwork, path_data};
use crate::resource::{ExportSvg, FindArea, ViewOp};
use crate::selection::Select;
use crate::view_ops::{self, Awaiting};

/// An answer still waiting for its pictures, and who it goes to.
pub(super) struct Due {
    waiting: Waiting,
    reply: ui::framework::EffectReply,
}

/// What an answer waits for.
enum Waiting {
    /// A view's or an export's pictures.
    Answer(Awaiting<ActionOutput>),
    /// The magic wand's or a colour's area being found for find-area.
    Colour(Box<Colouring>),
}

/// A colour find-area is looking for in the artwork as it was invoked on.
struct Colouring {
    search: ColourSearch,
    art: Artwork,
}

impl CanvasSurface {
    /// `op` answered at once when its pictures are, and otherwise through
    /// the effect's reply once the frame loop rendered them.
    pub(super) fn answer_when_drawn(
        &mut self,
        op: &ViewOp,
        cx: &mut Reading,
    ) -> ui::framework::EffectAnswer {
        let mut answer = self.answer(op, cx)?;
        if let Some(done) = answer.poll() {
            return done.map(ui::framework::Effected::Done);
        }
        self.due.push(Due {
            waiting: Waiting::Answer(answer),
            reply: cx.reply(),
        });
        Ok(ui::framework::Effected::Pending)
    }

    /// Answer the views whose pictures came.
    pub(super) fn take_due(&mut self) {
        let due = std::mem::take(&mut self.due);
        for mut due in due {
            match self.came(&mut due.waiting) {
                Some(done) => due.reply.finish(done),
                None => self.due.push(due),
            }
        }
    }

    /// What `waiting` came to, once its pictures came.
    fn came(&mut self, waiting: &mut Waiting) -> Option<Result<ActionOutput, String>> {
        match waiting {
            Waiting::Answer(answer) => answer.poll(),
            Waiting::Colour(colouring) => self.look_for(colouring),
        }
    }

    /// Find-area's magic wand or colour: found in pictures of the artwork,
    /// then selected as one step, once the frame loop rendered them where
    /// no thread can.
    pub(super) fn colour_job(
        &mut self,
        find: FindArea,
        query: ColourQuery,
        art: Artwork,
        document: ui::tilo_resource::Doc,
        cx: &mut Reading,
    ) -> ui::framework::Effected {
        let text = self.look_text.get_or_insert_with(TextRenderer::new);
        let mut renderer = self.tools.renderer.borrow_mut();
        let patient = std::mem::replace(&mut renderer.pictures.patient, true);
        let search = ColourSearch::start(&mut renderer, text, &art, query);
        renderer.pictures.patient = patient;
        drop(renderer);
        let asked = art.clone();
        let reply = cx.reply_then(move |_, output, cx| {
            let d = output
                .value
                .as_ref()
                .and_then(|value| value.get("d")?.as_str())
                .ok_or("the area found went missing")?
                .to_owned();
            let select = Select {
                d: Some(d),
                mode: find.mode,
                feather: find.feather,
                ..Select::default()
            };
            let answer = select_found(select, &asked, &document, cx)?;
            Ok(ui::framework::Effected::value(answer))
        });
        let mut due = Due {
            waiting: Waiting::Colour(Box::new(Colouring { search, art })),
            reply,
        };
        match self.came(&mut due.waiting) {
            Some(found) => due.reply.finish(found),
            None => self.due.push(due),
        }
        ui::framework::Effected::Pending
    }

    /// The area `colouring` found, as path data, once its pictures came.
    fn look_for(&mut self, colouring: &mut Colouring) -> Option<Result<ActionOutput, String>> {
        let text = self.look_text.get_or_insert_with(TextRenderer::new);
        let mut renderer = self.tools.renderer.borrow_mut();
        let patient = std::mem::replace(&mut renderer.pictures.patient, true);
        let found = colouring.search.poll(&mut renderer, text, &colouring.art);
        renderer.pictures.patient = patient;
        Some(found?.and_then(|found| {
            if found.is_empty() {
                return Err("no part of the artwork shows that colour".into());
            }
            Ok(listing(json!({"d": path_data(&found.to_path())})))
        }))
    }

    /// The invocation sees one immutable revision, even when the person edits
    /// while its settled picture is being rendered.
    pub(super) fn picture_job(&self, op: ViewOp, cx: &mut Reading) -> ui::framework::EffectAnswer {
        let version = match &op {
            ViewOp::Look { look } => look.version.as_deref(),
            _ => None,
        };
        let (art, revision, shown) = Self::at_version(cx, version)?;
        let path = match &op {
            ViewOp::Export { export } => export.path.clone(),
            _ => None,
        };
        let pictures = self.pictures.clone();
        let content = crate::content::store();
        let phase = ui::i18n::I18nManager::lookup_plain("canvas-rendering-artwork");
        Ok(cx.thread(
            move |progress| {
                progress.report(phase, None, None);
                pictures.render(&art, content, &op)
            },
            move |_, result, _| {
                let (picture, info) = result?;
                written(&picture, info, path.as_deref())
                    .and_then(|info| pictured(Some(&picture), info, &revision, shown.as_deref()))
                    .map(ui::framework::Effected::Done)
            },
        ))
    }

    /// `op` from the artwork it names, waiting for any picture it needs.
    fn answer(&mut self, op: &ViewOp, cx: &Reading) -> Result<Awaiting<ActionOutput>, String> {
        let version = match op {
            ViewOp::Look { look } => look.version.as_deref(),
            ViewOp::Inspect { inspect } => inspect.version.as_deref(),
            ViewOp::Measure { measure } => measure.version.as_deref(),
            _ => None,
        };
        let (art, revision, shown) = Self::at_version(cx, version)?;
        let versions = match op {
            ViewOp::CompareVersions { diff } => Some(Self::versions(cx, &art, diff)?),
            _ => None,
        };
        // Read before the renderer is lent to the drawing below.
        let listed = match op {
            ViewOp::Inspect { inspect } => {
                let mut value = view_ops::inspect(&art, inspect)?;
                stamp(&mut value, &revision, shown.as_deref());
                if let Value::Object(fields) = &mut value
                    && shown.is_none()
                {
                    fields.insert("selected".into(), json!(self.selection));
                    if let Some(reference) = self.reference_info(&art) {
                        fields.insert("reference".into(), reference);
                    }
                }
                Some(value)
            }
            _ => None,
        };
        let text = self.look_text.get_or_insert_with(TextRenderer::new);
        let mut renderer = self.tools.renderer.borrow_mut();
        // A picture made for a caller waits for the images in it to decode.
        let patient = std::mem::replace(&mut renderer.pictures.patient, true);
        let picture = |made: Awaiting<(Option<view_ops::Picture>, Value)>| {
            let shown = shown.clone();
            let revision = revision.clone();
            made.then(move |(picture, info)| {
                pictured(picture.as_ref(), info, &revision, shown.as_deref())
            })
        };
        let some = |made: Awaiting<(view_ops::Picture, Value)>| {
            made.then(|(picture, info)| Ok((Some(picture), info)))
        };
        let answer = match op {
            ViewOp::Look { look } => view_ops::draw_look(&mut renderer, text, &art, look)
                .map(|(drawing, finish)| picture(some(finish.of(drawing)))),
            // The export's finish writes the file a path names.
            ViewOp::Export { export } => view_ops::draw_export(&mut renderer, text, &art, export)
                .map(|(drawing, finish)| picture(some(finish.of(drawing)))),
            ViewOp::CompareVersions { diff } => match versions {
                Some(versions) => Ok(picture(view_ops::diff(
                    &mut renderer,
                    text,
                    &versions,
                    diff.look.unwrap_or(false),
                ))),
                None => Err("the versions to compare could not be read".into()),
            },
            ViewOp::Compare { compare } => view_ops::compare(&mut renderer, text, &art, compare)
                .map(|made| picture(some(made))),
            ViewOp::ExportSvg { svg: export } => {
                svg(&mut renderer, text, &art, revision.clone(), export)
            }
            ViewOp::Inspect { .. } => {
                Ok(Awaiting::done(Ok(listing(listed.unwrap_or(Value::Null)))))
            }
            ViewOp::Measure { measure } => {
                let (revision, shown) = (revision.clone(), shown.clone());
                view_ops::measure(&mut renderer, text, &art, &measure.ids).map(|measured| {
                    measured.then(move |measured| {
                        let mut value = json!({"measured": measured});
                        stamp(&mut value, &revision, shown.as_deref());
                        Ok(listing(value))
                    })
                })
            }
            ViewOp::Media => Ok(Awaiting::done(Ok(listing(view_ops::media(&art))))),
            ViewOp::SaveArtwork { save } => Ok(Awaiting::done(saved(
                crate::document::save(&art, save),
                &|info| stamp(info, &revision, shown.as_deref()),
            ))),
            ViewOp::FindArea { .. } => Err("area finding is handled as a job".into()),
        };
        renderer.pictures.patient = patient;
        answer
    }

    /// The reference picture as `inspect` reports it, when there is one.
    fn reference_info(&self, art: &Artwork) -> Option<Value> {
        let reference = art.reference.as_ref()?;
        let mut renderer = self.tools.renderer.borrow_mut();
        let frame = renderer.reference_frame(art);
        let pixels = renderer
            .reference_image(art)
            .map(|image| [image.width, image.height]);
        Some(json!({
            "image": reference.image,
            "frame": frame.map(|frame| [frame.x0, frame.y0, frame.width(), frame.height()]),
            "pixels": pixels,
            "opacity": reference.opacity,
            "visible": reference.visible,
        }))
    }
}

/// Select `select`, an area found in `art`, as the invoker, while the
/// artwork is still `art`: what the selection then is.
pub(super) fn select_found(
    select: Select,
    art: &Artwork,
    document: &ui::tilo_resource::Doc,
    cx: &Reading,
) -> Result<Value, String> {
    if cx.live().with(|current| current != art) {
        return Err(ui::i18n::I18nManager::lookup_plain(
            "canvas-finding-changed",
        ));
    }
    if cx.dispatcher().read_only() {
        return Err(ui::i18n::I18nManager::lookup_plain(
            "canvas-finding-read-only",
        ));
    }
    let mut selected = Artwork::from_doc(document).map_err(|error| error.to_string())?;
    let before = selected.clone();
    select
        .apply(&mut selected, &before)
        .map_err(|refusal| refusal.0)?;
    let answer = json!({"selection": crate::answer::selection(&selected)});
    cx.submit(Artwork::SELECT.with(select));
    Ok(answer)
}

/// An answer that is only `value`.
fn listing(value: Value) -> ActionOutput {
    ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    }
}

/// `info` with the revision it answers from and the version's name.
fn stamp(info: &mut Value, revision: &str, shown: Option<&str>) {
    if let Value::Object(fields) = info {
        fields.insert("revision".into(), revision.into());
        if let Some(shown) = shown {
            fields.insert("version".into(), shown.into());
        }
    }
}

/// `info` saying where `picture` was written, when the caller named a path.
fn written(
    picture: &view_ops::Picture,
    mut info: Value,
    path: Option<&str>,
) -> Result<Value, String> {
    let Some(path) = path else {
        return Ok(info);
    };
    let extensions: &[&str] = match picture.format {
        "jpeg" => &["jpg", "jpeg"],
        "webp" => &["webp"],
        _ => &["png"],
    };
    let written = crate::document::export_to(path, extensions, &picture.bytes)?;
    if let Value::Object(fields) = &mut info {
        fields.insert("path".into(), written.into());
    }
    Ok(info)
}

/// The answer of `info`, stamped, with `picture` kept as content.
fn pictured(
    picture: Option<&view_ops::Picture>,
    mut info: Value,
    revision: &str,
    shown: Option<&str>,
) -> Result<ActionOutput, String> {
    stamp(&mut info, revision, shown);
    let Some(picture) = picture else {
        return Ok(listing(info));
    };
    let hash = crate::content::store()
        .put(&picture.bytes)
        .map_err(|error| format!("the picture could not be stored: {error}"))?;
    let address = tilo_actions::content_resource(&hash)
        .ok_or_else(|| "the picture has no content address".to_owned())?;
    if let Value::Object(fields) = &mut info {
        fields.insert("content".into(), address.clone().into());
    }
    Ok(ActionOutput {
        value: Some(info),
        resources: vec![address],
    })
}

/// The artwork, or the marks `export` names, as SVG once the pictures it
/// embeds came.
fn svg(
    renderer: &mut crate::render::Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    revision: String,
    export: &ExportSvg,
) -> Result<Awaiting<ActionOutput>, String> {
    let part = match &export.ids {
        Some(ids) => Some(view_ops::part(art, ids)?),
        None => None,
    };
    let ground = part.is_none();
    let art = part.as_ref().unwrap_or(art);
    let mut drawing = Some(crate::panels::export::draw_svg(
        renderer,
        text,
        art,
        ground,
        export.outline_text == Some(true),
    ));
    let path = export.path.clone();
    let made = Awaiting::polled(move || {
        let ready = drawing.as_ref()?.ready();
        ready.then(|| drawing.take().map(|drawing| Ok(drawing.finish())))?
    });
    Ok(made.then(move |svg| svg_output(&svg, &revision, path.as_deref())))
}

/// What an SVG export answers: its markup kept as content, written to
/// `path` when named, and inline when small and not written.
fn svg_output(svg: &str, revision: &str, path: Option<&str>) -> Result<ActionOutput, String> {
    let address = crate::resource::store(svg.as_bytes())?;
    let mut value = json!({
        "format": "svg",
        "bytes": svg.len(),
        "content": address,
        "revision": revision,
    });
    let written = match path {
        Some(path) => Some(crate::document::export_to(path, &["svg"], svg.as_bytes())?),
        None => None,
    };
    if let (Some(written), Value::Object(fields)) = (&written, &mut value) {
        fields.insert("path".into(), written.clone().into());
    }
    // Written to a file, the markup need not be read again.
    if written.is_none()
        && svg.len() <= MAX_INLINE_SVG
        && let Value::Object(fields) = &mut value
    {
        fields.insert("svg".into(), svg.into());
    }
    Ok(ActionOutput {
        value: Some(value),
        resources: vec![address],
    })
}

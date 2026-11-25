//! Writing a copy of a document with the reader's markup in it: the pages in the reader's
//! order, its marks as standard PDF annotations, its form values, and the file's own markup
//! kept or replaced.

use serde::{Deserialize, Serialize};
use tilo_pdf::{
    AdoptedAnnotation, AnnotationRef, Edits, FieldValue, FormValue, NewAnnotation,
    NewAnnotationKind, PageEdit, PageSource, Point, Rect, Rotation, StampText,
};

use crate::adopt::kind_of;
use crate::dates::pdf_date;
use crate::{Document, Error, MAX_IMPORT_PAGES};

/// One page of the copy: where it comes from and how far it is turned.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportPage {
    /// Which of the files the request gives: 0 the document being saved, then the imports.
    #[serde(default)]
    pub source: usize,
    /// The page of that file, counted from 0.
    pub page: usize,
    /// A quarter turn clockwise added to the page's own.
    #[serde(default)]
    pub rotation: i32,
    /// Whether this occurrence keeps the file's adopted markup: the first one of a page does,
    /// later copies of the same page do not.
    #[serde(default = "yes")]
    pub primary: bool,
}

fn yes() -> bool {
    true
}

/// One mark the reader made, in the output page's display space.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExportMark {
    /// The output page, counted from 0.
    pub page: usize,
    /// `highlight`, `underline`, `strikeout`, `squiggly`, `ink`, `note`, `freetext` or
    /// `signature`.
    pub kind: String,
    /// `#rrggbb`; the reader's own dark ink for a signature that names none.
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub author: String,
    /// What the mark says: a note's or free text's words, a signature's words as assistive
    /// technology announces them.
    #[serde(default)]
    pub contents: String,
    /// Its `/NM`: the reader's id for it, so a re-export names it the same way.
    #[serde(default)]
    pub name: String,
    /// When it was made, in milliseconds since 1970; 0 for none.
    #[serde(default)]
    pub created_ms: i64,
    /// The lines a text mark covers.
    #[serde(default)]
    pub rects: Vec<[f32; 4]>,
    /// Ink strokes, as x and y in turn.
    #[serde(default)]
    pub strokes: Vec<Vec<f32>>,
    /// Where a note stands, or a stamp's box: `[x, y, w, h]`.
    #[serde(default)]
    pub rect: Option<[f32; 4]>,
    /// A free text's words, and the signature's base64 ink is not here: a signature carries its
    /// strokes and its name.
    #[serde(default)]
    pub text: String,
    /// A free text's font size, in points.
    #[serde(default)]
    pub size: Option<f32>,
    /// Who signed, and when, as the person wrote it.
    #[serde(default)]
    pub signer: String,
    #[serde(default)]
    pub date: String,
}

/// One answer to a comment, written as the annotation it answers' reply.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportReply {
    /// The output page the annotation answers stands on, counted from 0.
    pub page: usize,
    /// Where the reply's own annotation stands.
    pub x: f32,
    pub y: f32,
    pub author: String,
    pub body: String,
    /// The reply's `/NM`.
    pub name: String,
    /// The `/NM` of the annotation it answers.
    pub answers: String,
}

/// One form value the reader filled in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportValue {
    pub name: String,
    /// `text`, `checked` or `choice`.
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub on: bool,
    #[serde(default)]
    pub export: Option<String>,
}

/// A mark of the file's own that the reader took over: kept as the file has it when the reader
/// changed nothing, replaced or removed otherwise.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportAdopted {
    /// The source page it stands on, counted from 0.
    pub page: usize,
    /// Its index in that page's own list.
    pub index: usize,
    #[serde(default)]
    pub name: String,
    pub kind: String,
    pub unchanged: bool,
}

/// Everything a save writes: the pages, the reader's marks, its replies, the form's values and
/// the file's own markup it took over.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SaveRequest {
    #[serde(default)]
    pub pages: Vec<ExportPage>,
    #[serde(default)]
    pub marks: Vec<ExportMark>,
    #[serde(default)]
    pub replies: Vec<ExportReply>,
    #[serde(default)]
    pub values: Vec<ExportValue>,
    #[serde(default)]
    pub adopted: Vec<ExportAdopted>,
}

impl SaveRequest {
    /// The request as JSON, which is how a save reaches this crate from an app.
    ///
    /// # Errors
    ///
    /// Why the request is no JSON.
    pub fn from_json(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|error| format!("the save request is no JSON: {error}"))
    }
}

/// The turn a page's plan adds, as the engine's own turn.
fn rotation(degrees: i32) -> Rotation {
    Rotation::from_degrees(degrees.rem_euclid(360)).unwrap_or_default()
}

/// A point of the output page, in the page's own display space.
fn unrotate(point: Point, turn: i32, own: (f32, f32)) -> Point {
    let (w, h) = own;
    match turn.rem_euclid(360) {
        90 => Point::new(point.y, h - point.x),
        180 => Point::new(w - point.x, h - point.y),
        270 => Point::new(w - point.y, point.x),
        _ => point,
    }
}

fn rect(rect: Rect, turn: i32, own: (f32, f32)) -> tilo_pdf::Rect {
    let a = unrotate(Point::new(rect.x0, rect.y0), turn, own);
    let b = unrotate(Point::new(rect.x1, rect.y1), turn, own);
    tilo_pdf::Rect::from_corners(a, b)
}

fn corners(box_rect: [f32; 4], turn: i32, own: (f32, f32)) -> Rect {
    rect(crate::Rect::from_coordinates(box_rect).to_pdf(), turn, own)
}

fn strokes(strokes: &[Vec<f32>], turn: i32, own: (f32, f32)) -> Vec<Vec<Point>> {
    strokes
        .iter()
        .map(|stroke| {
            stroke
                .chunks_exact(2)
                .map(|pair| unrotate(Point::new(pair[0], pair[1]), turn, own))
                .collect()
        })
        .filter(|stroke: &Vec<Point>| stroke.len() >= 2)
        .collect()
}

/// The stamp a signature is written as: the drawn strokes or the typed name in the box the
/// reader placed, a line under it, the name and the date beside it — in the output page's
/// display space, which the caller maps to the file's.
fn signature_stamp(mark: &ExportMark) -> Option<NewAnnotationKind> {
    let [x, y, w, h] = mark.rect?;
    let base = h * 0.62;
    let at = |u: f32, v: f32| Point::new(x + u, y + v);
    let area = (w * 0.06, h * 0.10, w * 0.88, h * 0.46);
    let mut drawn: Vec<Vec<Point>> = Vec::new();
    let mut text: Vec<StampText> = Vec::new();
    if mark.strokes.is_empty() {
        let signer = mark.signer.trim();
        if !signer.is_empty() {
            text.push(StampText {
                at: at(area.0, area.1 + area.3 * 0.85),
                size: (area.3 * 0.72).clamp(10.0, 22.0),
                text: signer.to_owned(),
            });
        }
    } else {
        for stroke in &mark.strokes {
            let points: Vec<Point> = stroke
                .chunks_exact(2)
                .map(|pair| {
                    at(
                        area.0 + pair[0].clamp(0.0, 1.0) * area.2,
                        area.1 + pair[1].clamp(0.0, 1.0) * area.3,
                    )
                })
                .collect();
            if points.len() >= 2 {
                drawn.push(points);
            }
        }
    }
    drawn.push(vec![at(8.0, base), at(w - 8.0, base)]);
    let signer = mark.signer.trim();
    if !signer.is_empty() {
        text.push(StampText {
            at: at(8.0, base - 12.0),
            size: 10.0,
            text: signer.to_owned(),
        });
    }
    if !mark.date.trim().is_empty() {
        text.push(StampText {
            at: at(8.0, base - 24.0),
            size: 7.5,
            text: mark.date.trim().to_owned(),
        });
    }
    Some(NewAnnotationKind::Stamp {
        rect: Rect::from_xywh(x, y, w, h),
        strokes: drawn,
        stroke_width: 1.6,
        text,
    })
}

/// Save the reader's copy of `sources[0]` — the document it read — with the pages and marks the
/// request gives. `sources[1..]` are the files its imported pages come from.
///
/// # Errors
///
/// Why a file is no PDF, why a page cannot be read or written, or that this device can write
/// none now.
pub fn save(sources: &[&[u8]], request: &SaveRequest) -> Result<Vec<u8>, Error> {
    let Some((own, imports)) = sources.split_first() else {
        return Err(Error::Unreadable("no file was given to save".to_owned()));
    };
    if request.pages.len() > crate::MAX_PAGES || imports.len() >= MAX_IMPORT_PAGES {
        return Err(Error::Unreadable(
            "a copy holds fewer pages than that".to_owned(),
        ));
    }
    let document = Document::open(own)?;
    let imported = imports
        .iter()
        .map(|bytes| Document::open(bytes))
        .collect::<Result<Vec<_>, _>>()?;

    // Each output page's source geometry: the page's own display size, to map the reader's
    // display-space marks back into the file's user space.
    let geometry = |source: usize, page: usize| -> Result<(f32, f32), Error> {
        if source == 0 {
            document.page_size(page + 1)
        } else {
            imported
                .get(source - 1)
                .ok_or(Error::NoPage(page + 1))?
                .page_size(page + 1)
        }
    };

    let mut edits = Edits::default();
    let identity = request.pages.iter().enumerate().all(|(at, page)| {
        page.source == 0 && page.page == at && page.rotation.rem_euclid(360) == 0
    }) && request.pages.len() == document.page_count();
    if !identity {
        edits.pages = request
            .pages
            .iter()
            .map(|page| PageEdit {
                source: if page.source == 0 {
                    PageSource::Original(page.page)
                } else {
                    PageSource::Imported {
                        import: page.source - 1,
                        page: page.page,
                    }
                },
                rotation: rotation(page.rotation),
                primary: page.primary,
            })
            .collect();
    }

    for mark in &request.marks {
        let Some(page) = request.pages.get(mark.page) else {
            continue;
        };
        let own_size = geometry(page.source, page.page)?;
        let turn = page.rotation;
        let colour = crate::parse_colour(mark.color.as_deref().unwrap_or("#282131"));
        let kind = match mark.kind.as_str() {
            "highlight" | "underline" | "strikeout" | "squiggly" => {
                let quads: Vec<Rect> = mark
                    .rects
                    .iter()
                    .map(|r| corners(*r, turn, own_size))
                    .collect();
                if quads.is_empty() {
                    continue;
                }
                match mark.kind.as_str() {
                    "underline" => NewAnnotationKind::Underline { quads },
                    "strikeout" => NewAnnotationKind::StrikeOut { quads },
                    "squiggly" => NewAnnotationKind::Squiggly { quads },
                    _ => NewAnnotationKind::Highlight { quads },
                }
            }
            "ink" => {
                let drawn = strokes(&mark.strokes, turn, own_size);
                if drawn.is_empty() {
                    continue;
                }
                NewAnnotationKind::Ink {
                    strokes: drawn,
                    width: 2.0,
                }
            }
            "note" => {
                let [x, y, ..] = mark.rect.unwrap_or([0.0, 0.0, 0.0, 0.0]);
                NewAnnotationKind::Note {
                    at: unrotate(Point::new(x, y), turn, own_size),
                }
            }
            "freetext" => {
                let Some(box_rect) = mark.rect else {
                    continue;
                };
                NewAnnotationKind::FreeText {
                    rect: corners(box_rect, turn, own_size),
                    font_size: mark.size.unwrap_or(14.0),
                }
            }
            "signature" => {
                // The stamp is drawn in the output page's space, then mapped into the file's.
                let Some(kind) = signature_stamp(mark) else {
                    continue;
                };
                let mapped = match kind {
                    NewAnnotationKind::Stamp {
                        rect: box_rect,
                        strokes,
                        stroke_width,
                        text,
                    } => NewAnnotationKind::Stamp {
                        rect: rect(box_rect, turn, own_size),
                        strokes: strokes
                            .into_iter()
                            .map(|stroke| {
                                stroke
                                    .into_iter()
                                    .map(|p| unrotate(p, turn, own_size))
                                    .collect()
                            })
                            .collect(),
                        stroke_width,
                        text: text
                            .into_iter()
                            .map(|line| StampText {
                                at: unrotate(line.at, turn, own_size),
                                ..line
                            })
                            .collect(),
                    },
                    other => other,
                };
                mapped
            }
            _ => continue,
        };
        edits.annotations.push(NewAnnotation {
            page: mark.page,
            kind,
            color: colour,
            author: mark.author.clone(),
            contents: mark.contents.clone(),
            name: mark.name.clone(),
            modified: pdf_date(mark.created_ms),
            in_reply_to: None,
            reply_type: tilo_pdf::ReplyType::Reply,
        });
    }

    for reply in &request.replies {
        if request.pages.get(reply.page).is_none() {
            continue;
        }
        edits.annotations.push(NewAnnotation {
            page: reply.page,
            kind: NewAnnotationKind::Note {
                at: Point::new(reply.x, reply.y),
            },
            color: tilo_pdf::Rgba::new(0, 0, 0, 255),
            author: reply.author.clone(),
            contents: reply.body.clone(),
            name: reply.name.clone(),
            modified: String::new(),
            in_reply_to: Some(reply.answers.clone()),
            reply_type: tilo_pdf::ReplyType::Reply,
        });
    }

    edits.form_values = request
        .values
        .iter()
        .map(|value| FormValue {
            name: value.name.clone(),
            value: match value.kind.as_str() {
                "checked" => FieldValue::Checked {
                    on: value.on,
                    export: value.export.clone(),
                },
                "choice" => FieldValue::Choice(value.text.clone()),
                _ => FieldValue::Text(value.text.clone()),
            },
        })
        .collect();

    edits.adopted = request
        .adopted
        .iter()
        .filter_map(|entry| {
            Some(AdoptedAnnotation {
                target: AnnotationRef {
                    page: entry.page,
                    index: entry.index,
                    name: entry.name.clone(),
                    kind: kind_of(&entry.kind)?,
                },
                unchanged: entry.unchanged,
            })
        })
        .collect();

    let imports: Vec<&tilo_pdf::Document> = imported.iter().map(|doc| &doc.inner).collect();
    Ok(document.inner.export(&edits, &imports)?)
}

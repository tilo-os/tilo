//! The device's typesetting, as `so.tilo.typst-editor` reaches it: the one engine crate
//! (`tilo-typst`) answering with the JSON the package reads into its own records.
//!
//! Every call is made on the instance's thread, and every picture and PDF the engine makes is
//! kept in the app's content store here, named as the file the package shows or saves: the
//! package never holds bytes or a path.

use serde::Deserialize;
use serde_json::json;
use tilolang_tilo::instance::Platform;

use tilo_typst::{Compiled, Engine, Heading};

/// The app whose engine this is.
pub const APP: &str = tilo_typst::APP;

/// What an op is asked.
#[derive(Deserialize)]
struct Ask {
    source: String,
    /// The page, counted from 1, for `picture` and `text`.
    #[serde(default)]
    page: i64,
    /// The width a page is drawn at, in pixels, for `picture`.
    #[serde(default)]
    width: i64,
    /// The person's date and the instant now, when the caller states them; the platform's own
    /// otherwise.
    #[serde(default)]
    today: Option<String>,
    #[serde(default)]
    now: Option<i64>,
}

/// One `engine.call` for this app.
///
/// # Errors
///
/// An op this engine does not run, an ask it does not read, or work it cannot do.
pub fn call(platform: &dyn Platform, op: &str, input: &str) -> Result<String, String> {
    let ask: Ask = serde_json::from_str(input)
        .map_err(|why| format!("the ask is no JSON this engine reads: {why}"))?;
    let engine = Engine::shared();
    let clock = clock(platform, &ask)?;
    match op {
        "compile" => {
            let compiled = engine.compile(&ask.source, clock);
            let pdf = match &compiled.pdf {
                Some(bytes) => Some(
                    platform
                        .keep_content("document.pdf", "application/pdf", bytes)
                        .map_err(|why| format!("the PDF could not be kept: {why}"))?,
                ),
                None => None,
            };
            let Compiled {
                ok,
                elapsed_ms,
                pdf: _,
                pdf_trouble,
                ..
            } = compiled;
            // A record of resources is no JSON value, so the PDF's file is named by its parts.
            to_json(&json!({
                "ok": ok,
                "elapsed_ms": elapsed_ms,
                "pdf_url": pdf.as_ref().map(tilolang_tilo::runtime::blob::FileValue::url),
                "pdf_name": pdf.as_ref().map(|file| file.name.clone()),
                "pdf_size": pdf.as_ref().map(|file| file.size),
                "pdf_trouble": pdf_trouble,
            }))
        }
        "pages" => {
            let pages = engine.page_sizes(&ask.source, clock)?;
            to_json(&json!(pages))
        }
        "problems" => {
            let problems = engine.problems(&ask.source, clock);
            to_json(&json!(problems))
        }
        "picture" => {
            let page = page_of(ask.page)?;
            let width = u32::try_from(ask.width.clamp(16, 8192)).unwrap_or(8192);
            let png = engine.picture(&ask.source, page, width, clock)?;
            let kept = platform
                .keep_content(&format!("page-{page}.png"), "image/png", &png)
                .map_err(|why| format!("the page's picture could not be kept: {why}"))?;
            to_json(&file_json(&kept))
        }
        "outline" => {
            let headings: Vec<Heading> = engine.outline(&ask.source);
            to_json(&json!(headings))
        }
        "text" => {
            let page = page_of(ask.page)?;
            let cells = engine.text(&ask.source, page, clock)?;
            to_json(&json!(cells))
        }
        other => Err(format!(
            "`{other}` is no op this app's typesetting runs: it runs `compile`, `pages`, \
             `problems`, `picture`, `outline` and `text`"
        )),
    }
}

/// The page an op asks for, counted from 1.
fn page_of(page: i64) -> Result<usize, String> {
    usize::try_from(page)
        .ok()
        .filter(|page| *page > 0)
        .ok_or_else(|| format!("pages are counted from 1, and {page} is no page"))
}

/// The clock the engine reads: what the caller states, else the person's own date and instant.
fn clock(platform: &dyn Platform, ask: &Ask) -> Result<tilo_typst::Clock, String> {
    if let (Some(today), Some(now)) = (&ask.today, ask.now)
        && let Some(clock) = tilo_typst::Clock::at(today, now)
    {
        return Ok(clock);
    }
    let moment = platform.moment();
    let date = moment.date();
    let today = format!(
        "{}-{:02}-{:02}",
        date.year(),
        i32::from(date.month()),
        i32::from(date.day())
    );
    tilo_typst::Clock::at(&today, moment.timestamp().as_second())
        .ok_or_else(|| format!("`{today}` is no date this device reads"))
}

/// A kept file as the package names it: where its bytes are, what it is called and what it is.
fn file_json(file: &tilolang_tilo::runtime::blob::FileValue) -> serde_json::Value {
    json!({
        "url": file.url(),
        "name": file.name,
        "media": file.media,
        "size": file.size,
    })
}

fn to_json(value: &serde_json::Value) -> Result<String, String> {
    serde_json::to_string(value)
        .map_err(|why| format!("the typesetting engine's answer cannot be written: {why}"))
}

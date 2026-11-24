//! Typst typesetting, as its own library in the runtime bundle
//! (`libtilo_typst_engine.so`), opened the first time `so.tilo.typst-editor`
//! asks it to typeset.
//!
//! An app runs sandboxed and typesets nothing itself, so it asks this engine
//! through `engine.call`; the process that hosts it opens this library once and
//! files it under the app's name, so no other app is answered by it. The app
//! reads every answer as JSON into its own records; the process keeps the
//! pictures and the PDF in the app's content store and names them as files,
//! which the engine asks of its [`Device`] and never holds itself.
//!
//! The ops the app asks, each documented where it is answered:
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `compile` | `{source, today, now}` | `{ok, elapsed_ms, pdf_url, pdf_name, pdf_size, pdf_trouble}` |
//! | `picture` | `{source, page, width, today, now}` | a page drawn as a PNG file |
//! | `outline` | `{source}` | `{headings}` |
//! | `text` | `{source, page, today, now}` | `{cells}` |
//!
//! Outside the thin build this crate is an empty library.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
mod engine {
    use serde::Deserialize;
    use serde_json::json;
    use tilo_engine_loader::{Device, EngineApi, File, Frames, JobOp, JobSink};
    use tilo_typst::{Clock, Compiled, Engine, Heading};

    /// The app whose typesetting this is: the one that asks these ops, and no
    /// other.
    pub const APP: &str = "so.tilo.typst-editor";

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
        /// The person's date and the instant now, when the caller states them;
        /// the device's own otherwise.
        #[serde(default)]
        today: Option<String>,
        #[serde(default)]
        now: Option<i64>,
    }

    /// The jobs this engine runs: none. Every op answers now.
    fn jobs() -> &'static [JobOp] {
        &[]
    }

    /// One `engine.call` for this app.
    fn call(device: &Device, op: &str, input: &str) -> Result<String, String> {
        let ask: Ask = serde_json::from_str(input)
            .map_err(|why| format!("the ask is no JSON this engine reads: {why}"))?;
        let engine = Engine::shared();
        let clock = clock(device, &ask)?;
        match op {
            "compile" => {
                let compiled = engine.compile(&ask.source, clock);
                let pdf = match &compiled.pdf {
                    Some(bytes) => Some(
                        device
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
                // A record of resources is no JSON value, so the PDF's file is
                // named by its parts.
                to_json(&json!({
                    "ok": ok,
                    "elapsed_ms": elapsed_ms,
                    "pdf_url": pdf.as_ref().map(File::url),
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
                let kept = device
                    .keep_content(&format!("page-{page}.png"), "image/png", &png)
                    .map_err(|why| format!("the page's picture could not be kept: {why}"))?;
                to_json(&json!({
                    "url": kept.url(),
                    "name": kept.name,
                    "media": kept.media,
                    "size": kept.size,
                }))
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

    /// One `engine.start`: this engine runs no jobs.
    fn start(
        _device: &Device,
        _job: &str,
        op: &str,
        _input: &str,
        _moved: &JobSink,
    ) -> Result<(), String> {
        Err(format!("`{op}` is no job this app's typesetting runs"))
    }

    /// One job's progress: this engine runs no jobs.
    fn status(_device: &Device, job: &str) -> Result<String, String> {
        Err(format!("no job `{job}` runs on this device"))
    }

    /// One stream: this engine runs none.
    fn stream(
        _device: &Device,
        _op: &str,
        _input: &str,
        _frames: &Frames,
    ) -> Result<String, String> {
        Err("this engine runs no streams".to_owned())
    }

    /// One stopped job: this engine runs none, so it holds none.
    fn cancel(_device: &Device, _job: &str) -> bool {
        false
    }

    /// The one engine this library answers with.
    static API: EngineApi = EngineApi {
        app: APP,
        jobs,
        call,
        start,
        status,
        stream,
        cancel,
    };

    /// The entry the process opens this library for.
    #[unsafe(no_mangle)]
    pub fn tilo_engine_entry() -> &'static EngineApi {
        &API
    }

    /// The page an op asks for, counted from 1.
    fn page_of(page: i64) -> Result<usize, String> {
        usize::try_from(page)
            .ok()
            .filter(|page| *page > 0)
            .ok_or_else(|| format!("pages are counted from 1, and {page} is no page"))
    }

    /// The clock the engine reads: what the caller states, else the person's
    /// own date and instant.
    fn clock(device: &Device, ask: &Ask) -> Result<Clock, String> {
        if let (Some(today), Some(now)) = (&ask.today, ask.now)
            && let Some(clock) = Clock::at(today, now)
        {
            return Ok(clock);
        }
        let (today, now) = device.moment()?;
        Clock::at(&today, now).ok_or_else(|| format!("`{today}` is no date this device reads"))
    }

    fn to_json(value: &serde_json::Value) -> Result<String, String> {
        serde_json::to_string(value)
            .map_err(|why| format!("the typesetting engine's answer cannot be written: {why}"))
    }

    /// Proves this engine loads and runs against the runtime it is built with:
    /// a small page typeset, drawn and written as a PDF, against a fixed clock.
    pub fn self_check() -> Result<String, String> {
        let clock = Clock::at("2024-01-02", 1_704_153_600)
            .ok_or_else(|| "a fixed clock is no date this device reads".to_owned())?;
        let source = "#set page(width: 200pt, height: 120pt)\n= Check\nTypeset, drawn and \\
                      written.";
        let engine = Engine::shared();
        let compiled = engine.compile(source, clock);
        if !compiled.ok {
            return Err(format!(
                "the check's page does not typeset: {:?}",
                compiled
                    .diagnostics
                    .iter()
                    .map(|problem| problem.message.as_str())
                    .collect::<Vec<_>>()
            ));
        }
        if compiled.pages.len() != 1 {
            return Err(format!(
                "the check's page is {} pages, not one",
                compiled.pages.len()
            ));
        }
        let pdf = compiled
            .pdf
            .as_ref()
            .ok_or_else(|| format!("the check writes no PDF: {:?}", compiled.pdf_trouble))?;
        let png = engine.picture(source, 1, 200, clock)?;
        Ok(format!(
            "typeset one page, {} bytes of PDF and {} of PNG",
            pdf.len(),
            png.len()
        ))
    }
}

/// The engine's check binary's report: whether the self-check passed.
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn tilo_typst_engine_check() -> bool {
    match engine::self_check() {
        Ok(report) => {
            println!("tilo-typst-engine: {report}");
            true
        }
        Err(error) => {
            eprintln!("tilo-typst-engine: {error}");
            false
        }
    }
}

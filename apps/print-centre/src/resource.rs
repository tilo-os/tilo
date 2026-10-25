//! Print Centre as a resource: the print and scan defaults a person keeps
//! across their devices. Printers, documents in the queues and scanners are
//! this machine's live state, published as live rows; printing, scanning and
//! the pages are the surface's [`PrintCentreEffect`]s.

use ui::framework::{
    Effect, EffectAnswer, EffectCx, EffectStatus, Fixture, LiveRow, ResourceApp, ResourceSurface,
    RunMode, SurfaceCx, ViewCx,
};
use ui::prelude::*;
use ui::tilo_resource::{self, Atom, Invoke};

use crate::surface::PrintCentre;

/// Scan resolutions offered, in dots per inch.
pub const SCAN_RESOLUTIONS: [u16; 3] = [150, 300, 600];
/// Scan appearances offered.
pub const SCAN_COLORS: [&str; 3] = ["color", "grayscale", "line-art"];
/// Paper sources offered.
pub const SCAN_SOURCES: [&str; 3] = ["flatbed", "feeder", "feeder-duplex"];
/// Text recognition languages offered (ISO 639-2), with their names.
pub const OCR_LANGUAGES: [(&str, &str); 4] = [
    ("eng", "English"),
    ("deu", "German"),
    ("fra", "French"),
    ("spa", "Spanish"),
];

/// How a person prints and scans unless they choose otherwise for one
/// document. It follows them to every device. Each default is edited at its
/// path; a value that is not offered is refused however it is set.
#[derive(Model, Clone, Debug, PartialEq)]
#[model(check = PrintDefaults::offered)]
pub struct PrintDefaults {
    /// Print on both sides of the paper.
    pub two_sided: bool,
    /// Scan resolution, one of [`SCAN_RESOLUTIONS`].
    pub scan_resolution: u16,
    /// Scan appearance, one of [`SCAN_COLORS`].
    pub scan_color: Atom,
    /// Paper source, one of [`SCAN_SOURCES`].
    pub scan_source: Atom,
    /// Language text is recognized in, one of [`OCR_LANGUAGES`].
    pub ocr_language: Atom,
}

impl Default for PrintDefaults {
    fn default() -> Self {
        Self {
            two_sided: false,
            scan_resolution: 300,
            scan_color: Atom(SCAN_COLORS[0].to_owned()),
            scan_source: Atom(SCAN_SOURCES[0].to_owned()),
            ocr_language: Atom(OCR_LANGUAGES[0].0.to_owned()),
        }
    }
}

#[resource(kind = "print-centre.defaults", label = "Print Centre")]
impl PrintDefaults {}

impl PrintDefaults {
    /// Every default is one the scan and print controls offer.
    fn offered(&self) -> Result<(), &'static str> {
        if !SCAN_RESOLUTIONS.contains(&self.scan_resolution) {
            return Err("scan at 150, 300 or 600 dpi");
        }
        if !SCAN_COLORS.contains(&self.scan_color.0.as_str()) {
            return Err("scan in color, grayscale or line-art");
        }
        if !SCAN_SOURCES.contains(&self.scan_source.0.as_str()) {
            return Err("scan from the flatbed, feeder or feeder-duplex");
        }
        if !OCR_LANGUAGES
            .iter()
            .any(|(code, _)| *code == self.ocr_language.0)
        {
            return Err("recognize eng, deu, fra or spa");
        }
        Ok(())
    }

    /// The offered resolution's index, as the scan controls show it.
    pub fn resolution_index(&self) -> usize {
        SCAN_RESOLUTIONS
            .iter()
            .position(|dpi| *dpi == self.scan_resolution)
            .unwrap_or(1)
    }

    pub fn color_index(&self) -> usize {
        SCAN_COLORS
            .iter()
            .position(|color| *color == self.scan_color.0)
            .unwrap_or_default()
    }

    pub fn source_index(&self) -> usize {
        SCAN_SOURCES
            .iter()
            .position(|source| *source == self.scan_source.0)
            .unwrap_or_default()
    }

    pub fn language_index(&self) -> usize {
        OCR_LANGUAGES
            .iter()
            .position(|(code, _)| *code == self.ocr_language.0)
            .unwrap_or_default()
    }
}

/// What Print Centre does on this machine: the pages it shows, and the work
/// it asks of CUPS and SANE.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "print-centre.effects")]
pub enum PrintCentreEffect {
    /// Refresh printers and documents.
    #[effect(kind = "read")]
    Refresh,
    /// Find network printers.
    #[effect(job)]
    FindPrinters,
    /// Add printer.
    ///
    /// Sets up a printer the last search found nearby for every app.
    #[effect(job)]
    AddPrinter {
        #[param("The found printer's address")]
        printer: String,
    },
    /// Open dashboard.
    #[effect(view)]
    OpenDashboard,
    /// Open print queue.
    #[effect(view)]
    OpenQueue,
    /// Open print history.
    #[effect(view)]
    OpenHistory,
    /// Run printer diagnostics.
    #[effect(view)]
    RunDiagnostics,
    /// Print a document.
    ///
    /// Asks for the document, then shows its preview and printing options.
    #[effect(view)]
    PrintDocument,
    /// Scan a document.
    #[effect(view)]
    ScanDocument,
    /// New scanned document.
    ///
    /// Discards the current scanned pages after confirmation. Save the
    /// document first to keep it; exporting one page does not save the rest.
    #[effect(destructive)]
    NewScannedDocument,
    /// Open printer.
    #[effect(view)]
    OpenPrinter {
        #[param("The printer's queue name")]
        printer: String,
    },
    /// Set as default printer.
    #[effect(job)]
    SetDefaultPrinter {
        #[param("The printer's queue name")]
        printer: String,
    },
    /// Pause printer.
    ///
    /// Documents wait until it is resumed.
    #[effect(job)]
    PausePrinter {
        #[param("The printer's queue name")]
        printer: String,
    },
    /// Resume printer.
    #[effect(job)]
    ResumePrinter {
        #[param("The printer's queue name")]
        printer: String,
    },
    /// Cancel document.
    #[effect(job)]
    CancelDocument {
        #[param("The print job's id")]
        job: String,
    },
    /// Print again.
    #[effect(job)]
    RetryDocument {
        #[param("The print job's id")]
        job: String,
    },
    /// Confirm cancelling.
    ///
    /// Shows the document in the queue, asking whether to cancel it.
    #[effect(view, audience = "agents")]
    ConfirmCancel {
        #[param("The print job's id")]
        job: String,
    },
    /// Choose printer.
    ///
    /// Sends the document being set up to this printer.
    #[effect(view)]
    ChoosePrinter {
        #[param("The printer's queue name")]
        printer: String,
    },
    /// Send to printer.
    ///
    /// Prints the document being set up, with its copies and sides.
    #[effect(job)]
    SendToPrinter,
    /// Close print setup.
    ///
    /// Puts the document being set up away without printing it.
    #[effect(view)]
    ClosePrintSetup,
    /// Scan with a scanner.
    ///
    /// Opens scanning with it, keeping the pages scanned so far when
    /// scanning is already open.
    #[effect(view)]
    ScanWith {
        #[param("The scanner's device name")]
        scanner: String,
    },
    /// Scan page.
    ///
    /// Scans a page with the chosen scanner and settings and recognizes its
    /// text.
    #[effect(job)]
    ScanPage,
    /// Scan another page.
    #[effect(view)]
    ScanAnother,
    /// Previous scanned page.
    #[effect(view)]
    PreviousScanPage,
    /// Next scanned page.
    #[effect(view)]
    NextScanPage,
    /// Move scanned page earlier.
    MoveScanPageEarlier,
    /// Move scanned page later.
    MoveScanPageLater,
    /// Remove scanned page.
    RemoveScanPage,
    /// Rotate scanned page.
    ///
    /// Turns the page under review a quarter turn clockwise.
    RotateScanPage,
    /// Crop scanned page.
    ///
    /// Keeps the part of the page under review the crop frame selects.
    CropScanPage,
    /// Update recognized text.
    ///
    /// Recognizes the text of the page under review again.
    #[effect(job)]
    RecognizeText,
    /// Copy recognized text.
    CopyRecognizedText,
    /// Export scanned page.
    ///
    /// Asks where to save the page under review as an image.
    #[effect(attended, job)]
    SaveScannedPage,
    /// Save scanned document.
    ///
    /// Asks where to save every scanned page as one PDF with its text.
    #[effect(attended, job)]
    SaveScannedDocument,
}

/// Framework app.
pub struct PrintCentreShell;

impl ResourceApp for PrintCentreShell {
    type Resource = PrintDefaults;
    const ID: &'static str = "so.tilo.print-centre";
    const TITLE: &'static str = "Print Centre";

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    const ICON: Option<&'static str> = Some("print-centre");

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        if scenario.is_some_and(|name| name.ends_with("narrow")) {
            (520, 820)
        } else {
            (1280, 800)
        }
    }

    fn view(cx: &mut ViewCx<PrintDefaults>) -> Element {
        cx.surface(PrintCentre::new())
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "active",
            "attention",
            "empty",
            "narrow",
            "queue",
            "device",
            "discovery",
            "print",
            "print-narrow",
            "scan",
            "scan-result",
            "scan-result-narrow",
            "history",
            "diagnostics",
            "service-error",
            "service-error-narrow",
        ]
    }

    /// A made-up studio printer, its documents and a scanner, instead of
    /// asking the machine the screenshot is taken on.
    fn fixture(fixture: &mut Fixture<PrintDefaults>) {
        if let Some(snapshot) = crate::surface::scenario_snapshot(fixture.scenario()) {
            fixture.provide(snapshot);
        }
    }

    /// Printing shows a document set to print on both sides.
    fn scenario(name: Option<&str>) -> Vec<Invoke<PrintDefaults>> {
        if name.is_some_and(|name| name.starts_with("print")) {
            Invoke::edit(&PrintDefaults::paths().two_sided.set(true))
                .into_iter()
                .collect()
        } else {
            Vec::new()
        }
    }
}

impl ResourceSurface<PrintDefaults, PrintCentreEffect> for PrintCentre {
    fn present(&mut self, state: &std::rc::Rc<PrintDefaults>) {
        self.present_defaults(state);
    }

    /// A live window asks CUPS and SANE; a screenshot shows its scenario's
    /// made-up devices, or none.
    fn attach(&mut self, cx: SurfaceCx<PrintDefaults>) {
        self.controls = Some(cx.actions());
        self.dispatcher = Some(cx.dispatcher());
        match cx.mode() {
            RunMode::Live => self.refresh(),
            RunMode::Capture => self.show(cx.fixture::<crate::surface::Snapshot>().as_deref()),
            RunMode::Past => self.show(None),
        }
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(
        &mut self,
        effect: PrintCentreEffect,
        cx: &mut EffectCx<PrintDefaults, Self>,
    ) -> EffectAnswer {
        self.perform(effect, cx)
    }

    /// The scan and print steps that have nothing to act on right now.
    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    /// The printers, documents and scanners CUPS and SANE reported.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.device_rows()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(app: &mut TestApp<PrintCentreShell>, edit: &ui::tilo_resource::Edit<PrintDefaults>) {
        let _ = Invoke::edit(edit).map(|edit| app.invoke(edit));
    }

    #[test]
    fn defaults_follow_their_edits_and_refuse_what_is_not_offered() {
        let mut app = TestApp::<PrintCentreShell>::capture(Some("empty")).unwrap();
        assert_eq!(app.state(), PrintDefaults::default());
        let paths = PrintDefaults::paths();
        for edit in [
            paths.scan_resolution.set(600),
            paths.scan_color.set(Atom("grayscale".into())),
            paths.scan_source.set(Atom("feeder".into())),
            paths.ocr_language.set(Atom("fra".into())),
            paths.two_sided.set(true),
        ] {
            app.invoke(Invoke::edit(&edit).unwrap()).unwrap();
        }
        let state = app.state();
        assert_eq!(
            (
                state.resolution_index(),
                state.color_index(),
                state.source_index(),
                state.language_index(),
                state.two_sided
            ),
            (2, 1, 1, 2, true)
        );
        // A resolution or language that is not offered changes nothing.
        set(&mut app, &paths.scan_resolution.set(1200));
        set(&mut app, &paths.ocr_language.set(Atom("klingon".into())));
        assert_eq!(app.state(), state);
    }

    #[test]
    fn history_says_which_default_changed() {
        let mut app = TestApp::<PrintCentreShell>::capture(Some("empty")).unwrap();
        let paths = PrintDefaults::paths();
        for edit in [
            paths.two_sided.set(true),
            paths.scan_resolution.set(600),
            paths.scan_color.set(Atom("line-art".into())),
            paths.scan_source.set(Atom("feeder-duplex".into())),
            paths.ocr_language.set(Atom("deu".into())),
        ] {
            app.invoke(Invoke::edit(&edit).unwrap()).unwrap();
        }
        let catalogue =
            tilo_history::Catalogue::new("en-US", &[include_str!("../locales/en-US.ftl")]);
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        let sentences: Vec<String> = app
            .history()
            .into_iter()
            .filter(|entry| entry.what.is_some())
            .map(|entry| reader.sentence(&entry))
            .collect();
        assert_eq!(
            sentences,
            [
                "Printing on both sides from now on",
                "Scanning at 600 dpi from now on",
                "Scanning in line art from now on",
                "Scanning both sides from the feeder from now on",
                "Recognizing German text from now on",
            ]
        );
    }
}

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::PrintCentreShell::scenarios() {
            let mut app = TestApp::<super::PrintCentreShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}

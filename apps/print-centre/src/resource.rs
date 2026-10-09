//! Print Centre as a resource: the print and scan defaults a person keeps
//! across their devices. Printers, documents in the queues and scanners are
//! this machine's live state, published as live rows; printing, scanning and
//! the pages are the surface's [`PrintCentreEffect`]s.

use ui::framework::{
    Effect, EffectAnswer, EffectCx, EffectStatus, Fixture, LiveRow, ResourceApp, ResourceSurface,
    RunMode, SurfaceCx, ViewCx,
};
use ui::prelude::*;
use ui::tilo_resource::{self, Invoke};

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
/// document. It follows them to every device.
#[derive(Model, Clone, Debug, PartialEq)]
pub struct PrintDefaults {
    /// Print on both sides of the paper.
    pub two_sided: bool,
    /// Scan resolution, one of [`SCAN_RESOLUTIONS`].
    pub scan_resolution: u16,
    /// Scan appearance, one of [`SCAN_COLORS`].
    pub scan_color: String,
    /// Paper source, one of [`SCAN_SOURCES`].
    pub scan_source: String,
    /// Language text is recognized in, one of [`OCR_LANGUAGES`].
    pub ocr_language: String,
}

impl Default for PrintDefaults {
    fn default() -> Self {
        Self {
            two_sided: false,
            scan_resolution: 300,
            scan_color: SCAN_COLORS[0].to_owned(),
            scan_source: SCAN_SOURCES[0].to_owned(),
            ocr_language: OCR_LANGUAGES[0].0.to_owned(),
        }
    }
}

#[resource(kind = "print-centre.defaults", label = "Print Centre")]
impl PrintDefaults {
    /// Print on both sides.
    #[action]
    pub fn set_two_sided(&mut self, #[param("Whether to print on both sides")] two_sided: bool) {
        self.two_sided = two_sided;
    }

    /// Set the scan resolution.
    #[action]
    pub fn set_scan_resolution(
        &mut self,
        #[param("Dots per inch: 150, 300 or 600")] dpi: u16,
    ) -> Result<(), &'static str> {
        if !SCAN_RESOLUTIONS.contains(&dpi) {
            return Err("scan at 150, 300 or 600 dpi");
        }
        self.scan_resolution = dpi;
        Ok(())
    }

    /// Set the scan appearance.
    #[action]
    pub fn set_scan_color(
        &mut self,
        #[param("color, grayscale or line-art")] color: String,
    ) -> Result<(), &'static str> {
        if !SCAN_COLORS.contains(&color.as_str()) {
            return Err("scan in color, grayscale or line-art");
        }
        self.scan_color = color;
        Ok(())
    }

    /// Set the paper source.
    #[action]
    pub fn set_scan_source(
        &mut self,
        #[param("flatbed, feeder or feeder-duplex")] source: String,
    ) -> Result<(), &'static str> {
        if !SCAN_SOURCES.contains(&source.as_str()) {
            return Err("scan from the flatbed, feeder or feeder-duplex");
        }
        self.scan_source = source;
        Ok(())
    }

    /// Set the text recognition language.
    #[action]
    pub fn set_ocr_language(
        &mut self,
        #[param("eng, deu, fra or spa")] language: String,
    ) -> Result<(), &'static str> {
        if !OCR_LANGUAGES.iter().any(|(code, _)| *code == language) {
            return Err("recognize eng, deu, fra or spa");
        }
        self.ocr_language = language;
        Ok(())
    }
}

impl PrintDefaults {
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
            .position(|color| *color == self.scan_color)
            .unwrap_or_default()
    }

    pub fn source_index(&self) -> usize {
        SCAN_SOURCES
            .iter()
            .position(|source| *source == self.scan_source)
            .unwrap_or_default()
    }

    pub fn language_index(&self) -> usize {
        OCR_LANGUAGES
            .iter()
            .position(|(code, _)| *code == self.ocr_language)
            .unwrap_or_default()
    }
}

/// What Print Centre does on this machine: the pages it shows, and the work
/// it asks of CUPS and SANE.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "print-centre.effects", device_only)]
pub enum PrintCentreEffect {
    /// Refresh printers and documents.
    #[effect(permission = "read")]
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
    #[effect(view, programmatic)]
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
    #[effect(job)]
    SaveScannedPage,
    /// Save scanned document.
    ///
    /// Asks where to save every scanned page as one PDF with its text.
    #[effect(job)]
    SaveScannedDocument,
}

/// Framework app.
pub struct PrintCentreShell;

impl ResourceApp for PrintCentreShell {
    type Resource = PrintDefaults;
    const ID: &'static str = "so.tilo.print-centre";
    const TITLE: &'static str = "Print Centre";
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
            vec![PrintDefaults::SET_TWO_SIDED.with(true)]
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

    #[test]
    fn defaults_follow_their_actions_and_refuse_what_is_not_offered() {
        let mut app = TestApp::<PrintCentreShell>::capture(Some("empty")).unwrap();
        assert_eq!(app.state(), PrintDefaults::default());
        app.invoke(PrintDefaults::SET_SCAN_RESOLUTION.with(600))
            .unwrap();
        app.invoke(PrintDefaults::SET_SCAN_COLOR.with("grayscale"))
            .unwrap();
        app.invoke(PrintDefaults::SET_SCAN_SOURCE.with("feeder"))
            .unwrap();
        app.invoke(PrintDefaults::SET_OCR_LANGUAGE.with("fra"))
            .unwrap();
        app.invoke(PrintDefaults::SET_TWO_SIDED.with(true)).unwrap();
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
        let _ = app.invoke(PrintDefaults::SET_SCAN_RESOLUTION.with(1200));
        let _ = app.invoke(PrintDefaults::SET_OCR_LANGUAGE.with("klingon"));
        assert_eq!(app.state(), state);
    }
}

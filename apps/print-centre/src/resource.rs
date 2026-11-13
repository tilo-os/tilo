//! Print Centre as a resource: the print and scan defaults a person keeps across their
//! devices.

use ui::framework::{Effect, Fixture, ResourceApp, ViewCx};
use ui::prelude::*;
use ui::tilo_resource::{self, Atom, Invoke};

pub const SCAN_RESOLUTIONS: [u16; 3] = [150, 300, 600];
pub const SCAN_COLORS: [&str; 3] = ["color", "grayscale", "line-art"];
pub const SCAN_SOURCES: [&str; 3] = ["flatbed", "feeder", "feeder-duplex"];
/// Text recognition languages offered (ISO 639-2), with their names.
pub const OCR_LANGUAGES: [(&str, &str); 4] = [
    ("eng", "English"),
    ("deu", "German"),
    ("fra", "French"),
    ("spa", "Spanish"),
];

/// How a person prints and scans unless they choose otherwise for one document. Each
/// default is edited at its path; a value that is not offered is refused however it is set.
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

impl PrintDefaults {
    /// What the record holds: how the person prints and scans unless they choose otherwise
    /// for one document.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<tilo_resource::__private::serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "print-centre.defaults",
    read_content = PrintDefaults::read_document
)]
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

/// What Print Centre does on this machine: the pages it shows, and the work it asks of CUPS
/// and SANE.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "print-centre.effects")]
pub enum PrintCentreEffect {
    #[effect(kind = "read")]
    Refresh,
    #[effect(job)]
    FindPrinters,
    #[effect(job)]
    AddPrinter {
        #[param("The found printer's address")]
        printer: String,
    },
    #[effect(view)]
    OpenDashboard,
    #[effect(view)]
    OpenQueue,
    #[effect(view)]
    OpenHistory,
    #[effect(view)]
    RunDiagnostics,
    #[effect(view)]
    OpenNeeds,
    #[effect(view)]
    PrintDocument,
    #[effect(view)]
    ScanDocument,
    #[effect(destructive)]
    NewScannedDocument,
    #[effect(view)]
    OpenPrinter {
        #[param("The printer's queue name")]
        printer: String,
    },
    #[effect(job)]
    SetDefaultPrinter {
        #[param("The printer's queue name")]
        printer: String,
    },
    #[effect(job)]
    #[effect(label = "print-centre-pause-printer")]
    PausePrinter {
        #[param("The printer's queue name")]
        printer: String,
    },
    #[effect(job)]
    #[effect(label = "print-centre-resume-printer")]
    ResumePrinter {
        #[param("The printer's queue name")]
        printer: String,
    },
    #[effect(job)]
    CancelDocument {
        #[param("The print job's id")]
        job: String,
    },
    #[effect(job)]
    #[effect(label = "print-centre-print-again")]
    RetryDocument {
        #[param("The print job's id")]
        job: String,
    },
    #[effect(view, audience = "agents")]
    ConfirmCancel {
        #[param("The print job's id")]
        job: String,
    },
    #[effect(view)]
    ChoosePrinter {
        #[param("The printer's queue name")]
        printer: String,
    },
    #[effect(view)]
    #[effect(label = "print-centre-previous-page")]
    PreviousPrintPage,
    #[effect(view)]
    #[effect(label = "print-centre-next-page")]
    NextPrintPage,
    #[effect(view)]
    SetPrintOption {
        #[param("The option's name")]
        option: String,
        #[param("The value the printer reported")]
        value: String,
    },
    #[effect(job)]
    SendToPrinter,
    #[effect(view)]
    ClosePrintSetup,
    #[effect(view)]
    ScanWith {
        #[param("The scanner's device name")]
        scanner: String,
    },
    #[effect(job)]
    #[effect(label = "print-centre-scan-page")]
    ScanPage,
    #[effect(view)]
    #[effect(label = "print-centre-scan-another")]
    ScanAnother,
    #[effect(view)]
    PreviousScanPage,
    #[effect(view)]
    NextScanPage,
    MoveScanPageEarlier,
    MoveScanPageLater,
    RemoveScanPage,
    RotateScanPage,
    CropScanPage,
    #[effect(job)]
    RecognizeText,
    CopyRecognizedText,
    #[effect(attended, job)]
    SaveScannedPage,
    #[effect(attended, job)]
    SaveScannedDocument,
}

pub struct PrintCentreShell;

impl ResourceApp for PrintCentreShell {
    type Resource = PrintDefaults;
    const ID: &'static str = "so.tilo.print-centre";
    const TITLE: &'static str = "Print Centre";
    const TITLE_KEY: &'static str = "print-centre-app-title";

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
        crate::surface::window(cx)
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

    /// A made-up studio printer, its documents and a scanner, instead of asking the machine
    /// the screenshot is taken on.
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

ui::reach_tests!(PrintCentreShell);

#[cfg(test)]
mod agent_disclosure_tests {
    #[test]
    fn every_scenario_shows_an_agent_its_actions() {
        if let Err(failures) =
            ui::framework::testing::check_agents_are_shown_every_action::<super::PrintCentreShell>()
        {
            panic!("{failures}");
        }
    }
}

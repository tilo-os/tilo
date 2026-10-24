//! Camera's controls, as slots in the window's bar ([`ui::window_toolbar`]):
//! Photo, Video, Codes and Documents as its views; Open Photos and Camera
//! settings as its actions; on the documents, the selected page as the
//! location and what can be done with it as actions. Each runs the published
//! action its control on the canvas ran. The capture settings stay over the
//! picture.

use ui::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar,
};

use super::CameraApp;
use crate::actions::{CameraEffect, DOCUMENT_ROW};
use crate::model::{CaptureMode, Screen};

pub(crate) const PHOTOS: &str = "camera.open-photos";
pub(crate) const SETTINGS: &str = "camera.settings";

/// The views, in the order the bar shows them; the index is
/// [`CameraApp::selected_capture_mode`]'s.
pub(crate) const VIEWS: [(&str, &str); 4] = [
    ("camera.view.photo", "Photo"),
    ("camera.view.video", "Video"),
    ("camera.view.codes", "Codes"),
    ("camera.view.documents", "Documents"),
];

/// What can be done with the selected page: the bar action's id, icon and
/// name, and the published effect it runs.
const PAGE_ACTIONS: [(&str, &str, &str, CameraEffect); 8] = [
    (
        "camera.document.export",
        "download",
        "Save as PDF",
        CameraEffect::ExportPdf,
    ),
    (
        "camera.document.recognize",
        "file-text",
        "Recognize text",
        CameraEffect::RecognizeText,
    ),
    (
        "camera.document.rotate",
        "rotate-cw",
        "Rotate",
        CameraEffect::RotatePage,
    ),
    (
        "camera.document.crop",
        "scissors",
        "Crop",
        CameraEffect::CropPage,
    ),
    (
        "camera.document.move-earlier",
        "chevron-left",
        "Move page earlier",
        CameraEffect::MovePageEarlier,
    ),
    (
        "camera.document.move-later",
        "chevron-right",
        "Move page later",
        CameraEffect::MovePageLater,
    ),
    (
        "camera.document.copy-text",
        "copy",
        "Copy text",
        CameraEffect::CopyText,
    ),
    (
        "camera.document.remove",
        "trash-2",
        "Remove page",
        CameraEffect::RemovePage,
    ),
];

impl CameraApp {
    /// The window bar's slots for what the window shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = WindowToolbar::default();
        if self.base_screen() == Screen::Document && !self.document_scan.pages.is_empty() {
            toolbar.location = Some(ToolbarLocation {
                label: format!(
                    "Page {} of {}",
                    self.document_scan.selected + 1,
                    self.document_scan.pages.len()
                ),
                ..ToolbarLocation::default()
            });
        }
        // Under the settings sheet only the sheet can be operated.
        if self.screen == Screen::Settings {
            return toolbar;
        }
        if !self.is_recording() && !self.is_counting_down() {
            toolbar.views = Some(ToolbarViews {
                name: String::from("Mode"),
                choices: VIEWS
                    .iter()
                    .map(|(id, label)| ToolbarView {
                        id: (*id).to_owned(),
                        label: (*label).to_owned(),
                    })
                    .collect(),
                chosen: VIEWS
                    .get(self.selected_capture_mode())
                    .map_or_else(String::new, |(id, _)| (*id).to_owned()),
            });
        }
        toolbar.actions = self.page_actions();
        toolbar.actions.extend([
            ToolbarAction::new(PHOTOS, "image", "Open Photos"),
            ToolbarAction::new(SETTINGS, "settings", "Camera settings"),
        ]);
        toolbar
    }

    /// The selected page's actions, while the documents show one and no
    /// crop is in progress; each is available when its effect is.
    fn page_actions(&self) -> Vec<ToolbarAction> {
        let pages = self.document_scan.pages.len();
        if self.base_screen() != Screen::Document
            || pages == 0
            || self.document_crop_editor.is_some()
        {
            return Vec::new();
        }
        let selected = self.document_scan.selected;
        let free = self.document_job.is_none();
        let has_text = self
            .document_scan
            .pages
            .get(selected)
            .is_some_and(|page| page.ocr.is_some());
        PAGE_ACTIONS
            .iter()
            .filter(|(_, _, _, effect)| *effect != CameraEffect::CopyText || has_text)
            .map(|(id, icon, name, effect)| {
                let available = match effect {
                    CameraEffect::MovePageEarlier => selected > 0,
                    CameraEffect::MovePageLater => selected + 1 < pages,
                    _ => true,
                };
                ToolbarAction::new(*id, *icon, *name).enabled(free && available)
            })
            .collect()
    }

    /// Run what the person did in the window's bar through the published
    /// action its control on the canvas ran. Returns whether anything was
    /// asked.
    pub(super) fn take_window_toolbar_events(&mut self) -> bool {
        let mut asked = false;
        for event in ui::window_toolbar::take_events() {
            asked = true;
            let ToolbarEvent::Action { id } = event else {
                continue;
            };
            match id.as_str() {
                PHOTOS => self.open_photos(),
                SETTINGS => self.invoke(&CameraEffect::OpenSettings),
                "camera.view.photo" | "camera.view.video" => {
                    let mode = if id == "camera.view.video" {
                        CaptureMode::Video
                    } else {
                        CaptureMode::Photo
                    };
                    self.invoke(&CameraEffect::UseMode {
                        mode: mode.as_str().to_owned(),
                    });
                }
                "camera.view.codes" => self.invoke(&CameraEffect::OpenScanner),
                "camera.view.documents" => self.invoke(&CameraEffect::OpenDocument),
                page => {
                    if let Some((_, _, _, effect)) =
                        PAGE_ACTIONS.iter().find(|(action, ..)| *action == page)
                    {
                        self.invoke_on(DOCUMENT_ROW, effect);
                    }
                }
            }
        }
        asked
    }

    /// Run `effect` on Camera itself, as the palette and agents do.
    fn invoke(&mut self, effect: &CameraEffect) {
        match &self.dispatcher {
            Some(dispatcher) => dispatcher.effect(effect),
            None => self.toast.show("Camera is not open on a record".into()),
        }
    }

    pub(super) fn declare_window_toolbar(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::testing::TestApp;

    struct SampleCamera;

    impl ui::framework::ResourceApp for SampleCamera {
        type Resource = crate::record::Record;
        const ID: &'static str = "so.tilo.camera";
        const TITLE: &'static str = "Camera";

        fn view(cx: &mut ui::framework::ViewCx<crate::record::Record>) -> ui::widget::Element {
            cx.surface(CameraApp::sample())
        }
    }

    fn opened() -> TestApp<SampleCamera> {
        let mut app = TestApp::<SampleCamera>::new().expect("Camera opens");
        app.harness().present_in_window_bar();
        app.settle();
        app
    }

    fn bar() -> WindowToolbar {
        ui::window_toolbar::declared().expect("the window declares its slots")
    }

    fn action_ids() -> Vec<String> {
        bar().actions.into_iter().map(|action| action.id).collect()
    }

    fn press(app: &mut TestApp<SampleCamera>, id: &str) {
        app.harness()
            .press_window_slot(ToolbarEvent::Action { id: id.into() });
        app.settle();
    }

    fn camera<T>(app: &mut TestApp<SampleCamera>, read: impl FnOnce(&mut CameraApp) -> T) -> T {
        app.with_surface::<CameraApp, _>(read)
            .expect("the camera surface")
    }

    /// Every name and action target the canvas draws.
    fn drawn(app: &mut TestApp<SampleCamera>) -> Vec<String> {
        let tree = camera(app, |camera| {
            ui::Widget::accessibility(camera).expect("the camera's semantics")
        });
        tree.children
            .iter()
            .flat_map(|node| node.name.iter().chain(node.action_target.iter()).cloned())
            .collect()
    }

    /// The modes are the bar's views and Photos and the settings its
    /// actions, each running Camera's published action; the canvas draws
    /// none of them.
    #[test]
    fn the_modes_photos_and_the_settings_are_the_window_bars() {
        let mut app = opened();
        let views = bar().views.expect("the modes are the bar's views");
        let ids: Vec<&str> = views.choices.iter().map(|view| view.id.as_str()).collect();
        assert_eq!(ids, VIEWS.map(|(id, _)| id));
        assert_eq!(views.chosen, "camera.view.photo");
        assert_eq!(action_ids(), [PHOTOS, SETTINGS]);
        let canvas = drawn(&mut app);
        for own in [
            "camera.mode:photo",
            "camera.mode:video",
            "camera.nav:scanner",
            "camera.nav:document",
            "camera.nav:settings",
            PHOTOS,
        ] {
            assert!(
                !canvas.iter().any(|label| label == own),
                "the canvas draws no {own} of its own: {canvas:?}"
            );
        }

        press(&mut app, "camera.view.video");
        assert_eq!(camera(&mut app, |camera| camera.mode), CaptureMode::Video);
        assert_eq!(bar().views.unwrap().chosen, "camera.view.video");
        press(&mut app, "camera.view.codes");
        assert_eq!(camera(&mut app, |camera| camera.screen), Screen::Scanner);
        press(&mut app, "camera.view.documents");
        assert_eq!(camera(&mut app, |camera| camera.screen), Screen::Document);
        press(&mut app, "camera.view.photo");
        assert_eq!(
            camera(&mut app, |camera| (camera.screen, camera.mode)),
            (Screen::Camera, CaptureMode::Photo)
        );

        press(&mut app, SETTINGS);
        assert_eq!(camera(&mut app, |camera| camera.screen), Screen::Settings);
        assert_eq!(
            ui::window_toolbar::declared(),
            None,
            "under the settings sheet only the sheet can be operated"
        );
    }

    /// On the documents the selected page is the bar's location and what
    /// can be done with it its actions, each running the page's published
    /// effect; the canvas draws neither.
    #[test]
    fn the_selected_page_is_the_window_bars_location_and_actions() {
        let mut app = opened();
        press(&mut app, "camera.view.documents");
        assert_eq!(bar().location, None, "no page yet");
        assert_eq!(action_ids(), [PHOTOS, SETTINGS]);

        camera(&mut app, CameraApp::capture_document_page);
        app.settle();
        assert_eq!(
            bar().location.map(|location| location.label).as_deref(),
            Some("Page 1 of 1")
        );
        let actions = bar().actions;
        let ids: Vec<&str> = actions.iter().map(|action| action.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "camera.document.export",
                "camera.document.recognize",
                "camera.document.rotate",
                "camera.document.crop",
                "camera.document.move-earlier",
                "camera.document.move-later",
                "camera.document.remove",
                PHOTOS,
                SETTINGS,
            ]
        );
        let enabled = |id: &str| {
            actions
                .iter()
                .find(|action| action.id == id)
                .is_some_and(|action| action.enabled)
        };
        assert!(enabled("camera.document.rotate"));
        assert!(!enabled("camera.document.move-earlier"), "one page only");
        assert!(!enabled("camera.document.move-later"), "one page only");
        let canvas = drawn(&mut app);
        for own in [
            "Page 1 of 1",
            "Rotate selected page clockwise",
            "Remove selected page",
            "Save searchable PDF",
        ] {
            assert!(
                !canvas.iter().any(|label| label == own),
                "the canvas draws no {own} of its own: {canvas:?}"
            );
        }

        let size = |app: &mut TestApp<SampleCamera>| {
            camera(app, |camera| {
                let page = camera.document_scan.pages[0].rendered();
                (page.width(), page.height())
            })
        };
        let (width, height) = size(&mut app);
        press(&mut app, "camera.document.rotate");
        assert_eq!(size(&mut app), (height, width), "Rotate turns the page");

        press(&mut app, "camera.document.crop");
        assert!(camera(&mut app, |camera| camera
            .document_crop_editor
            .is_some()));
        assert!(
            !action_ids()
                .iter()
                .any(|id| id.starts_with("camera.document.")),
            "the crop in progress keeps its own Cancel and Apply"
        );
        camera(&mut app, CameraApp::cancel_document_crop);
        app.settle();

        press(&mut app, "camera.document.remove");
        assert!(camera(&mut app, |camera| camera
            .document_scan
            .pages
            .is_empty()));
        assert_eq!(bar().location, None);
    }

    /// Press the notice's action `index` in a bar `width` wide, by pointer
    /// or through its semantic action.
    fn press_notice(app: &mut TestApp<SampleCamera>, width: f64, semantic: bool) {
        let harness = app.harness();
        let pressed = if semantic {
            harness.invoke_window_notice_action(width, 0)
        } else {
            harness.click_window_notice_action(width, 0)
        };
        pressed.expect("the notice's action is in the bar");
        app.settle();
    }

    /// The labels of the notice's buttons in a bar `width` wide.
    fn notice_buttons(app: &mut TestApp<SampleCamera>, width: f64) -> Vec<String> {
        app.harness()
            .window_bar_notice(width)
            .expect("the notice is up")
            .buttons
            .into_iter()
            .map(|button| button.name)
            .collect()
    }

    const WAYS: [(f64, bool); 4] = [
        (1280.0, false),
        (1280.0, true),
        (420.0, false),
        (420.0, true),
    ];

    /// A scan that failed asks to try again, and carries Try again: pressed
    /// by pointer or through its semantic action, wide or compact, it scans
    /// the page again.
    #[test]
    fn a_failed_scans_try_again_scans_the_page_again() {
        for (width, semantic) in WAYS {
            let mut app = opened();
            camera(&mut app, |camera| {
                camera.show_screen(Screen::Document);
                camera.show_with_action(
                    crate::document_scan::capture_error_message("Device busy").into(),
                    super::super::notice::NoticeAction::ScanAgain,
                );
            });
            app.settle();
            assert_eq!(notice_buttons(&mut app, width), ["Try again"]);
            let before = camera(&mut app, |camera| camera.document_scan.pages.len());
            press_notice(&mut app, width, semantic);
            assert_eq!(
                camera(&mut app, |camera| camera.document_scan.pages.len()),
                before + 1,
                "Try again scanned a page ({width}, semantic: {semantic})"
            );
        }
    }

    /// Text recognition that could not start carries Try again, which
    /// starts it again.
    #[test]
    fn recognitions_try_again_starts_it_again() {
        for (width, semantic) in WAYS {
            let mut app = opened();
            camera(&mut app, |camera| {
                camera.show_screen(Screen::Document);
                camera.capture_document_page();
                camera.show_with_action(
                    "Text recognition could not start. Try again in a moment.".into(),
                    super::super::notice::NoticeAction::RecognizeAgain,
                );
            });
            app.settle();
            assert_eq!(notice_buttons(&mut app, width), ["Try again"]);
            press_notice(&mut app, width, semantic);
            assert!(
                camera(&mut app, |camera| camera.document_job.is_some()),
                "recognition started again ({width}, semantic: {semantic})"
            );
        }
    }

    /// Finding no camera asks to refresh, and carries Refresh, which looks
    /// for cameras again.
    #[test]
    fn no_cameras_refresh_looks_again() {
        for (width, semantic) in WAYS {
            let mut app = opened();
            camera(&mut app, |camera| {
                camera.retry_camera();
                camera.camera_issue = Some(super::super::CameraIssue::StartFailed);
            });
            app.settle();
            assert_eq!(notice_buttons(&mut app, width), ["Refresh"]);
            press_notice(&mut app, width, semantic);
            assert_eq!(
                camera(&mut app, |camera| camera.camera_issue),
                None,
                "Refresh looked for cameras again ({width}, semantic: {semantic})"
            );
        }
    }
}

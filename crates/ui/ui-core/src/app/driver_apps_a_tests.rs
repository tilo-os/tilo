//! What the display-less driver answers about a window's bar and its text that no caller in a
//! window needs: the notice the bar shows and the buttons that run it, and the browser fonts a
//! host loads for text the app shaped before they arrived.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::cell::RefCell;
use std::rc::Rc;

use crate::app::driver::Driver;
use crate::permissions::MemberId;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget as _;
use crate::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarNotice, WindowToolbar};

/// An app whose window's bar carries a notice with one button, and that hears what a press on it
/// runs.
struct NoticeApp {
    content: crate::widget::Column,
    heard: Rc<RefCell<Vec<ToolbarEvent>>>,
    notice: bool,
}

impl crate::widget::Decorator for NoticeApp {
    type Inner = crate::widget::Column;

    fn inner(&self) -> &Self::Inner {
        &self.content
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.content
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        // A notice's button is its poster's, never one of the app's part events: the app polls
        // for its press as the save status does.
        if crate::window_toolbar::take_notice_press("try-again") {
            self.heard.borrow_mut().push(ToolbarEvent::Action {
                id: "try-again".into(),
            });
        }
        for event in crate::window_toolbar::take_events() {
            self.heard.borrow_mut().push(event);
        }
        crate::window_toolbar::declare(Some(WindowToolbar::default()));
        crate::window_toolbar::notify(self.notice.then(|| {
            ToolbarNotice::new("Saving failed", crate::widget::toast::ToastKind::Error)
                .action(ToolbarAction::new("try-again", "refresh", "Try again"))
        }));
        self.content.sync(state, path);
    }
}

fn notice_app() -> (Driver, Rc<RefCell<Vec<ToolbarEvent>>>) {
    mounted(true)
}

fn mounted(notice: bool) -> (Driver, Rc<RefCell<Vec<ToolbarEvent>>>) {
    let heard = Rc::new(RefCell::new(Vec::new()));
    let app = NoticeApp {
        content: crate::widget::Column::new()
            .push(Box::new(crate::widget::Button::new("send", "Send"))),
        heard: Rc::clone(&heard),
        notice,
    };
    let mut driver = Driver::for_root(
        crate::State::new(),
        app,
        (900, 600),
        Theme::dark().with_reduce_motion(true),
    );
    driver.present_in_window_bar();
    driver.relayout();
    (driver, heard)
}

/// The notice's text and its buttons are what the bar shows across a wide window and a narrow
/// one, and both a reader's action and a press on a button reach the app.
#[test]
fn the_windows_notice_and_its_buttons_are_read_and_run_at_any_width() {
    let (mut driver, heard) = notice_app();
    for width in [1280.0, 420.0] {
        let notice = driver
            .window_bar_notice(width)
            .unwrap_or_else(|| panic!("the notice is up at {width} px"));
        assert_eq!(notice.text, "Saving failed");
        let names: Vec<String> = notice.buttons.into_iter().map(|b| b.name).collect();
        assert_eq!(names, ["Try again"], "at {width} px");
    }
    driver
        .invoke_window_notice_action(1280.0, 0)
        .expect("the notice's button runs from its semantic action");
    assert_eq!(
        heard.borrow().as_slice(),
        [ToolbarEvent::Action {
            id: "try-again".into()
        }]
    );
    driver
        .click_window_notice_action(420.0, 0)
        .expect("the notice's button takes a press where it stands");
    assert_eq!(heard.borrow().len(), 2, "the press ran the same action");
}

/// With no notice up, the bar shows none, and its button is nothing to press.
#[test]
fn a_bar_with_no_notice_reads_none() {
    let (mut driver, _heard) = mounted(false);
    assert!(driver.window_bar_notice(1280.0).is_none());
    assert!(driver.invoke_window_notice_action(1280.0, 0).is_err());
    assert!(driver.click_window_notice_action(1280.0, 0).is_err());
}

/// A font the first shaping pass had to skip is installed late: the next frame lays out again,
/// and the document and keyboard focus do not move.
#[test]
fn a_late_browser_font_lays_out_again_without_moving_the_document_or_focus() {
    let root = crate::font_catalog::checkout_root();
    let catalog = crate::font_catalog::FontCatalog::product("", "en-US");
    let fonts = catalog
        .load_browser_startup_bytes(crate::font_catalog::test_font_files(&root))
        .unwrap();
    let mut driver = Driver::new_with_controlled_inputs(
        crate::State::new(),
        Box::new(crate::widget::Text::new("مَرْحَبًا")),
        (400, 200),
        Theme::dark(),
        TextRenderer::with_browser_font_system(fonts),
        MemberId::person("font-test"),
    );
    assert_eq!(
        driver.take_browser_font_demands(),
        ["NotoSansArabic[wght].ttf"]
    );
    let heads = driver.state_mut().doc.get_heads();
    let focused = driver.focused();
    let frames = driver.frames();
    let bytes = std::fs::read(root.join("NotoSansArabic[wght].ttf")).unwrap();
    assert!(
        driver
            .install_browser_font(
                "NotoSansArabic[wght].ttf",
                bytes.clone(),
                &crate::font_catalog::test_sha256(&bytes)
            )
            .unwrap(),
        "the font is installed"
    );
    assert_eq!(
        driver.frames(),
        frames,
        "installing a font draws nothing by itself"
    );
    driver.relayout();
    assert_eq!(driver.frames(), frames + 1, "the next frame lays out");
    assert_eq!(driver.state_mut().doc.get_heads(), heads);
    assert_eq!(driver.focused(), focused);
}

//! Images go into a note the way a person puts them there, in the
//! framework-hosted app: Ctrl+V after copying one elsewhere, or dropping an
//! image file on the window. Each becomes an image block whose bytes the
//! notebook keeps, a screenshot larger than an asset included.

use notes::actions::create_page_action;
use notes::read;
use notes::{NotesShell, PageId};
use ui::clipboard::ClipboardImage;
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;

fn note() -> (TestApp<NotesShell>, PageId) {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    let page = PageId::new_v4();
    app.invoke(Invoke::apply(&create_page_action(page, None, "First")).unwrap())
        .unwrap();
    let rect = app.harness().targets()["block_editor"];
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    app.harness().type_text("above");
    (app, page)
}

fn png(width: u32, height: u32, noisy: bool) -> Vec<u8> {
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        if noisy {
            let v = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503);
            let [r, g, b, _] = v.to_le_bytes();
            image::Rgb([r, g, b])
        } else {
            image::Rgb([200, 40, 40])
        }
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

/// The note's image blocks, with the bytes the notebook keeps for each.
fn images(app: &TestApp<NotesShell>, page: PageId) -> Vec<(String, Vec<u8>)> {
    let store = app.content();
    app.read_document(|doc| {
        let assets =
            notes::notebook::image_snapshot(&notes::read::notebook(doc).images, |digest| {
                store.read(digest)
            });
        read::page_block_snapshots(doc, page)
            .into_iter()
            .filter_map(|(_, text)| {
                let source = text.split("](").nth(1)?.trim_end_matches(')').to_owned();
                source.starts_with("asset:").then_some(())?;
                Some((text, assets.get(&source).cloned().unwrap_or_default()))
            })
            .collect()
    })
}

fn paste(app: &mut TestApp<NotesShell>) {
    app.harness().set_modifiers(&["ctrl".into()]);
    app.harness().key("v");
    app.harness().set_modifiers(&[]);
}

#[test]
fn ctrl_v_puts_a_copied_image_in_the_note() {
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let (mut app, page) = note();
    let screenshot = png(8, 6, false);
    recorder.copy_image_elsewhere(ClipboardImage {
        mime: "image/png",
        bytes: screenshot.clone(),
    });
    paste(&mut app);
    let pasted = images(&app, page);
    assert_eq!(pasted.len(), 1, "{pasted:?}");
    assert!(
        pasted[0].0.starts_with("![Pasted image](asset:"),
        "{}",
        pasted[0].0
    );
    assert_eq!(
        pasted[0].1, screenshot,
        "the notebook keeps the image as copied"
    );
    assert!(app.undo(), "pasting is undoable");
    assert!(images(&app, page).is_empty());
}

/// A pasted screenshot is drawn, not left as an empty plate: the image block
/// is laid out as soon as it exists, and the store must not keep that first
/// look (before the notebook's bytes reached the snapshot) as a failure.
#[test]
fn a_pasted_screenshot_decodes_for_drawing() {
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let (mut app, page) = note();
    recorder.copy_image_elsewhere(ClipboardImage {
        mime: "image/png",
        bytes: png(64, 48, false),
    });
    paste(&mut app);
    let pasted = images(&app, page);
    let source = pasted[0]
        .0
        .split("](")
        .nth(1)
        .unwrap()
        .trim_end_matches(')')
        .to_owned();
    for _ in 0..3 {
        app.harness().relayout();
    }
    let drawn =
        crate::framework_support::surface(&mut app, |notes| notes.image_decoded_for_test(&source));
    assert!(drawn, "{source} never decoded");
}

/// The pasted picture appears by itself: once its decode settles off the UI
/// thread, the window schedules a frame and that frame paints the picture,
/// with no key, click or relayout from anyone. A plate that waits for the
/// next input to show its picture is the grey box a person sees.
#[test]
fn a_pasted_screenshot_is_painted_once_it_decodes_without_further_input() {
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let (mut app, page) = note();
    recorder.copy_image_elsewhere(ClipboardImage {
        mime: "image/png",
        bytes: png(64, 48, false),
    });
    paste(&mut app);
    let source = images(&app, page)[0]
        .0
        .split("](")
        .nth(1)
        .unwrap()
        .trim_end_matches(')')
        .to_owned();
    // Pictures the window's frame paints: draws with vello's image tag
    // (`vello_encoding::DrawTag::IMAGE`). The empty plate paints none.
    let pictures = |app: &mut TestApp<NotesShell>| {
        app.harness()
            .frame_render_input()
            .scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|tag| tag.0 == 0x28C)
            .count()
    };
    // Wait for the decode without laying anything out: `surface_mut` runs
    // no frame, unlike `surface`.
    assert!(
        app.surface_mut::<notes::NotesApp>()
            .image_decoded_for_test(&source),
        "{source} never decoded"
    );
    // The decode may settle while the paste's own frames still run, which
    // then paint the picture; when it settles after them, its completion
    // must schedule the frame that does.
    if pictures(&mut app) == 0 {
        let schedule = app.harness().frame_schedule();
        assert!(
            schedule.needs_redraw || schedule.needs_animation_frame,
            "the picture decoded but no frame is due to show it: {:?}",
            app.harness().redraw_sources()
        );
        assert!(app.harness().poll_frame().unwrap(), "no frame was drawn");
    }
    assert_eq!(
        pictures(&mut app),
        1,
        "the frame after the decode still paints the empty plate"
    );
}

#[test]
fn a_screenshot_larger_than_an_asset_is_kept_smaller() {
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let (mut app, page) = note();
    let screenshot = png(1400, 900, true);
    assert!(screenshot.len() > notes::notebook::MAX_ASSET_BYTES);
    recorder.copy_image_elsewhere(ClipboardImage {
        mime: "image/png",
        bytes: screenshot,
    });
    paste(&mut app);
    let pasted = images(&app, page);
    assert_eq!(pasted.len(), 1);
    let kept = &pasted[0].1;
    assert!(!kept.is_empty() && kept.len() <= notes::notebook::MAX_ASSET_BYTES);
    assert!(image::load_from_memory(kept).is_ok(), "still an image");
}

#[test]
fn a_dropped_image_file_goes_into_the_note() {
    let (mut app, page) = note();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("diagram.png");
    let bytes = png(5, 5, false);
    std::fs::write(&path, &bytes).unwrap();
    app.harness()
        .dispatch_to_root(&ui::winit::event::WindowEvent::DroppedFile(path));
    let dropped = images(&app, page);
    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert!(
        dropped[0].0.starts_with("![diagram](asset:"),
        "{}",
        dropped[0].0
    );
    assert_eq!(dropped[0].1, bytes);
}

//! The notebook this app writes, saved for the tilolang Notes to convert once it opens a
//! workspace this app wrote: the documents of a notebook made with this app's own actions, its
//! record and every part of its notes, as this device keeps them.
//!
//! The fixture is `crates/platform/tilolang-tilo/tests/legacy/rust_notes.hex`, one document a
//! line: its document id, a tab, the saved Automerge document as hex. Write it again with
//!
//! ```text
//! t exec --fetch crates/platform/tilolang-tilo/tests/legacy/rust_notes.hex -- \
//!     cargo test --locked -p notes --test it rust_notebook_fixture -- --ignored
//! ```

use std::fmt::Write as _;

use notes::actions::{
    create_page_action, new_tag_action, place_image_action, set_tag_assignment_action,
    write_body_action,
};
use notes::{MainPanel, PageId};
use ui::tilo_resource::Invoke;

use crate::framework_support::{apply, notes, parts};

/// Where the fixture is written, from this crate.
const FIXTURE: &str = "../../crates/platform/tilolang-tilo/tests/legacy/rust_notes.hex";

/// A small PNG for the picture a note shows.
fn png() -> Vec<u8> {
    let image = image::RgbImage::from_fn(8, 6, |x, y| {
        image::Rgb([
            u8::try_from(x.saturating_mul(30)).unwrap(),
            u8::try_from(y.saturating_mul(40)).unwrap(),
            90,
        ])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[test]
#[ignore = "writes the fixture the tilolang Notes converts; run it by name to write it again"]
fn rust_notebook_fixture() {
    let mut app = notes();
    // Fixed ids, so the test reading the fixture names each page.
    let id = |n: u128| PageId::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0000 | n);
    let handbook = id(1);
    let setup = id(2);
    let laptop = id(3);
    let security = id(4);
    let inbox = id(5);
    let work = id(0x10);
    let urgent = id(0x11);

    apply(&mut app, &create_page_action(handbook, None, "Handbook"));
    apply(&mut app, &create_page_action(inbox, None, "Inbox"));
    apply(
        &mut app,
        &create_page_action(setup, Some(handbook), "Setup"),
    );
    apply(
        &mut app,
        &create_page_action(security, Some(handbook), "Security"),
    );
    apply(&mut app, &create_page_action(laptop, Some(setup), "Laptop"));

    let body = |app: &mut _, page: PageId, markdown: &str| {
        let blocks = MainPanel::seed_blocks(markdown, |title| (title == "Inbox").then_some(inbox));
        apply(app, &write_body_action(page, &blocks));
    };
    body(
        &mut app,
        handbook,
        "## Getting started\n\nRead the [guide](https://tilo.sh/guide) first.\n\n\
         - Ask questions\n- Write things down\n\n\
         - [ ] Get your laptop\n- [x] Join the team workspace\n\n\
         | Name | Role |\n| --- | --- |\n| Ada | Lead |\n",
    );
    body(&mut app, setup, "### Accounts\n\n1. Mail\n2. Chat\n");
    body(
        &mut app,
        laptop,
        "Pick one up at the desk. See [[Inbox]].\n",
    );
    body(&mut app, security, "Keep your key safe.\n");
    body(&mut app, inbox, "Call the dentist\n");
    app.invoke_child::<notes::NoteList>(
        &notes::note_list::chain_resource(),
        Invoke::apply(&place_image_action(laptop, png(), Some("The desk"))).unwrap(),
    )
    .unwrap();

    app.invoke(Invoke::apply(&new_tag_action(work, "work")).unwrap())
        .unwrap();
    app.invoke(Invoke::apply(&new_tag_action(urgent, "urgent")).unwrap())
        .unwrap();
    apply(&mut app, &set_tag_assignment_action(handbook, work, true));
    apply(&mut app, &set_tag_assignment_action(laptop, work, true));
    apply(&mut app, &set_tag_assignment_action(inbox, urgent, true));

    let mut out = String::new();
    let root = app.checkout().id.clone();
    let saved = app.resource_document(&root).expect("the notebook is held");
    let _ = writeln!(out, "{}\t{}", root.as_str(), hex(&saved));
    for (index, _) in parts(&app) {
        let part = notes::note_list::chain().part(index);
        let saved = app.resource_document(&part).expect("the part is held");
        let _ = writeln!(out, "{}\t{}", part.as_str(), hex(&saved));
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    std::fs::write(&path, out).unwrap();
}

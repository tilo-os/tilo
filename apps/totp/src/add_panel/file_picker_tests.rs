//! "Scan a QR image" against the host's chooser: the app asks for the file the person
//! picks and reads it through the descriptor that choice opened.

use std::sync::{Arc, Mutex};

use tilo_dialogs::portal;

use super::tests::{click, opened, panel};

const CANNOT_GIVE: &str = "This device can't give apps the files you choose right now. Sign out and back in, then try again.";

/// A chooser that answers once the test says the person picked.
#[derive(Clone, Default)]
struct LaterChooser {
    answer: Arc<Mutex<Option<std::sync::mpsc::Sender<portal::OpenFileOutcome>>>>,
}

impl tilo_dialogs::FilePicker for LaterChooser {
    fn pick(&self, _: portal::PickRequest) -> portal::PortalChannel {
        refused(portal::PortalOutcome::Failed(CANNOT_GIVE.into()))
    }

    fn pick_multiple(&self, _: portal::PickRequest) -> portal::MultiChannel {
        refused(portal::MultiOutcome::Failed(CANNOT_GIVE.into()))
    }

    fn open_handle(&self, _: portal::PickRequest) -> portal::OpenFileChannel {
        let (answer, channel) = std::sync::mpsc::channel();
        *self.answer.lock().expect("answer") = Some(answer);
        channel
    }

    fn launch(&self, _: portal::LaunchRequest) -> portal::LaunchChannel {
        refused(portal::LaunchOutcome::Failed(CANNOT_GIVE.into()))
    }
}

fn refused<T>(outcome: T) -> std::sync::mpsc::Receiver<T> {
    let (answer, channel) = std::sync::mpsc::channel();
    let _ = answer.send(outcome);
    channel
}

/// A PNG a page reads as its bytes, at a path a native host can open.
fn qr_png(uri: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("totp-setup-qr-{}.png", uuid::Uuid::new_v4()));
    crate::app::tests::render_qr_png(uri, &path);
    path
}

#[test]
fn the_totp_app_asks_for_an_image_and_fills_the_sheet_from_it() {
    let uri = "otpauth://totp/Acme:bob@acme.io?secret=JBSWY3DPEHPK3PXP&issuer=Acme";
    let image = qr_png(uri);
    let chooser = LaterChooser::default();
    let mut services = ui::process::ProcessServices::null();
    services.files = Arc::new(chooser.clone());
    let _host = ui::process::enter(&services);

    let mut app = opened((1280, 900));
    let issuer = panel(&mut app, |panel| panel.values.issuer.clone());
    click(&mut app, "totp-scan");
    let answer = chooser
        .answer
        .lock()
        .expect("answer")
        .take()
        .expect("Scan a QR image opens the chooser");
    answer
        .send(portal::OpenFileOutcome::Opened(portal::OpenedFile::at(
            std::fs::File::open(&image).expect("the rendered QR image"),
            &image,
        )))
        .expect("the app waits for the chooser");
    let filled = app
        .harness()
        .wait_until(ui::time::Duration::from_secs(30), |_| {
            issuer.with(|issuer| issuer.as_str() == "Acme")
        });
    assert!(filled, "the chosen image fills the add form");
    assert_eq!(
        panel(&mut app, |panel| panel.field_snapshot()),
        (
            "Acme".to_owned(),
            "bob@acme.io".to_owned(),
            "JBSWY3DPEHPK3PXP".to_owned()
        )
    );
    assert!(
        panel(&mut app, |panel| panel.is_open()),
        "the add sheet stays open to try another image"
    );
    std::fs::remove_file(&image).ok();
}

#[test]
fn an_image_without_a_readable_code_says_so_over_the_open_sheet() {
    let chooser = LaterChooser::default();
    let mut services = ui::process::ProcessServices::null();
    services.files = Arc::new(chooser.clone());
    let _host = ui::process::enter(&services);

    let mut app = opened((1280, 900));
    click(&mut app, "totp-scan");
    let answer = chooser
        .answer
        .lock()
        .expect("answer")
        .take()
        .expect("Scan a QR image opens the chooser");
    let mut photo = std::io::Cursor::new(Vec::new());
    image::GrayImage::from_pixel(96, 96, image::Luma([255]))
        .write_to(&mut photo, image::ImageFormat::Png)
        .expect("a PNG");
    let path = std::env::temp_dir().join(format!("totp-photo-{}.png", uuid::Uuid::new_v4()));
    std::fs::write(&path, photo.into_inner()).expect("the photograph");
    answer
        .send(portal::OpenFileOutcome::Opened(portal::OpenedFile::at(
            std::fs::File::open(&path).expect("the photograph"),
            &path,
        )))
        .expect("the app waits for the chooser");
    // The image is read off the frame, so how many frames pass before its answer is the
    // machine's speed: wait for the answer rather than count them.
    app.harness()
        .wait_until(ui::time::Duration::from_secs(30), |_| {
            ui::window_toolbar::notice().is_some()
        });

    let notice = ui::window_toolbar::notice().map(|notice| notice.text);
    assert_eq!(
        notice.as_deref(),
        Some("Couldn\u{2019}t read a code from that image"),
        "the person is told the image held no code they can use"
    );
    std::fs::remove_file(&path).ok();
}

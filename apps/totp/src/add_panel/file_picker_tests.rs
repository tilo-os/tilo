//! "Scan a QR image" against the chooser the app reaches on a Tilo device.
//!
//! On a device the launcher hands every app a connection to the session
//! shell's chooser service, and the app's host services answer every chooser
//! through it (`tilo_dialogs::shell`). The shell shows the chooser for an Open
//! that asks for the chosen file's contents; an Open that asks for a path is
//! answered only from what the workspace already granted the app, and the
//! Authenticator is granted nothing. These tests drive the real client of that
//! service over a socket, against a stand-in for the shell that answers as the
//! shell does, so a regression to a path chooser fails here rather than on the
//! device.

#![cfg(unix)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use tilo_dialogs::portal::{self, ContentsOutcome};
use tilo_dialogs::shell::{FilePortalGrant, Reply, Request};

use super::tests::{click, opened, panel};

/// What the session shell answers an app whose workspace granted it no
/// files when it asks for a path (`portal-chooser-nothing-granted`).
const NOTHING_GRANTED: &str = "This app has been given no files to choose from.";

/// The session shell's end of one app's chooser connection: every request
/// it was sent, in order.
#[derive(Clone, Default)]
struct SessionShell {
    requests: Arc<Mutex<Vec<Request>>>,
}

impl SessionShell {
    /// Serve the shell's end of the connection on a thread, answering every
    /// contents chooser with `chosen` (as the person picking `name`) and
    /// refusing every path chooser, as the shell refuses an app granted no
    /// files. Returns the app's end.
    fn serve(&self, name: &str, chosen: Vec<u8>) -> UnixStream {
        let (app, shell) = UnixStream::pair().expect("a socket pair");
        let requests = Arc::clone(&self.requests);
        let name = name.to_owned();
        std::thread::Builder::new()
            .name("totp-test-session-shell".into())
            .spawn(move || answer_all(&shell, &requests, &name, &chosen))
            .expect("the session shell's thread");
        app
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().expect("requests").clone()
    }
}

fn answer_all(shell: &UnixStream, requests: &Mutex<Vec<Request>>, name: &str, chosen: &[u8]) {
    let Ok(reader) = shell.try_clone() else {
        return;
    };
    let mut writer = shell;
    for line in BufReader::new(reader).lines() {
        let Ok(line) = line else { return };
        let Ok(request) = serde_json::from_str::<Request>(&line) else {
            return;
        };
        requests.lock().expect("requests").push(request.clone());
        let reply = answer(request, name, chosen);
        let Ok(mut frame) = serde_json::to_vec(&reply) else {
            return;
        };
        frame.push(b'\n');
        if writer
            .write_all(&frame)
            .and_then(|()| writer.flush())
            .is_err()
        {
            return;
        }
    }
}

fn answer(request: Request, name: &str, chosen: &[u8]) -> Reply {
    match request {
        Request::PickContents { id, .. } => Reply::ContentsGranted {
            id,
            grants: vec![FilePortalGrant {
                request: id,
                token: uuid::Uuid::new_v4().to_string(),
                name: name.to_owned(),
                size: chosen.len() as u64,
            }],
        },
        Request::ReadContents {
            id,
            offset,
            max_bytes,
            ..
        } => {
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(chosen.len());
            let end = start
                .saturating_add(usize::try_from(max_bytes).unwrap_or(usize::MAX))
                .min(chosen.len());
            Reply::ContentsChunk {
                id,
                bytes: chosen.get(start..end).unwrap_or_default().to_vec(),
                eof: end == chosen.len(),
            }
        }
        Request::ReleaseContents { id, .. } => Reply::ContentsReleased { id },
        Request::Pick { id, .. } => Reply::Failed {
            id,
            reason: NOTHING_GRANTED.to_owned(),
        },
        other => Reply::Failed {
            id: request_id(&other),
            reason: "not a chooser request".to_owned(),
        },
    }
}

fn request_id(request: &Request) -> u64 {
    match request {
        Request::Pick { id, .. }
        | Request::PickContents { id, .. }
        | Request::ReadContents { id, .. }
        | Request::ReleaseContents { id, .. }
        | Request::Open { id, .. }
        | Request::OpenUri { id, .. }
        | Request::AwaitIdentity { id }
        | Request::ChooseIdentity { id }
        | Request::ChooseApp { id, .. } => *id,
    }
}

/// A PNG of the QR code that sets up `uri`.
fn qr_png(uri: &str) -> Vec<u8> {
    let path = std::env::temp_dir().join(format!("totp-setup-qr-{}.png", uuid::Uuid::new_v4()));
    crate::app::tests::render_qr_png(uri, &path);
    let png = std::fs::read(&path).expect("the rendered QR image");
    std::fs::remove_file(&path).ok();
    png
}

#[test]
fn the_totp_app_opens_the_file_picker() {
    let uri = "otpauth://totp/Acme:bob@acme.io?secret=JBSWY3DPEHPK3PXP&issuer=Acme";
    let shell = SessionShell::default();
    let link = tilo_dialogs::shell::ShellLink::connect(shell.serve("acme-qr.png", qr_png(uri)))
        .expect("the chooser connection");
    let mut services = ui::host::HostServices::null();
    services.files = link.picker(None);
    let _host = ui::host::enter(&services);

    let mut app = opened((1280, 900));
    let issuer = panel(&mut app, |panel| panel.values.issuer.clone());
    click(&mut app, "totp-scan");
    let filled = app
        .harness()
        .wait_until(ui::time::Duration::from_secs(30), |_| {
            issuer.with(|issuer| issuer.as_str() == "Acme")
        });

    let requests = shell.requests();
    let chooser = requests.iter().find_map(|request| match request {
        Request::PickContents {
            title, multiple, ..
        } => Some((title.clone(), *multiple)),
        _ => None,
    });
    assert_eq!(
        chooser,
        Some(("Choose a QR image".to_owned(), false)),
        "Scan a QR image asks the session shell to show its file chooser: {requests:?}"
    );
    assert!(
        !requests
            .iter()
            .any(|request| matches!(request, Request::Pick { .. })),
        "the app asks for the chosen file's contents, never a path the shell refuses an app \
         granted no files: {requests:?}"
    );
    assert!(filled, "the chosen image fills the add form");
    assert_eq!(
        panel(&mut app, |panel| panel.field_snapshot()),
        (
            "Acme".to_owned(),
            "bob@acme.io".to_owned(),
            "JBSWY3DPEHPK3PXP".to_owned()
        ),
        "the QR code's account is in the form"
    );
}

/// A chooser that answers once the test says the person picked, after the
/// frames the click ran: as a browser page's file input does, which wakes
/// the window and changes nothing else.
#[derive(Clone, Default)]
struct LaterChooser {
    answer: Arc<Mutex<Option<std::sync::mpsc::Sender<ContentsOutcome>>>>,
}

impl tilo_dialogs::FilePicker for LaterChooser {
    fn pick(&self, _: portal::PickRequest) -> portal::PortalChannel {
        refused(portal::PortalOutcome::Failed(NOTHING_GRANTED.into()))
    }

    fn pick_multiple(&self, _: portal::PickRequest) -> portal::MultiChannel {
        refused(portal::MultiOutcome::Failed(NOTHING_GRANTED.into()))
    }

    fn open_file(&self, _: portal::PickRequest) -> portal::OpenFileChannel {
        refused(portal::OpenFileOutcome::Failed(NOTHING_GRANTED.into()))
    }

    fn launch(&self, _: portal::LaunchRequest) -> portal::LaunchChannel {
        refused(portal::LaunchOutcome::Failed(NOTHING_GRANTED.into()))
    }

    fn pick_contents(&self, _: portal::PickRequest) -> portal::ContentsChannel {
        let (answer, channel) = std::sync::mpsc::channel();
        *self.answer.lock().expect("answer") = Some(answer);
        channel
    }
}

fn refused<T>(outcome: T) -> std::sync::mpsc::Receiver<T> {
    let (answer, channel) = std::sync::mpsc::channel();
    let _ = answer.send(outcome);
    channel
}

#[test]
fn an_image_without_a_readable_code_says_so_over_the_open_sheet() {
    let chooser = LaterChooser::default();
    let mut services = ui::host::HostServices::null();
    services.files = Arc::new(chooser.clone());
    let _host = ui::host::enter(&services);

    let mut app = opened((1280, 900));
    click(&mut app, "totp-scan");
    let answer = chooser
        .answer
        .lock()
        .expect("answer")
        .take()
        .expect("Scan a QR image opens the chooser");
    // A photo with no code in it.
    let mut photo = std::io::Cursor::new(Vec::new());
    image::GrayImage::from_pixel(96, 96, image::Luma([255]))
        .write_to(&mut photo, image::ImageFormat::Png)
        .expect("a PNG");
    answer
        .send(ContentsOutcome::Picked(vec![portal::PickedContents {
            name: "photo.png".into(),
            media_type: Some("image/png".into()),
            bytes: photo.into_inner(),
            location: None,
        }]))
        .expect("the app waits for the chooser");
    // Frames as the live window runs them once nothing but the chooser
    // moved: no input, no document change.
    for _ in 0..3 {
        app.harness().idle();
    }

    let notice = ui::window_toolbar::notice().map(|notice| notice.text);
    assert_eq!(
        notice.as_deref(),
        Some("Couldn\u{2019}t read a code from that image"),
        "the person is told the image held no code they can use"
    );
    assert!(
        panel(&mut app, |panel| panel.is_open()),
        "the add sheet stays open to try another image"
    );
}

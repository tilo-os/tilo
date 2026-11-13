//! "Scan a QR image" against the chooser a Tilo device reaches: these drive the
//! real client over a socket, against a stand-in for the shell.

#![cfg(unix)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use tilo_dialogs::portal::{self, ContentsOutcome};
use tilo_dialogs::shell::{FilePortalGrant, Reply, Request};

use super::tests::{click, opened, panel};

const CANNOT_GIVE: &str = "This device can't give apps the files you choose right now. Sign out and back in, then try again.";

/// The shell's end of one app's chooser connection: every request, in order.
#[derive(Clone, Default)]
struct SessionShell {
    requests: Arc<Mutex<Vec<Request>>>,
}

impl SessionShell {
    /// Serve the shell's end on a thread: every contents chooser answers `chosen`,
    /// every path chooser is refused, as it is for an app granted no files.
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
            reason: CANNOT_GIVE.to_owned(),
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
        | Request::ChooseApp { id, .. }
        | Request::AwaitIdentity { id }
        | Request::ChooseIdentity { id } => *id,
    }
}

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
        "the app asks for the chosen file's contents, never a path a shell that cannot give \
         refuses an app granted no files: {requests:?}"
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

/// A chooser that answers once the test says the person picked.
#[derive(Clone, Default)]
struct LaterChooser {
    answer: Arc<Mutex<Option<std::sync::mpsc::Sender<ContentsOutcome>>>>,
}

impl tilo_dialogs::FilePicker for LaterChooser {
    fn pick(&self, _: portal::PickRequest) -> portal::PortalChannel {
        refused(portal::PortalOutcome::Failed(CANNOT_GIVE.into()))
    }

    fn pick_multiple(&self, _: portal::PickRequest) -> portal::MultiChannel {
        refused(portal::MultiOutcome::Failed(CANNOT_GIVE.into()))
    }

    fn open_file(&self, _: portal::PickRequest) -> portal::OpenFileChannel {
        refused(portal::OpenFileOutcome::Failed(CANNOT_GIVE.into()))
    }

    fn launch(&self, _: portal::LaunchRequest) -> portal::LaunchChannel {
        refused(portal::LaunchOutcome::Failed(CANNOT_GIVE.into()))
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
    // Frames as the live window runs them once nothing but the chooser moved.
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

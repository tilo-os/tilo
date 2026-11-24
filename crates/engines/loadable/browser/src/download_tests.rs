use std::path::Path;
use std::sync::mpsc;

use test_http::{Reply, Responder};

use super::{Ask, Move, Stop, run};

/// A download of `url` as a page hands it over, named `name`.
fn ask(url: &str, name: &str) -> Ask {
    Ask {
        url: url.to_owned(),
        name: name.to_owned(),
        ..Ask::default()
    }
}

/// Every move `ask` makes into `folder`, the last one being how it ended.
fn moves(ask: &Ask, folder: &Path) -> Vec<Move> {
    let (_, url) = Ask::read(
        &serde_json::to_string(&serde_json::json!({
            "url": ask.url,
            "name": ask.name,
            "referrer": ask.referrer,
            "cookies": ask.cookies,
            "user_agent": ask.user_agent,
        }))
        .unwrap(),
    )
    .unwrap();
    let mut told = Vec::new();
    let ended = run(ask, &url, folder, &Stop::default(), &mut |moved| {
        told.push(moved.clone());
    });
    assert_eq!(
        told.last(),
        Some(&ended),
        "the move answered is the last one told"
    );
    told
}

/// What is in `folder`, hidden files included, by name.
fn listing(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_download_saves_the_file_and_tells_its_progress_in_steps() {
    let body: Vec<u8> = (0..400_000_u32).map(|n| (n % 251) as u8).collect();
    let server = Responder::start(vec![Reply::status(200).body(body.clone())]);
    let folder = tempfile::tempdir().unwrap();
    let mut asked = ask(
        &format!("{}/files/report.bin", server.url()),
        "Quarterly report.pdf",
    );
    asked.cookies = "session=abc; theme=dark".to_owned();
    asked.referrer = "https://example.com/reports".to_owned();
    asked.user_agent = "Tilo Browser".to_owned();

    let told = moves(&asked, folder.path());

    let request = server.next_request();
    assert_eq!(request.header("cookie"), Some("session=abc; theme=dark"));
    assert_eq!(
        request.header("referer"),
        Some("https://example.com/reports")
    );
    assert_eq!(request.header("user-agent"), Some("Tilo Browser"));
    let saved = folder.path().join("Quarterly report.pdf");
    assert_eq!(
        told.last(),
        Some(&Move::Done {
            path: saved.clone(),
            name: "Quarterly report.pdf".to_owned()
        })
    );
    let detail: serde_json::Value = serde_json::from_str(&told.last().unwrap().detail()).unwrap();
    assert_eq!(
        detail,
        serde_json::json!({"path": saved.to_string_lossy(), "name": "Quarterly report.pdf"}),
        "the job's last word names the file for its `done:`"
    );
    assert_eq!(std::fs::read(&saved).unwrap(), body);
    assert_eq!(
        listing(folder.path()),
        ["Quarterly report.pdf"],
        "no partial file is left"
    );

    let progress: Vec<(u64, u64)> = told
        .iter()
        .filter_map(|moved| match moved {
            Move::Running { done, total } => Some((*done, *total)),
            _ => None,
        })
        .collect();
    assert_eq!(
        progress.first(),
        Some(&(0, 0)),
        "a download is running before the site answers"
    );
    assert_eq!(
        progress.last(),
        Some(&(400_000, 400_000)),
        "the last step is the whole file"
    );
    assert!(
        progress.windows(2).all(|pair| pair[0].0 <= pair[1].0),
        "progress never goes back: {progress:?}"
    );
    assert!(
        progress.len() > 3,
        "a file of many pieces is told in steps: {progress:?}"
    );
    assert!(
        progress.len() <= 103,
        "steps are hundredths, not every piece: {}",
        progress.len()
    );
}

#[test]
fn a_download_of_unknown_size_says_so_until_it_ends() {
    let server = Responder::start(vec![
        Reply::status(200)
            .body(b"plain text".to_vec())
            .without_content_length(),
    ]);
    let folder = tempfile::tempdir().unwrap();

    let told = moves(
        &ask(&format!("{}/notes.txt", server.url()), ""),
        folder.path(),
    );

    let ended = told.last().unwrap();
    assert_eq!(ended.state(), "done");
    let Move::Done { path, name } = ended else {
        panic!("{ended:?}")
    };
    assert_eq!(
        name, "notes.txt",
        "with no suggested name, the address names the file"
    );
    assert_eq!(std::fs::read(path).unwrap(), b"plain text");
    let before_end = &told[told.len() - 2];
    assert_eq!(
        before_end,
        &Move::Running {
            done: 10,
            total: 10
        }
    );
    assert!(
        told[..told.len() - 2]
            .iter()
            .all(|moved| matches!(moved, Move::Running { total: 0, .. })),
        "the size is unknown until the file ends: {told:?}"
    );
}

#[test]
fn a_download_never_replaces_a_file_and_never_leaves_its_folder() {
    let server = Responder::start(vec![
        Reply::status(200)
            .header(
                "Content-Disposition",
                "attachment; filename=\"plain.txt\"; filename*=UTF-8''r%C3%A9sum%C3%A9.txt",
            )
            .body(b"new".to_vec()),
        Reply::status(200).body(b"escaped".to_vec()),
    ]);
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("résumé.txt"), b"old").unwrap();

    let told = moves(&ask(&format!("{}/x", server.url()), ""), folder.path());
    let Some(Move::Done { name, .. }) = told.last() else {
        panic!("{told:?}")
    };
    assert_eq!(
        name, "résumé 2.txt",
        "the server's name, beside the file already there"
    );
    assert_eq!(
        std::fs::read(folder.path().join("résumé.txt")).unwrap(),
        b"old"
    );

    let told = moves(
        &ask(&format!("{}/y", server.url()), "../../.profile"),
        folder.path(),
    );
    let Some(Move::Done { path, name }) = told.last() else {
        panic!("{told:?}")
    };
    assert_eq!(
        name, "profile",
        "a suggested name holds no folder and is not hidden"
    );
    assert_eq!(path.parent(), Some(folder.path()));
}

#[test]
fn a_stopped_download_ends_cancelled_and_removes_what_it_saved() {
    let server = Responder::serve(|peer| {
        let head = "HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n";
        if peer.write(head.as_bytes()) && peer.write(&vec![7_u8; 100_000]) {
            // The rest never comes: the download waits on the network when it is stopped.
            let _ = peer.wait_for_close();
        }
    });
    let folder = tempfile::tempdir().unwrap();
    let asked = ask(&format!("{}/big.iso", server.url()), "big.iso");
    let (_, url) = Ask::read(&format!(r#"{{"url":"{}/big.iso"}}"#, server.url())).unwrap();
    let stop = Stop::default();
    let (sender, receiver) = mpsc::channel();
    let running = {
        let stop = stop.clone();
        let folder = folder.path().to_owned();
        std::thread::spawn(move || {
            run(&asked, &url, &folder, &stop, &mut |m| {
                sender.send(m.clone()).unwrap();
            })
        })
    };
    loop {
        let next = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        if let Move::Running { done, total } = next
            && done > 0
        {
            assert_eq!(total, 1_000_000);
            break;
        }
    }
    assert_eq!(
        listing(folder.path()).len(),
        1,
        "a running download holds its partial file"
    );

    stop.stop();

    assert_eq!(running.join().unwrap(), Move::Cancelled);
    assert!(
        listing(folder.path()).is_empty(),
        "nothing of a stopped download is left"
    );
}

#[test]
fn a_failed_download_says_why_in_words_and_leaves_nothing() {
    let server = Responder::start(vec![
        Reply::status(404),
        Reply::status(403),
        Reply::status(200).body(vec![1_u8; 2000]).cut_short(true),
    ]);
    let folder = tempfile::tempdir().unwrap();
    let base = server.url();

    for (path, said) in [
        ("/gone", "127.0.0.1 no longer has this file (404)"),
        ("/private", "127.0.0.1 refused the download (403)"),
        ("/cut", "The connection to 127.0.0.1 was lost"),
    ] {
        let told = moves(&ask(&format!("{base}{path}"), "file.bin"), folder.path());
        assert_eq!(
            told.last(),
            Some(&Move::Failed {
                error: said.to_owned()
            }),
            "{path}"
        );
        assert!(
            listing(folder.path()).is_empty(),
            "{path} leaves nothing behind"
        );
    }

    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let told = moves(
        &ask(&format!("http://127.0.0.1:{port}/x"), ""),
        folder.path(),
    );
    assert_eq!(
        told.last(),
        Some(&Move::Failed {
            error: "127.0.0.1 could not be reached".to_owned()
        })
    );
}

#[test]
fn only_a_web_address_is_downloaded() {
    for input in [
        r#"{"url":"file:///etc/passwd"}"#,
        r#"{"url":"not an address"}"#,
        "{}",
    ] {
        assert!(Ask::read(input).is_err(), "{input}");
    }
    let (ask, url) = Ask::read(r#"{"url":" https://example.com/a.zip ","name":"a.zip"}"#).unwrap();
    assert_eq!(url.as_str(), "https://example.com/a.zip");
    assert_eq!(ask.name, "a.zip");
}

//! A key's calls while a mail import goes on in the same workspace: Ada's logged-in computer
//! writes mail into the workspace as fast as it reads it in, one new message after another, and
//! her backend's key lists her notes and her folder all the while, through the API
//! server as `tilo ls -w` asks it. Mail's writes are another app's: every listing answers at once,
//! the last as fast as the first, and a folder of more files than a page holds lists whole, never
//! "kept changing" for writes to Mail.

use super::api::ISSUER;
use super::api_on_server::{api_on_server, approved_key};
use super::key::Key;
use super::*;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const MAIL: &str = "so.tilo.mail";

/// More files than one page of a listing holds, so the folder is read over several pages.
const FILES: usize = 300;

/// How often the key lists while the import goes on.
const ROUNDS: usize = 8;

/// What one listing of the key may take while Mail is written: well under a second on the real
/// path, however busy Mail is.
const CALL_BUDGET: Duration = Duration::from_millis(1_000);

/// Every row `path` lists, page after page as `tilo ls` reads them, with what the key was answered
/// when a page was refused.
async fn listed_whole(key: &Key<'_>, path: &str) -> Result<usize, String> {
    let (mut rows, mut cursor) = (0, None::<String>);
    loop {
        let page = match &cursor {
            Some(cursor) => format!("{path}&cursor={cursor}"),
            None => path.to_owned(),
        };
        let (status, listed) = key.get(&page).await;
        if status != 200 {
            return Err(format!("{status} {listed}"));
        }
        rows += listed["rows"].as_array().map_or(0, Vec::len);
        match listed["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => return Ok(rows),
        }
    }
}

/// How long `call` took, and what it answered.
async fn timed<T>(call: impl std::future::Future<Output = T>) -> (Duration, T) {
    let started = Instant::now();
    let answer = call.await;
    (started.elapsed(), answer)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mail_import_beside_never_slows_a_keys_listings() {
    Box::pin(beside_an_import()).await;
}

async fn beside_an_import() {
    let on_server = Box::pin(api_on_server(ISSUER)).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let computer = enrol(&ada, "Ada's computer", DeviceCapabilities::member());
    let token = approved_key(&on_server, &ada, &computer, "Ada's backend").await;
    let key = Key { api: &api, token };
    let (status, made) = key
        .post("/v1/workspaces", &json!({"name": "Company"}), &[])
        .await;
    assert_eq!(status, 201, "{made}");
    let workspace = made["id"].as_str().unwrap().to_owned();
    let base = format!("/v1/workspaces/{workspace}");

    // Her computer holds the workspace, with her notes and her folder.
    let mut page = super::resource_support::open_admitted_peer(
        &on_server.hosted,
        &computer,
        &on_server.host,
        &workspace,
    )
    .await;
    page.create("so.tilo.notes");
    let mut folder = tilo_fs::Namespace::new(hex::encode(computer.cert.device_public_key)).unwrap();
    for index in 0..FILES {
        folder
            .create_file(tilo_fs::ROOT_INO, format!("f{index:03}.txt"), None, 0, 1)
            .unwrap();
    }
    super::put_files::write_tree(&page, &mut folder).unwrap();
    page.durable().await;
    let notes = format!("{base}/resources?path=notes&limit=40");
    let files = format!("{base}/resources?path=files&limit=100");
    let deadline = tilo_test_env::hang_deadline();
    let files_listed = loop {
        let listed = listed_whole(&key, &files).await;
        if let Ok(rows) = listed
            && rows >= FILES
        {
            break rows;
        }
        assert!(
            Instant::now() < deadline,
            "the folder never listed whole: {listed:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    // It imports mail: a new message written into Mail after another, as fast as it can.
    let stop = Arc::new(AtomicBool::new(false));
    let written = Arc::new(AtomicUsize::new(0));
    let import = {
        let (stop, written) = (Arc::clone(&stop), Arc::clone(&written));
        tokio::task::spawn_blocking(move || {
            while !stop.load(Ordering::Relaxed) {
                page.create(MAIL);
                page.pump();
                written.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(5));
            }
            page
        })
    };
    let deadline = tilo_test_env::hang_deadline();
    while written.load(Ordering::Relaxed) < 20 {
        assert!(Instant::now() < deadline, "the import never wrote");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // The key lists beside it.
    let mut rounds: Vec<Vec<(&str, Duration)>> = Vec::with_capacity(ROUNDS);
    let mut during_rounds: Vec<usize> = Vec::with_capacity(ROUNDS);
    for round in 0..ROUNDS {
        let before = written.load(Ordering::Relaxed);
        let mut took = Vec::new();
        for (call, path) in [("notes", &notes)] {
            let (elapsed, (status, listed)) = timed(key.get(path)).await;
            assert_eq!(status, 200, "listing {call} in round {round}: {listed}");
            took.push((call, elapsed));
        }
        // The folder over its pages: each read on from the last one's cursor while Mail moves.
        let (elapsed, listed) = timed(listed_whole(&key, &files)).await;
        assert_eq!(
            listed,
            Ok(files_listed),
            "the folder lists whole beside Mail's writes, in round {round}"
        );
        took.push(("files", elapsed / 3));
        let during = written.load(Ordering::Relaxed) - before;
        eprintln!("round {round}: {took:?}, {during} messages written meanwhile");
        rounds.push(took);
        during_rounds.push(during);
    }
    stop.store(true, Ordering::Relaxed);
    let page = import.await.unwrap();
    page.durable().await;
    let written = written.load(Ordering::Relaxed);
    eprintln!("{written} messages written in all");
    // The import writes one message after another as fast as it can, so how many it wrote in
    // all measures how long the rounds took, not whether it kept up: the same test wrote 90-99
    // across eight rounds on a faster machine and 126 when the rounds ran longer. What the
    // listings were beside is an import that never stopped: every round found it writing.
    for (round, during) in during_rounds.iter().enumerate() {
        assert!(
            *during > 0,
            "the import wrote nothing while round {round} listed: {during_rounds:?}"
        );
    }

    for (round, took) in rounds.iter().enumerate() {
        for (call, elapsed) in took {
            assert!(
                *elapsed < CALL_BUDGET,
                "listing {call} in round {round} took {elapsed:?} beside the import"
            );
        }
    }
    let slowest = |took: &[(&str, Duration)]| took.iter().map(|(_, took)| *took).max().unwrap();
    let (first, last) = (slowest(&rounds[1]), slowest(&rounds[ROUNDS - 1]));
    assert!(
        last <= first * 3 + Duration::from_millis(150),
        "no listing grows slower than the ones before it: {first:?} early, {last:?} at the end"
    );
}

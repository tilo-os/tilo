//! A listing of a tracker's issues over the API costs each page its rows: the same page at two
//! thousand issues costs what it does at a hundred, so an agent listing a company's tracker pays
//! for the rows it reads.
//!
//! The daemon serves the workspace it governs and runs its apps, so a key's listing reads the
//! tracker's records as the app lays them out. A test that opens the tracker before the app runs
//! stops at the app's open.

#![allow(
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "one scenario in order, and the times are printed for reading"
)]

use serde_json::{Value, json};
use std::time::{Duration, Instant};

use crate::language::host;

/// The sizes listed: a small company's tracker, and a big one's.
const SIZES: [usize; 2] = [100, 2_000];

/// The issues one `create-many` makes: a list stays one part's worth, and one
/// action's payload stays under what one action may carry.
const LIST: usize = 100;

/// The page sizes a caller lists with: the CLI's, and the largest a listing
/// answers (a larger `limit` is clamped to it).
const PAGES: [usize; 2] = [40, 200];

/// The most a listing of two thousand issues may take in an unoptimized
/// build on a shared machine: a bound on the whole, not a duration to keep.
const LISTING_CEILING: Duration = Duration::from_secs(90);

/// The account's actions at `base`, as `/v1` answers them.
async fn act(key: &host::Key, base: &str, resource: &str, action: &str, args: &Value) -> Value {
    let (status, reply) = key
        .post(
            &format!("{base}/resources/{resource}/actions/{action}"),
            args,
        )
        .await;
    assert_eq!(status, 200, "{resource}#{action}: {reply}");
    assert_eq!(reply["status"], "ok", "{resource}#{action}: {reply}");
    reply["result"].clone()
}

async fn fill(key: &host::Key, base: &str, size: usize) {
    for start in (0..size).step_by(LIST) {
        let end = (start + LIST).min(size);
        let titles: Vec<Value> = (start..end)
            .map(|at| json!(format!("Issue {at}")))
            .collect();
        act(
            key,
            base,
            "so.tilo.tasks",
            "new_tasks",
            &json!({"titles": titles}),
        )
        .await;
    }
}

/// How many resources `rows` stand for, each once: a row of one task carries its actions with it.
fn resources(rows: &[Value]) -> usize {
    rows.iter()
        .filter_map(|row| row["id"].as_str())
        .collect::<std::collections::HashSet<_>>()
        .len()
}

fn quantile(took: &[Duration], quantile: f64) -> Duration {
    let mut sorted = took.to_vec();
    sorted.sort();
    let at = ((sorted.len() as f64 - 1.0) * quantile).round() as usize;
    sorted.get(at).copied().unwrap_or_default()
}

/// Every row the listing at `path` holds, a page at a time as the cursor each
/// page answers leads on, and how long each page took.
async fn every_page(key: &host::Key, base: &str, limit: usize) -> (Vec<Value>, Vec<Duration>) {
    let mut rows = Vec::new();
    let mut took = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let at = cursor
            .as_deref()
            .map(|cursor| format!("&cursor={cursor}"))
            .unwrap_or_default();
        let began = Instant::now();
        let (status, page) = key
            .get(&format!(
                "{base}/resources?path=so.tilo.tasks/tl:so.tilo.tasks&limit={limit}{at}"
            ))
            .await;
        took.push(began.elapsed());
        assert_eq!(status, 200, "{page}");
        let mut got = page["rows"].as_array().cloned().unwrap_or_default();
        let next = page["next_cursor"].as_str().map(str::to_owned);
        // A page can be short of its limit and still go on: a resource offers
        // many rows, so a page ends where the rows do. One with nothing on it
        // and a cursor would lead nowhere.
        assert!(
            !got.is_empty() || next.is_none(),
            "a page with no rows and a cursor leads nowhere: {page}"
        );
        rows.append(&mut got);
        match next {
            Some(next) => cursor = Some(next),
            None => return (rows, took),
        }
        assert!(took.len() < 10_000, "the cursor leads somewhere");
    }
}

/// A workspace of the person's own, made for them as their computer makes one.
async fn workspace_for(host: &host::Host, name: &str) -> String {
    let person: [u8; 32] = hex::decode(host.person())
        .expect("the person's id is hex")
        .try_into()
        .expect("a root is 32 bytes");
    host.daemon()
        .workspaces()
        .create_for(name, None, person)
        .await
        .unwrap_or_else(|refused| panic!("the workspace {name:?}: {refused}"))
        .workspace
}

/// The person's own tracker in `workspace`, opened and filled with `size` issues; its API base.
async fn tracker(key: &host::Key, workspace: &str, size: usize) -> String {
    let base = format!("/v1/workspaces/{workspace}");
    act(key, &base, "so.tilo.tasks", "open", &json!({})).await;
    fill(key, &base, size).await;
    base
}

#[test]
fn listing_a_tracker_costs_each_page_its_rows_at_any_size() {
    host::run(|host| async move {
        let key = host.key("Mover");
        let mut per_row = Vec::new();
        for size in SIZES {
            let workspace = workspace_for(host.as_ref(), &format!("Company of {size}")).await;
            let base = tracker(&key, &workspace, size).await;
            // The first page builds what the listing reads; every page after it
            // pays its own rows.
            let (status, warm) = key
                .get(&format!(
                    "{base}/resources?path=so.tilo.tasks/tl:so.tilo.tasks&limit=40"
                ))
                .await;
            assert_eq!(status, 200, "{warm}");
            // What one call of the API costs before it reads a row: a listing of
            // the app's own page, whose rows do not grow with the tracker.
            let mut base_took = Vec::new();
            for _ in 0..5 {
                let began = Instant::now();
                let (status, _) = key
                    .get(&format!("{base}/resources?path=so.tilo.tasks&limit=40"))
                    .await;
                base_took.push(began.elapsed());
                assert_eq!(status, 200);
            }
            eprintln!(
                "{size} issues: a call of the app's own page takes p50 {:?}",
                quantile(&base_took, 0.5)
            );
            for limit in PAGES {
                let began = Instant::now();
                let (rows, took) = every_page(&key, &base, limit).await;
                let total = began.elapsed();
                assert_eq!(
                    resources(&rows),
                    size,
                    "every issue is listed once at {size} issues, {limit} a page"
                );
                let (first, last) = (
                    took.first().copied().unwrap_or_default(),
                    took.last().copied().unwrap_or_default(),
                );
                let slowest = took.iter().max().copied().unwrap_or_default();
                let row = total.as_secs_f64() / size as f64;
                eprintln!(
                    "{size} issues, {limit} a page: {} pages in {total:?}; first {first:?}, last \
                     {last:?}, slowest {slowest:?}, p50 {:?}, {row:.5} s per row",
                    took.len(),
                    quantile(&took, 0.5)
                );
                assert!(
                    total < LISTING_CEILING,
                    "listing {size} issues at {limit} a page took {total:?}"
                );
                assert!(
                    last <= first * 4 + Duration::from_millis(100),
                    "the last page at {size} issues took {last:?}, the first {first:?}"
                );
                per_row.push((size, limit, row));
            }
            if size == SIZES[1] {
                // A page answers a thousand rows (the listing's bound): a tracker this size is
                // listed in two calls, and each is still one answer's worth of memory.
                let (status, page) = key
                    .get(&format!(
                        "{base}/resources?path=so.tilo.tasks/tl:so.tilo.tasks&limit=1000"
                    ))
                    .await;
                assert_eq!(status, 200, "{page}");
                assert_eq!(
                    page["rows"].as_array().map(Vec::len),
                    Some(1000),
                    "a page answers a thousand rows: {}",
                    page["rows"].as_array().map(Vec::len).unwrap_or_default()
                );
                assert!(page["next_cursor"].is_string(), "{page}");
            }
        }
        let small = per_row
            .iter()
            .filter(|(size, _, _)| *size == SIZES[0])
            .map(|(_, _, row)| *row)
            .fold(f64::INFINITY, f64::max);
        for (size, limit, row) in &per_row {
            assert!(
                *row <= small * 3.0 + 0.002,
                "a row at {size} issues, {limit} a page costs {row:.5} s, at {} {small:.5} s",
                SIZES[0]
            );
        }
    });
}

/// A tracker held in many parts is listed whole: a listing reads every part it reaches, never
/// ending at the newest one it holds.
#[test]
fn a_tracker_is_listed_whole_from_its_records() {
    const SIZE: usize = 300;
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Mover");
        let base = tracker(&key, &workspace, SIZE).await;
        for limit in PAGES {
            let began = Instant::now();
            let (rows, took) = every_page(&key, &base, limit).await;
            eprintln!(
                "{limit} a page: {} rows in {} pages, {:?}",
                rows.len(),
                took.len(),
                began.elapsed()
            );
            let names: std::collections::HashSet<&str> = rows
                .iter()
                .filter_map(|row| row["label"].as_str())
                .collect();
            assert_eq!(
                names.len(),
                SIZE,
                "every issue is listed once, {limit} a page"
            );
        }
    });
}

/// A person invited into a workspace moves its member list, and with it the policy every
/// caller's access is read under: an agent listing the tracker right after must be answered from
/// the replica it already acts through, at about what a listing costs any other time.
#[test]
fn a_listing_right_after_a_member_is_invited_costs_what_any_listing_does() {
    const SIZE: usize = 600;
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Mover");
        let base = tracker(&key, &workspace, SIZE).await;
        let listing = format!("{base}/resources?path=so.tilo.tasks/tl:so.tilo.tasks&limit=40");
        let list = || async {
            let began = Instant::now();
            let (status, page) = key.get(&listing).await;
            assert_eq!(status, 200, "{page}");
            assert!(
                page["rows"].as_array().is_some_and(|rows| !rows.is_empty()),
                "{page}"
            );
            began.elapsed()
        };
        list().await;
        let mut usual = Vec::new();
        for _ in 0..5 {
            usual.push(list().await);
        }
        let usual = quantile(&usual, 0.5);

        // The member list moves: an invitation is made in the workspace.
        let (status, invited) = key
            .post(
                &format!("{base}/resources/workspace/invitations/actions/invite"),
                &json!({"role": "editor"}),
            )
            .await;
        assert_eq!(status, 200, "{invited}");
        assert!(
            invited["result"]["link"].as_str().is_some(),
            "an invitation's link: {invited}"
        );

        let after = list().await;
        let next = list().await;
        eprintln!(
            "{SIZE} issues: a listing takes {usual:?}; right after the invitation \
             {after:?}, then {next:?}"
        );
        assert!(
            after <= usual * 4 + Duration::from_millis(250),
            "the listing right after the invitation took {after:?}, any other {usual:?}"
        );
    });
}

/// A listing of another app, asked while a tracker of thousands of issues is first listed,
/// answers within a few times what it costs on its own and far within what the tracker takes.
#[test]
fn another_apps_listing_waits_for_no_tracker_taking_in_its_parts() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Mover");
        let base = format!("/v1/workspaces/{workspace}");
        act(&key, &base, "so.tilo.tasks", "open", &json!({})).await;
        act(&key, &base, "contacts", "open", &json!({})).await;
        fill(&key, &base, SIZES[1]).await;

        let contacts = format!("{base}/resources?path=contacts&limit=40");
        let list_contacts = || async {
            let began = Instant::now();
            let (status, page) = key.get(&contacts).await;
            assert_eq!(status, 200, "{page}");
            began.elapsed()
        };
        list_contacts().await;
        let mut alone = Vec::new();
        for _ in 0..5 {
            alone.push(list_contacts().await);
        }
        let alone = quantile(&alone, 0.5);
        let issues = format!("{base}/resources?path=so.tilo.tasks/tl:so.tilo.tasks&limit=40");
        let tracker = async {
            let began = Instant::now();
            let (status, page) = key.get(&issues).await;
            assert_eq!(status, 200, "{page}");
            began.elapsed()
        };
        let beside = async {
            // The tracker's listing starts taking in its parts first.
            tokio::time::sleep(Duration::from_millis(300)).await;
            list_contacts().await
        };
        let (tracker, beside) = tokio::join!(tracker, beside);
        eprintln!(
            "the tracker's first listing took {tracker:?}; the contacts listed beside it in \
             {beside:?}, alone in {alone:?}"
        );
        assert!(
            beside <= alone * 8 + Duration::from_secs(2),
            "the contacts listed in {beside:?} beside the tracker's first listing ({tracker:?}), \
             alone in {alone:?}"
        );
    });
}

/// A tracker kept in many parts, listed first, answers that listing, and a request of another
/// workspace answers at once: one workspace's listing never holds the others' requests.
#[test]
fn a_trackers_listing_holds_no_other_request() {
    const SIZE: usize = 1_000;
    /// How long another workspace's request may wait while the tracker is listed, beyond a
    /// multiple of what it takes alone on this machine.
    const OTHER: Duration = Duration::from_secs(3);
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Mover");
        let base = tracker(&key, &workspace, SIZE).await;
        // Another workspace the daemon governs: the person's own, made as their computer makes
        // one for them.
        let other = format!(
            "/v1/workspaces/{}",
            workspace_for(host.as_ref(), "Elsewhere").await
        );
        act(&key, &other, "so.tilo.tasks", "open", &json!({})).await;

        let mut alone = Vec::new();
        for _ in 0..3 {
            let began = Instant::now();
            let (status, page) = key
                .get(&format!("{other}/resources?path=so.tilo.tasks&limit=40"))
                .await;
            assert_eq!(status, 200, "{page}");
            alone.push(began.elapsed());
        }
        let alone = quantile(&alone, 0.5);
        let nearest = format!("{other}/resources?path=so.tilo.tasks?limit=40");
        let tracker = async {
            let began = Instant::now();
            let (status, page) = key
                .get(&format!(
                    "{base}/resources?path=so.tilo.tasks/tl:so.tilo.tasks&limit=40"
                ))
                .await;
            (status, page, began.elapsed())
        };
        let elsewhere = async {
            // The tracker's listing starts taking in its parts first.
            tokio::time::sleep(Duration::from_millis(300)).await;
            let began = Instant::now();
            let (status, page) = key.get(&nearest).await;
            (status, page, began.elapsed())
        };
        let ((first_status, first, first_took), (status, elsewhere, other_took)) =
            tokio::join!(tracker, elsewhere);
        eprintln!(
            "the tracker's first page in {first_took:?}, another workspace's in {other_took:?}, \
             alone in {alone:?}"
        );
        assert_eq!(status, 200, "{elsewhere}");
        assert_eq!(first_status, 200, "{first}");
        assert!(
            other_took <= alone * 8 + OTHER,
            "another workspace's request waited {other_took:?} for the tracker's listing, alone \
             {alone:?}"
        );
        assert!(
            first_took < LISTING_CEILING,
            "the tracker's first listing took {first_took:?}"
        );
        assert_eq!(
            first["rows"].as_array().map_or(0, Vec::len),
            40,
            "the first page lists its issues: {first}"
        );
    });
}

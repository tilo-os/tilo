#![allow(
    clippy::arithmetic_side_effects,
    reason = "fixture times are small sums of a fixed clock"
)]

use super::*;
use crate::browsers::profiles;

/// 2026-11-28, the clock these fixtures are read at.
const NOW: i64 = 1_795_000_000;

/// A Firefox profile signed in to Slack and to a site, with a container's cookie, an expired one
/// and one stored in milliseconds, as Firefox writes `cookies.sqlite`.
pub(crate) fn firefox_home(home: &Path, now: i64) {
    let firefox = home.join(".mozilla/firefox");
    let profile = firefox.join("p.default-release");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(
        firefox.join("profiles.ini"),
        "[Profile0]\nName=default-release\nIsRelative=1\nPath=p.default-release\n",
    )
    .unwrap();
    let connection = rusqlite::Connection::open(profile.join("cookies.sqlite")).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE moz_cookies (id INTEGER PRIMARY KEY, originAttributes TEXT NOT NULL \
             DEFAULT '', name TEXT, value TEXT, host TEXT, path TEXT, expiry INTEGER, \
             isSecure INTEGER, isHttpOnly INTEGER, sameSite INTEGER DEFAULT 0);",
        )
        .unwrap();
    for (attributes, name, value, host, expiry, secure, http_only, same) in [
        (
            "",
            "d",
            "slack-session",
            ".slack.com",
            now + 86_400,
            1,
            1,
            0,
        ),
        (
            "",
            "user_session",
            "gh-session",
            "github.com",
            (now + 3_600) * 1000,
            1,
            1,
            1,
        ),
        (
            "^userContextId=2",
            "d",
            "work-container",
            ".slack.com",
            now + 86_400,
            1,
            1,
            0,
        ),
        ("", "old", "expired", ".example.org", now - 1, 0, 0, 0),
        ("", "bad name", "x", ".example.org", now + 60, 0, 0, 0),
    ] {
        connection
            .execute(
                "INSERT INTO moz_cookies (originAttributes, name, value, host, path, expiry, \
                 isSecure, isHttpOnly, sameSite) VALUES (?1, ?2, ?3, ?4, '/', ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    attributes, name, value, host, expiry, secure, http_only, same
                ],
            )
            .unwrap();
    }
}

/// A Chromium profile at cookie database version 24, whose values are sealed with Chromium's
/// fixed `v10` key behind the hash of their host, with a partitioned cookie beside them.
fn chromium_home(home: &Path, now: i64) {
    let profile = home.join(".config/chromium/Default/Network");
    std::fs::create_dir_all(&profile).unwrap();
    let connection = rusqlite::Connection::open(profile.join("Cookies")).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE meta (key TEXT, value TEXT); INSERT INTO meta VALUES ('version', '24');
             CREATE TABLE cookies (host_key TEXT, top_frame_site_key TEXT, name TEXT, value TEXT, \
             encrypted_value BLOB, path TEXT, expires_utc INTEGER, is_secure INTEGER, \
             is_httponly INTEGER, samesite INTEGER);",
        )
        .unwrap();
    let key = tilo_seal::compat::chromium_oscrypt_key(b"peanuts");
    let seal = |host: &str, value: &str| {
        let mut plain = Sha256::digest(host.as_bytes()).to_vec();
        plain.extend_from_slice(value.as_bytes());
        let mut sealed = b"v10".to_vec();
        sealed.extend(tilo_seal::compat::chromium_oscrypt_ciphertext(&key, &plain).unwrap());
        sealed
    };
    let chromium_time = |unix: i64| (unix + WINDOWS_EPOCH_OFFSET) * 1_000_000;
    for (host, partition, name, sealed, expires, same) in [
        (
            ".discord.com",
            "",
            "__dcfduid",
            seal(".discord.com", "discord-session"),
            chromium_time(now + 600),
            1,
        ),
        (
            "mail.example.com",
            "",
            "SID",
            seal("mail.example.com", "mail-session"),
            0,
            -1,
        ),
        (
            ".tracker.example",
            "https://news.example",
            "id",
            seal(".tracker.example", "x"),
            chromium_time(now + 600),
            0,
        ),
        (
            "forged.example",
            "",
            "SID",
            seal("another.example", "moved"),
            chromium_time(now + 600),
            0,
        ),
    ] {
        connection
            .execute(
                "INSERT INTO cookies VALUES (?1, ?2, ?3, '', ?4, '/', ?5, 1, 1, ?6)",
                rusqlite::params![host, partition, name, sealed, expires, same],
            )
            .unwrap();
    }
}

fn read_all(home: &Path) -> Vec<(String, Vec<Cookie>)> {
    profiles(home, &home.join(".config"))
        .iter()
        .map(|profile| (profile.label.clone(), sign_ins(profile, NOW).unwrap()))
        .collect()
}

fn values(cookies: &[Cookie]) -> Vec<(&str, &str, &str, bool)> {
    cookies
        .iter()
        .map(|cookie| {
            (
                cookie.domain.as_str(),
                cookie.name.as_str(),
                cookie.value.as_str(),
                cookie.host_only,
            )
        })
        .collect()
}

/// Firefox's sign-ins come over as the profile the person browses with keeps them: a
/// container's, an expired cookie and one no browser would send stay, and a cookie counted in
/// milliseconds expires when it says.
#[test]
fn a_firefox_profile_brings_the_sites_it_is_signed_in_to() {
    let home = tempfile::tempdir().unwrap();
    firefox_home(home.path(), NOW);
    let read = read_all(home.path());
    assert_eq!(read.len(), 1);
    let (label, cookies) = &read[0];
    assert_eq!(label, "Firefox · default-release");
    assert_eq!(
        values(cookies),
        [
            ("github.com", "user_session", "gh-session", true),
            ("slack.com", "d", "slack-session", false),
        ]
    );
    assert_eq!(cookies[0].expires_unix_seconds, Some(NOW + 3_600));
    assert_eq!(cookies[0].same_site, SameSite::Lax);
    assert_eq!(cookies[1].same_site, SameSite::None);
    assert!(
        cookies
            .iter()
            .all(|cookie| cookie.secure && cookie.http_only)
    );
    assert!(
        !format!("{cookies:?}").contains("session"),
        "no value is ever printed"
    );
    assert_eq!(site_count(cookies), 2);
}

/// A Chromium profile's sealed cookies open with Chromium's key, each checked against the host
/// it was sealed for: a value moved to another host, and a cookie partitioned under another
/// site, stay. A cookie for the session comes over as one.
#[test]
fn a_chromium_profile_opens_its_sealed_cookies() {
    let home = tempfile::tempdir().unwrap();
    chromium_home(home.path(), NOW);
    let read = read_all(home.path());
    assert_eq!(read.len(), 1);
    let (label, cookies) = &read[0];
    assert_eq!(label, "Chromium · Default");
    assert_eq!(
        values(cookies),
        [
            ("discord.com", "__dcfduid", "discord-session", false),
            ("mail.example.com", "SID", "mail-session", true),
        ]
    );
    assert_eq!(cookies[0].expires_unix_seconds, Some(NOW + 600));
    assert_eq!(cookies[1].expires_unix_seconds, None);
    assert_eq!(cookies[1].same_site, SameSite::Unspecified);
}

/// The sessions a profile's sign-ins are kept as: one for each service with a window of its own,
/// holding only that service's cookies, and the person's own window's with every cookie, split
/// where one session's bounds end.
#[test]
fn sign_ins_are_kept_per_service_and_for_the_persons_window_within_bounds() {
    let home = tempfile::tempdir().unwrap();
    firefox_home(home.path(), NOW);
    chromium_home(home.path(), NOW);
    let all: Vec<Cookie> = read_all(home.path())
        .into_iter()
        .flat_map(|(_, cookies)| cookies)
        .collect();
    let kept = sessions(&all);
    let shape: Vec<(Service, usize)> = kept
        .iter()
        .map(|(service, cookies)| (*service, cookies.len()))
        .collect();
    assert_eq!(
        shape,
        [
            (Service::Slack, 1),
            (Service::Discord, 1),
            (Service::Web, 4)
        ]
    );
    assert_eq!(kept[0].1[0].value, "slack-session");

    let many: Vec<Cookie> = (0..MAX_COOKIES * 2 + 3)
        .map(|index| {
            let mut cookie = all[0].clone();
            cookie.name = format!("c{index}");
            cookie
        })
        .collect();
    let split: Vec<usize> = sessions(&many)
        .into_iter()
        .filter(|(service, _)| *service == Service::Web)
        .map(|(_, cookies)| cookies.len())
        .collect();
    assert_eq!(split, [MAX_COOKIES, MAX_COOKIES, 3]);
    for (service, cookies) in sessions(&many) {
        let session = browser_continuity::sessions::SessionCookies {
            schema: 1,
            session_id: "kept".into(),
            service,
            identity_public_key: [7; 32],
            setup_binding: "workspace".into(),
            source_snapshot_digest: [1; 32],
            cookies,
            origin_storage: Vec::new(),
        };
        session
            .validate()
            .expect("every session is one the vault keeps");
    }
}

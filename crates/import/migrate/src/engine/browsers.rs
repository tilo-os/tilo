//! Browsers on this computer as a migration source: each profile's bookmarks, history and saved
//! passwords, read by the installer's readers ([`crate::browsers`],
//! [`browser_continuity`]) and landed through the apps' own actions: bookmarks into the person's
//! library in Browser (`import_bookmarks`), history into their history there
//! (`import_history`), each password into their vault in Passwords (`save_login`, whose
//! password is a secret parameter that is never printed), and the sites each profile is signed
//! in to as the workspace's website sign-ins (`workspace/mail-accounts#add-website-session`,
//! whose cookies are a secret parameter too): the person's own Browser window takes them, so
//! they are signed in to those sites in Tilo, and Slack and Discord open signed in, each in a
//! window of its own.

use std::path::Path;

use anyhow::{Result, anyhow};
use browser_continuity::imports::{ImportBatch, ImportBundle};
use browser_continuity::sessions::{Cookie, Service};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::registry::Row;
use super::state::{Item, Review};
use super::{Cx, Here, Landed, Lander, Source};
use crate::browsers::{self, BrowserProfile, Engine};

/// The browsers on this computer.
pub struct Browsers;

impl Source for Browsers {
    fn here(&self, _: &Cx<'_>, row: &'static Row) -> Here {
        Here {
            next: Some(format!("tilo migrate review {}", row.key)),
            ..Here::default()
        }
    }

    fn review(&self, cx: &Cx<'_>, row: &'static Row, _: Option<&str>) -> Result<Review> {
        let mut review = review(cx.home, &config_dir(cx.home));
        super::keep_chosen(&mut review, cx.place.review(row.key)?.as_ref());
        Ok(review)
    }

    fn summary(&self, _: &'static Row, review: &Review) -> String {
        summary(review)
    }

    fn land(
        &self,
        cx: &Cx<'_>,
        _: &'static Row,
        review: &Review,
        _: Option<&super::files::Sink>,
        lander: &mut Lander<'_>,
    ) -> Result<()> {
        land(cx.home, &config_dir(cx.home), review, lander)
    }
}

const IMPORT_BOOKMARKS: &str = "browser#import_bookmarks";
const IMPORT_HISTORY: &str = "browser#import_history";
const SAVE_LOGIN: &str = "passwords#save_login";
/// The workspace's accounts' action that keeps a website sign-in.
const ADD_WEBSITE_SESSION: &str = "add-website-session";
/// The most rows one import carries, so its arguments stay well under what one call takes.
const ROWS_PER_CALL: usize = 4_000;
/// The parts of a profile a review lists.
const PARTS: [&str; 4] = ["bookmarks", "history", "passwords", "sign-ins"];

/// What one profile holds.
struct Read {
    bookmarks: Option<ImportBundle>,
    history: Option<ImportBundle>,
    logins: Result<Vec<crate::thunderbird::SavedLogin>, String>,
    sign_ins: Result<Vec<Cookie>, crate::thunderbird::LoginsError>,
}

/// Now, in Unix seconds: what a cookie's expiry is measured against.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// The config directory of this computer's home `home`.
fn config_dir(home: &Path) -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
}

/// The key a review lists `profile` under: `firefox/default-release`, `google-chrome/Default`.
fn key_of(profile: &BrowserProfile) -> String {
    let browser = profile.browser.to_ascii_lowercase().replace(' ', "-");
    let name = profile
        .label
        .split_once(" · ")
        .map_or(profile.label.as_str(), |(_, name)| name);
    format!("{browser}/{name}")
}

fn read(profile: &BrowserProfile) -> Read {
    use browser_continuity::sources::{
        GrantedProfile, parse_chromium_bookmarks_bundle, parse_chromium_history_bundle,
        parse_firefox_places_bundle,
    };
    let granted = std::fs::File::open(&profile.directory)
        .and_then(GrantedProfile::from_directory)
        .ok();
    let (bookmarks, history) = match (&granted, profile.engine) {
        (Some(granted), Engine::Chromium) => (
            parse_chromium_bookmarks_bundle(granted).ok(),
            parse_chromium_history_bundle(granted).ok(),
        ),
        (Some(granted), Engine::Firefox) => {
            let places = parse_firefox_places_bundle(granted).ok();
            (places.clone(), places)
        }
        (None, _) | (Some(_), Engine::Safari) => (None, None),
    };
    Read {
        bookmarks: bookmarks.map(|bundle| bundle.bookmark_bundle()),
        history: history.map(|bundle| bundle.history_bundle()),
        logins: match profile.sealed_by {
            Some(system) => Err(format!(
                "{system} keeps {}'s saved passwords locked to its own sign-in, so they are read \
                 only there: export them from {} while {system} runs, or sign in to its sync",
                profile.browser, profile.browser
            )),
            None => browsers::saved_logins(profile).map_err(|error| error.to_string()),
        },
        sign_ins: match profile.sealed_by {
            Some(system) => Err(crate::thunderbird::LoginsError::Unreadable(format!(
                "{system} keeps the sites {} is signed in to locked to its own sign-in, so they \
                 come over only from {system} itself",
                profile.browser
            ))),
            None => browsers::sign_ins::sign_ins(profile, now()),
        },
    }
}

fn bookmark_count(bundle: &ImportBundle) -> usize {
    bundle
        .chunks
        .iter()
        .map(|chunk| chunk.bookmarks.len())
        .sum()
}

fn history_count(bundle: &ImportBundle) -> usize {
    bundle.chunks.iter().map(|chunk| chunk.history.len()).sum()
}

/// What a person reads of the sign-ins `cookies` bring beside their own window: the services
/// that open signed in, each in a window of its own.
fn sign_ins_note(profile: &BrowserProfile, cookies: &[Cookie]) -> Option<String> {
    let services: Vec<&str> = browsers::sign_ins::sessions(cookies)
        .iter()
        .filter(|(service, _)| *service != Service::Web)
        .map(|(service, _)| service.label())
        .collect();
    let windows = match services.as_slice() {
        [] => None,
        [one] => Some(format!(
            "{one} also becomes an account of the workspace, opening signed in in a window of \
             its own"
        )),
        many => Some(format!(
            "{} also become accounts of the workspace, each opening signed in in a window of \
             its own",
            many.join(" and ")
        )),
    };
    let lasting = session_only_said(profile, cookies);
    match (windows, lasting) {
        (Some(windows), Some(lasting)) => Some(format!("{windows}. {lasting}")),
        (windows, lasting) => windows.or(lasting),
    }
}

/// What becomes of the sign-ins `profile` keeps only until its browser closes: a Chromium-family
/// browser's come over and last until Browser closes, as they did there; Firefox and Safari
/// never write theirs down, so they stay behind.
fn session_only_said(profile: &BrowserProfile, cookies: &[Cookie]) -> Option<String> {
    if profile.engine != Engine::Chromium {
        return Some(format!(
            "Sites {} keeps you signed in to only until it closes stay there: sign in to them \
             again in Browser",
            profile.browser
        ));
    }
    let session_only: Vec<Cookie> = cookies
        .iter()
        .filter(|cookie| cookie.expires_unix_seconds.is_none())
        .cloned()
        .collect();
    match browsers::sign_ins::site_count(&session_only) {
        0 => None,
        1 => Some(format!(
            "1 site kept you signed in only until {} closed: it stays signed in until Browser \
             closes",
            profile.browser
        )),
        sites => Some(format!(
            "{sites} sites kept you signed in only until {} closed: they stay signed in until \
             Browser closes",
            profile.browser
        )),
    }
}

/// The kind of the items that name an account a profile was signed in to, which Migrations
/// offers to connect through the account's own sign-in once a run is done. Nothing of them is
/// brought in: the browser's cookies are no grant for the account's mail, calendars and files.
pub const ACCOUNT_TO_CONNECT: &str = "account-to-connect";

/// The accounts `profile`, listed under `key`, was signed in to (`{key}/account/google`): each
/// titled by its provider, its note the address a password the profile saved for its sign-in
/// page names.
fn accounts_to_connect(key: &str, profile: &BrowserProfile, held: &Read) -> Vec<Item> {
    let mut accounts = match &held.sign_ins {
        Ok(cookies) => browsers::sign_ins::accounts(cookies),
        Err(_) => Vec::new(),
    };
    if browsers::sign_ins::discord_signed_in(profile) {
        accounts.push(browsers::sign_ins::Account::Discord);
    }
    let logins = held.logins.as_deref().unwrap_or_default();
    accounts
        .into_iter()
        .map(|account| Item {
            key: format!("{key}/account/{}", account.key()),
            kind: ACCOUNT_TO_CONNECT.to_owned(),
            title: account.label().to_owned(),
            count: 0,
            counts: String::new(),
            within: Some(key.to_owned()),
            chosen: false,
            note: browsers::sign_ins::address_of(account, logins),
        })
        .collect()
}

/// What a person reads, once a run is done, of the accounts `review` found its profiles signed
/// in to.
fn accounts_said(review: &Review) -> Option<String> {
    let mut names: Vec<&str> = review
        .items
        .iter()
        .filter(|item| item.kind == ACCOUNT_TO_CONNECT)
        .map(|item| item.title.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    let (last, rest) = names.split_last()?;
    let listed = if rest.is_empty() {
        (*last).to_owned()
    } else {
        format!("{} and {last}", rest.join(", "))
    };
    Some(format!(
        "These browsers were also signed in to {listed}: connect them as the workspace's \
         accounts in Migrations, each through its own sign-in page"
    ))
}

/// `tilo migrate review browsers`: each profile in `home`, whose config directory is `config`, and
/// its bookmarks, history, passwords and the sites it is signed in to.
pub fn review(home: &Path, config: &Path) -> Review {
    use crate::thunderbird::LoginsError;
    let mut items = Vec::new();
    let mut unreadable = Vec::new();
    for profile in browsers::profiles(home, config) {
        let key = key_of(&profile);
        let held = read(&profile);
        let mut parts = Vec::new();
        if let Some(bundle) = &held.bookmarks {
            parts.push(("bookmarks", bookmark_count(bundle), None));
        }
        if let Some(bundle) = &held.history {
            parts.push(("history", history_count(bundle), None));
        }
        match &held.logins {
            Ok(logins) => parts.push(("passwords", logins.len(), None)),
            Err(why) if why.contains("saved no") || why.contains("no passwords") => {}
            Err(why) => unreadable.push(format!("{}: passwords: {why}", profile.label)),
        }
        match &held.sign_ins {
            Ok(cookies) => parts.push((
                "sign-ins",
                browsers::sign_ins::site_count(cookies),
                sign_ins_note(&profile, cookies),
            )),
            Err(LoginsError::NoneSaved) => {}
            Err(LoginsError::Locked) => unreadable.push(format!(
                "{}: sign-ins: the system keyring keeps their key locked; unlock it (sign in to \
                 the desktop the browser runs on), then review again",
                profile.label
            )),
            Err(LoginsError::Unreadable(why)) => {
                unreadable.push(format!("{}: sign-ins: {why}", profile.label));
            }
        }
        let total = parts.iter().map(|(_, count, _)| count).sum();
        items.push(Item {
            key: key.clone(),
            kind: "browser-profile".to_owned(),
            title: profile.label.clone(),
            count: total,
            counts: "things".to_owned(),
            within: None,
            chosen: false,
            note: None,
        });
        for (part, count, note) in parts {
            items.push(Item {
                key: format!("{key}/{part}"),
                kind: part.to_owned(),
                title: format!("{} {part}", profile.label),
                count,
                counts: if part == "sign-ins" { "sites" } else { part }.to_owned(),
                within: Some(key.clone()),
                chosen: false,
                note,
            });
        }
        items.extend(accounts_to_connect(&key, &profile, &held));
    }
    Review {
        source: "browsers".to_owned(),
        title: "Browsers on this computer".to_owned(),
        handle: home.display().to_string(),
        items,
        unreadable,
    }
}

/// What bringing `review` over brings, in a few words.
#[must_use]
pub fn summary(review: &Review) -> String {
    let chosen = review.chosen();
    let count = |kind: &str| -> usize {
        chosen
            .iter()
            .filter(|item| item.kind == kind)
            .map(|item| item.count)
            .sum()
    };
    format!(
        "Bring {}, {}, {} and the sign-ins to {} from this computer's browsers",
        super::services::counted(count("bookmarks"), "bookmarks"),
        super::services::counted(count("history"), "pages of history"),
        super::services::counted(count("passwords"), "passwords"),
        super::services::counted(count("sign-ins"), "sites"),
    )
}

/// `bundle` in pieces each one call carries: the whole bundle when it is small, so a
/// bookmark folder tree comes with it.
fn pieces(bundle: &ImportBundle) -> Result<Vec<ImportBundle>> {
    let rows: usize = bundle
        .chunks
        .iter()
        .map(|chunk| chunk.bookmarks.len().saturating_add(chunk.history.len()))
        .sum();
    if rows <= ROWS_PER_CALL {
        return Ok(vec![bundle.clone()]);
    }
    let mut bookmarks: Vec<_> = bundle
        .chunks
        .iter()
        .flat_map(|chunk| chunk.bookmarks.clone())
        .collect();
    let mut history: Vec<_> = bundle
        .chunks
        .iter()
        .flat_map(|chunk| chunk.history.clone())
        .collect();
    let mut pieces = Vec::new();
    let mut part = 0_u64;
    while !bookmarks.is_empty() || !history.is_empty() {
        let take_bookmarks = bookmarks.len().min(ROWS_PER_CALL);
        let take_history = history
            .len()
            .min(ROWS_PER_CALL.saturating_sub(take_bookmarks));
        // Each piece is a source of its own, so a piece imported again is not counted twice
        // and pieces never stand for one another.
        let mut source = blake3::Hasher::new();
        source.update(&bundle.source);
        source.update(&part.to_le_bytes());
        part = part.saturating_add(1);
        pieces.push(
            ImportBundle::from_batch(ImportBatch {
                source: *source.finalize().as_bytes(),
                bookmarks: bookmarks.drain(..take_bookmarks).collect(),
                history: history.drain(..take_history).collect(),
            })
            .map_err(|error| anyhow!("a browser import did not take shape: {error}"))?,
        );
    }
    Ok(pieces)
}

/// `piece`'s bookmarks (`part` `bookmarks`) or visits as Browser's own import reads them:
/// `{"bookmarks": [{"url", "title"}]}`, `{"visits": [{"url", "title", "last", "count"}]}`.
fn for_browser(piece: &ImportBundle, part: &str) -> String {
    let chunks = piece.chunks.iter();
    let value = if part == "bookmarks" {
        let rows: Vec<Value> = chunks
            .flat_map(|chunk| &chunk.bookmarks)
            .map(|row| json!({"url": row.url, "title": row.title}))
            .collect();
        json!({ "bookmarks": rows })
    } else {
        let rows: Vec<Value> = chunks
            .flat_map(|chunk| &chunk.history)
            .map(|row| {
                json!({
                    "url": row.url,
                    "title": row.title,
                    "last": row.last_visit,
                    "count": row.visit_count,
                })
            })
            .collect();
        json!({ "visits": rows })
    };
    value.to_string()
}

/// Bring the chosen parts of each chosen profile over, each a step of the lander's invocation.
///
/// # Errors
/// The migration was stopped, or the workspace refused.
pub fn land(home: &Path, config: &Path, review: &Review, lander: &mut Lander<'_>) -> Result<()> {
    let chosen: Vec<String> = review
        .chosen()
        .iter()
        .filter(|item| item.within.is_some())
        .map(|item| item.key.clone())
        .collect();
    for profile in browsers::profiles(home, config) {
        let key = key_of(&profile);
        let wants = |part: &str| chosen.contains(&format!("{key}/{part}"));
        if !PARTS.iter().any(|part| wants(part)) {
            continue;
        }
        let held = read(&profile);
        if wants("bookmarks")
            && let Some(bundle) = &held.bookmarks
        {
            lander.open("browser");
            for (index, piece) in pieces(bundle)?.iter().enumerate() {
                let encoded = for_browser(piece, "bookmarks");
                let named = format!(
                    "{key}/bookmarks/{index}/{}",
                    blake3::hash(encoded.as_bytes()).to_hex()
                );
                lander.once(&named, "bookmark imports", |workspace, invocation| {
                    workspace.act(invocation, IMPORT_BOOKMARKS, json!({"bundle": encoded}))?;
                    Ok(Landed::One)
                })?;
            }
        }
        if wants("history")
            && let Some(bundle) = &held.history
        {
            lander.open("browser");
            for (index, piece) in pieces(bundle)?.iter().enumerate() {
                let encoded = for_browser(piece, "history");
                let named = format!(
                    "{key}/history/{index}/{}",
                    blake3::hash(encoded.as_bytes()).to_hex()
                );
                lander.once(&named, "history imports", |workspace, invocation| {
                    workspace.act(invocation, IMPORT_HISTORY, json!({"bundle": encoded}))?;
                    Ok(Landed::One)
                })?;
            }
        }
        if wants("passwords") {
            let logins = held.logins.map_err(|why| {
                anyhow!(
                    "{}'s passwords can't be read ({why}). Close the browser, or for a primary \
                     password export them from the browser, then start again",
                    profile.label
                )
            })?;
            lander.open("passwords");
            for login in &logins {
                let name = site_of(&login.origin);
                lander.once(
                    &format!("{key}/login/{}/{}", login.origin, login.username),
                    "passwords",
                    |workspace, invocation| {
                        workspace.act(
                            invocation,
                            SAVE_LOGIN,
                            json!({
                                "name": name,
                                "origin": login.origin,
                                "username": login.username,
                                "password": login.password.as_str(),
                            }),
                        )?;
                        Ok(Landed::One)
                    },
                )?;
            }
        }
        if wants("sign-ins") {
            let cookies = held.sign_ins.map_err(|why| {
                anyhow!(
                    "{}'s sign-ins can't be read ({why}). Close the browser, or unlock the \
                     system keyring, then start again",
                    profile.label
                )
            })?;
            land_sign_ins(&profile, &key, &cookies, lander)?;
        }
    }
    if let Some(said) = accounts_said(review) {
        lander.say(&said);
    }
    Ok(())
}

/// Keep the sign-ins `cookies` of `profile` (listed under `key`) as the workspace's website
/// sign-ins: one for each service that opens in a window of its own, and every cookie in as
/// few as hold them for the person's own Browser window. Each is the workspace's account of it,
/// named after the profile, so bringing the profile over again keeps the newer cookies in place
/// of the older ones.
fn land_sign_ins(
    profile: &BrowserProfile,
    key: &str,
    cookies: &[Cookie],
    lander: &mut Lander<'_>,
) -> Result<()> {
    let profile_id = blake3::hash(key.as_bytes()).to_hex();
    let profile_id = profile_id.get(..16).unwrap_or_default();
    let mut part = 0_usize;
    for (service, session) in browsers::sign_ins::sessions(cookies) {
        let (id, title, site) = if service == Service::Web {
            part = part.saturating_add(1);
            let title = if part == 1 {
                format!("Sign-ins from {}", profile.label)
            } else {
                format!("Sign-ins from {} ({part})", profile.label)
            };
            let site = session
                .first()
                .map(|cookie| format!("https://{}/", cookie.host))
                .unwrap_or_default();
            (format!("web-{profile_id}-{part}"), title, site)
        } else {
            (
                format!("{}-{profile_id}", service.as_str()),
                format!("{} (from {})", service.label(), profile.label),
                service.url().to_owned(),
            )
        };
        let encoded = Zeroizing::new(serde_json::to_string(&session)?);
        let named = format!(
            "{key}/sign-ins/{id}/{}",
            blake3::hash(encoded.as_bytes()).to_hex()
        );
        lander.once(&named, "website sign-ins", |workspace, _| {
            workspace.person(
                super::thunderbird::MAIL_ACCOUNTS,
                ADD_WEBSITE_SESSION,
                json!({
                    "id": id,
                    "title": title,
                    "service": service.as_str(),
                    "site": site,
                    "cookies": encoded.as_str(),
                }),
            )?;
            Ok(Landed::One)
        })?;
    }
    Ok(())
}

/// What a login is called in the vault: the site it signs in to.
fn site_of(origin: &str) -> String {
    url::Url::parse(origin)
        .ok()
        .and_then(|url| url.domain().map(str::to_owned))
        .unwrap_or_else(|| origin.to_owned())
}

#[cfg(test)]
mod tests {
    use super::super::state::Place;
    use super::super::tests::test_env::{Recorded, Told, land_as};
    use super::super::writer::Invocation;
    use super::*;

    /// A Firefox profile signed in to Slack and GitHub, reviewed and its sign-ins chosen, lands
    /// as the person's own requests to keep them: Slack's cookies as Slack's account, and every
    /// cookie as the sign-ins the person's own Browser window takes. Landing it again keeps
    /// nothing twice.
    #[test]
    fn a_profiles_sign_ins_become_the_workspaces_website_sign_ins() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        crate::browsers::sign_ins::tests::firefox_home(&home, now());
        let place = Place::at(&dir.path().join("state"));
        let mut review = review(&home, &home.join(".config"));
        let item = review
            .items
            .iter()
            .find(|item| item.kind == "sign-ins")
            .expect("the profile's sign-ins are listed");
        assert_eq!(item.key, "firefox/default-release/sign-ins");
        assert_eq!((item.count, item.counts.as_str()), (2, "sites"));
        assert!(
            item.note
                .as_deref()
                .is_some_and(|note| note.starts_with("Slack also becomes an account")),
            "{item:?}"
        );
        review
            .choose(&["firefox/default-release/sign-ins".to_owned()], true, true)
            .unwrap();
        place.keep_review(&review).unwrap();
        assert!(summary(&review).contains("the sign-ins to 2 sites"));

        let invocation = Invocation {
            id: "run-1".into(),
            title: summary(&review),
        };
        let mut workspace = Recorded::default();
        let mut told = Told::default();
        land_as(
            &place,
            "browsers",
            &invocation,
            &mut workspace,
            &mut told,
            |lander| land(&home, &home.join(".config"), &review, lander),
        )
        .unwrap();
        let kept: Vec<(&str, &str, &str)> = workspace
            .websites
            .iter()
            .map(|asked| {
                (
                    asked["service"].as_str().unwrap(),
                    asked["title"].as_str().unwrap(),
                    asked["site"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            kept,
            [
                (
                    "slack",
                    "Slack (from Firefox · default-release)",
                    "https://app.slack.com/"
                ),
                (
                    "web",
                    "Sign-ins from Firefox · default-release",
                    "https://github.com/"
                ),
            ]
        );
        let cookies = |at: usize| -> Vec<Cookie> {
            serde_json::from_str(workspace.websites[at]["cookies"].as_str().unwrap()).unwrap()
        };
        assert_eq!(cookies(0).len(), 1);
        assert_eq!(cookies(0)[0].value, "slack-session");
        let web: Vec<String> = cookies(1).iter().map(|c| c.value.clone()).collect();
        assert_eq!(web, ["gh-session", "slack-session"]);
        assert!(
            workspace.actions.is_empty(),
            "no app is asked: sign-ins are the person's own requests"
        );

        let mut again = Recorded::default();
        land_as(
            &place,
            "browsers",
            &invocation,
            &mut again,
            &mut told,
            |lander| land(&home, &home.join(".config"), &review, lander),
        )
        .unwrap();
        assert!(again.websites.is_empty(), "nothing is kept twice");
    }

    /// What becomes of the sign-ins a browser keeps only until it closes is said where they are
    /// chosen: a Chromium profile's come over for Browser's session, and Firefox's stay.
    #[test]
    fn a_review_says_what_becomes_of_sign_ins_kept_only_until_the_browser_closes() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        crate::browsers::sign_ins::tests::firefox_home(&home, now());
        crate::browsers::sign_ins::tests::chromium_home(&home, now());
        let review = review(&home, &home.join(".config"));
        let note = |key: &str| -> String {
            review
                .items
                .iter()
                .find(|item| item.key == key)
                .and_then(|item| item.note.clone())
                .unwrap_or_default()
        };
        assert_eq!(
            note("chromium/Default/sign-ins"),
            "Discord also becomes an account of the workspace, opening signed in in a window of \
             its own. 1 site kept you signed in only until Chromium closed: it stays signed in \
             until Browser closes"
        );
        assert!(
            note("firefox/default-release/sign-ins").ends_with(
                "Sites Firefox keeps you signed in to only until it closes stay there: sign in \
                 to them again in Browser"
            ),
            "{}",
            note("firefox/default-release/sign-ins")
        );
    }

    /// The accounts a profile was signed in to are listed under it for Migrations to offer once
    /// the run is done, bringing nothing in of their own.
    #[test]
    fn a_profiles_accounts_are_listed_to_connect_through_their_own_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        crate::browsers::sign_ins::tests::firefox_home(&home, now());
        crate::browsers::sign_ins::tests::firefox_signed_in(
            &home,
            now(),
            &[("SID", ".google.com"), ("sp_dc", ".spotify.com")],
        );
        let review = review(&home, &home.join(".config"));
        let listed: Vec<(&str, &str, Option<&str>)> = review
            .items
            .iter()
            .filter(|item| item.kind == ACCOUNT_TO_CONNECT)
            .map(|item| {
                (
                    item.key.as_str(),
                    item.title.as_str(),
                    item.within.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            listed,
            [
                (
                    "firefox/default-release/account/google",
                    "Google",
                    Some("firefox/default-release")
                ),
                (
                    "firefox/default-release/account/spotify",
                    "Spotify",
                    Some("firefox/default-release")
                ),
            ]
        );
        assert_eq!(
            accounts_said(&review).as_deref(),
            Some(
                "These browsers were also signed in to Google and Spotify: connect them as the \
                 workspace's accounts in Migrations, each through its own sign-in page"
            )
        );
    }
}

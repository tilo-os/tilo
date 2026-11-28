//! Browsers on this computer as a migration source: each profile's bookmarks, history and saved
//! passwords, read by the installer's readers ([`crate::browsers`],
//! [`browser_continuity`]) and landed through the apps' own actions: bookmarks into the person's
//! library in Browser (`import-bookmarks`), history into their private history
//! (`import-history-bundle`), each password into their vault in Passwords (`save-login`, whose
//! password is a secret parameter that is never printed).

use std::path::Path;

use anyhow::{Result, anyhow};
use browser_continuity::imports::{ImportBatch, ImportBundle};
use serde_json::json;

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

const IMPORT_BOOKMARKS: &str = "browser/browser.personal-bookmarks#import-bookmarks";
const IMPORT_HISTORY: &str = "browser/browser.browsing-history#import-history-bundle";
const SAVE_LOGIN: &str = "passwords/passwords.vault#save-login";
/// The most rows one import carries, so its arguments stay well under what one call takes.
const ROWS_PER_CALL: usize = 4_000;

/// What one profile holds.
struct Read {
    bookmarks: Option<ImportBundle>,
    history: Option<ImportBundle>,
    logins: Result<Vec<crate::thunderbird::SavedLogin>, String>,
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
        (None, _) => (None, None),
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

/// `tilo migrate review browsers`: each profile in `home`, whose config directory is `config`, and
/// its bookmarks, history and passwords.
pub fn review(home: &Path, config: &Path) -> Review {
    let mut items = Vec::new();
    let mut unreadable = Vec::new();
    for profile in browsers::profiles(home, config) {
        let key = key_of(&profile);
        let held = read(&profile);
        let mut parts = Vec::new();
        if let Some(bundle) = &held.bookmarks {
            parts.push(("bookmarks", bookmark_count(bundle)));
        }
        if let Some(bundle) = &held.history {
            parts.push(("history", history_count(bundle)));
        }
        match &held.logins {
            Ok(logins) => parts.push(("passwords", logins.len())),
            Err(why) if why.contains("saved no") || why.contains("no passwords") => {}
            Err(why) => unreadable.push(format!("{}: passwords: {why}", profile.label)),
        }
        let total = parts.iter().map(|(_, count)| count).sum();
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
        for (part, count) in parts {
            items.push(Item {
                key: format!("{key}/{part}"),
                kind: part.to_owned(),
                title: format!("{} {part}", profile.label),
                count,
                counts: part.to_owned(),
                within: Some(key.clone()),
                chosen: false,
                note: None,
            });
        }
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
        "Bring {}, {} and {} from this computer's browsers",
        super::services::counted(count("bookmarks"), "bookmarks"),
        super::services::counted(count("history"), "pages of history"),
        super::services::counted(count("passwords"), "passwords"),
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
        if !["bookmarks", "history", "passwords"]
            .iter()
            .any(|part| wants(part))
        {
            continue;
        }
        let held = read(&profile);
        if wants("bookmarks")
            && let Some(bundle) = &held.bookmarks
        {
            lander.open("browser");
            for (index, piece) in pieces(bundle)?.iter().enumerate() {
                let encoded = serde_json::to_string(piece)?;
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
                let encoded = serde_json::to_string(piece)?;
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

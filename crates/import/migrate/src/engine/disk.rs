//! An old system left on a disk, as a migration source: Windows, macOS or Linux that ran on this
//! computer before Tilo, on a partition beside it or on a drive attached to it, read from its
//! folders with nothing of it running ([`crate::old_system`]).
//!
//! One review lists everything a person's home there kept: its folders (the familiar ones chosen
//! until the person chooses), the folders Windows keeps elsewhere ([`super::windows::on_disk`]),
//! every browser profile ([`super::browsers`]) and the Thunderbird profile
//! ([`super::thunderbird`]). Each lands through the engine that reads it on a running system, so
//! a folder from the disk is copied as a folder here is, and a browser's bookmarks reach Browser
//! as a running browser's do.
//!
//! Which home is read: the one the person named (`connect disk --url <path>`, or a review's
//! `profile`, either a home or the top of a system), else the first one found on the disks this
//! computer has open.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use super::files::{self, Local};
use super::registry::Row;
use super::state::{Item, Journal, Review};
use super::{Connecting, Cx, Here, Lander, Source};
use crate::old_system::{self, OldHome};

/// An old system on a disk.
pub struct Disk;

/// Where the person's choice of home is kept, beside the source's review.
const FROM: &str = "from";
/// Which of the home's Thunderbird profiles the review read.
const THUNDERBIRD: &str = "thunderbird";
/// The prefixes a browser's and Thunderbird's items are listed under, apart from folders, which
/// are listed by their paths.
const BROWSERS: &str = "browsers:";
const MAIL: &str = "thunderbird:";

/// The home `named` names: a home, or the top of a system holding one person's home.
fn named_home(named: &Path) -> Result<OldHome> {
    if let Some(os) = old_system::system_at(named) {
        let homes = old_system::homes_of(named, os);
        return match homes.as_slice() {
            [home] => Ok(home.clone()),
            [] => bail!("{} holds {} but no one's home", named.display(), os.label()),
            several => bail!(
                "{} holds the homes of {}: name one of them",
                named.display(),
                several
                    .iter()
                    .map(|home| home.home.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
    }
    OldHome::at(named).map_err(|why| anyhow!(why))
}

/// The home a step of this source reads: `given`, else the one the person chose before, else the
/// first one found.
fn home(cx: &Cx<'_>, row: &'static Row, given: Option<&str>) -> Result<OldHome> {
    if let Some(given) = given.filter(|given| Path::new(given).is_absolute()) {
        return named_home(Path::new(given));
    }
    if let Some(kept) = cx.place.field::<PathBuf>(row.key, FROM)?
        && kept.is_dir()
    {
        return named_home(&kept);
    }
    old_system::found_here()
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!(NONE_FOUND))
}

const NONE_FOUND: &str = "No Windows, macOS or Linux is on the disks this computer has open. \
                          Open the drive that holds it in Files first, or name its folder: \
                          `tilo migrate connect disk --url <folder>`";

/// The keys of `items` under `prefix`, with it taken off: what the engine that reads them lists.
fn under(review: &Review, prefix: &str) -> Review {
    Review {
        source: review.source.clone(),
        title: review.title.clone(),
        handle: review.handle.clone(),
        items: review
            .items
            .iter()
            .filter_map(|item| {
                let key = item.key.strip_prefix(prefix)?;
                Some(Item {
                    key: key.to_owned(),
                    within: item
                        .within
                        .as_deref()
                        .and_then(|within| within.strip_prefix(prefix))
                        .map(str::to_owned),
                    ..item.clone()
                })
            })
            .collect(),
        unreadable: Vec::new(),
    }
}

/// `items` listed under `prefix`.
fn prefixed(items: Vec<Item>, prefix: &str) -> impl Iterator<Item = Item> {
    items.into_iter().map(move |item| Item {
        key: format!("{prefix}{}", item.key),
        within: item.within.map(|within| format!("{prefix}{within}")),
        ..item
    })
}

/// The folders of `review`: those it lists by their paths.
fn folders(review: &Review) -> Review {
    Review {
        items: review
            .items
            .iter()
            .filter(|item| item.kind == "folder" && item.key.starts_with('/'))
            .cloned()
            .collect(),
        unreadable: Vec::new(),
        ..review.clone()
    }
}

/// What the home `old` kept, as one review.
#[must_use]
pub fn review_of(old: &OldHome, source: &str) -> (Review, Option<String>) {
    let mut items: Vec<Item> = Vec::new();
    let mut unreadable = Vec::new();
    for (path, familiar) in old.folders() {
        let title = path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        items.push(files::item(
            &old.home,
            &path.to_string_lossy(),
            &title,
            familiar,
        ));
    }
    if old.os == old_system::Os::Windows {
        // A folder Windows keeps is listed once, as what Windows keeps it as.
        for item in super::windows::on_disk(&old.home) {
            match items.iter_mut().find(|known| known.key == item.key) {
                Some(known) => {
                    known.title = item.title;
                    known.note = item.note;
                }
                None => items.push(item),
            }
        }
    }
    let browsers = super::browsers::review(&old.home, &old.home.join(".config"));
    unreadable.extend(browsers.unreadable);
    items.extend(prefixed(browsers.items, BROWSERS));
    let mut thunderbird = None;
    if !crate::thunderbird::scan(&old.home).reviewed.is_empty() {
        match super::thunderbird::review(&old.home, None) {
            Ok(mail) => {
                thunderbird = Some(mail.handle);
                unreadable.extend(mail.unreadable);
                items.extend(prefixed(mail.items, MAIL));
            }
            Err(why) => unreadable.push(format!("Thunderbird: {why:#}")),
        }
    }
    let review = Review {
        source: source.to_owned(),
        title: format!("What {} kept", old.label()),
        handle: old.home.display().to_string(),
        items,
        unreadable,
    };
    (review, thunderbird)
}

impl Source for Disk {
    fn here(&self, _: &Cx<'_>, row: &'static Row) -> Here {
        let found = old_system::found_here();
        if found.is_empty() {
            return Here {
                unavailable: Some(NONE_FOUND.to_owned()),
                ..Here::default()
            };
        }
        Here {
            here: Some(json!(found.iter().map(OldHome::label).collect::<Vec<_>>())),
            next: Some(format!("tilo migrate review {}", row.key)),
            ..Here::default()
        }
    }

    fn connect(&self, cx: &Cx<'_>, row: &'static Row, how: Connecting<'_>) -> Result<Value> {
        let named = how
            .url
            .ok_or_else(|| anyhow!("name the old system's folder, or one person's home in it"))?;
        let old = named_home(Path::new(named))?;
        cx.place.keep_field(row.key, FROM, &old.home)?;
        Ok(json!({
            "source": row.key,
            "said": format!(
                "{} is read next; next: tilo migrate review {}",
                old.label(),
                row.key
            ),
        }))
    }

    fn review(&self, cx: &Cx<'_>, row: &'static Row, given: Option<&str>) -> Result<Review> {
        let old = home(cx, row, given)?;
        cx.place.keep_field(row.key, FROM, &old.home)?;
        let (mut review, thunderbird) = review_of(&old, row.key);
        match thunderbird {
            Some(handle) => cx.place.keep_field(row.key, THUNDERBIRD, &handle)?,
            None => cx.place.forget_field(row.key, THUNDERBIRD)?,
        }
        // What the person chose stays chosen while the review is of the same home; a home read
        // for the first time starts with its familiar folders chosen.
        if let Some(kept) = cx.place.review(row.key)?
            && kept.handle == review.handle
        {
            super::keep_chosen(&mut review, Some(&kept));
        }
        Ok(review)
    }

    fn summary(&self, _: &'static Row, review: &Review) -> String {
        let chosen = review.chosen();
        let files: usize = chosen
            .iter()
            .filter(|item| item.kind == "folder")
            .map(|item| item.count)
            .sum();
        let browsers = chosen.iter().any(|item| item.key.starts_with(BROWSERS));
        let mail = chosen.iter().any(|item| item.key.starts_with(MAIL));
        let mut what = vec![super::services::counted(files, "files")];
        if browsers {
            what.push("browser data".to_owned());
        }
        if mail {
            what.push("Thunderbird".to_owned());
        }
        format!("Bring {} from {}", what.join(", "), review.handle)
    }

    /// Only a run that copies folders needs somewhere to put them.
    fn files_under(&self, _: &'static Row, review: &Review) -> Option<&'static str> {
        (!folders(review).chosen().is_empty()).then_some("")
    }

    fn acts(&self, review: &Review) -> bool {
        review
            .chosen()
            .iter()
            .any(|item| item.key.starts_with(BROWSERS) || item.key.starts_with(MAIL))
    }

    fn land(
        &self,
        cx: &Cx<'_>,
        row: &'static Row,
        review: &Review,
        sink: Option<&files::Sink>,
        lander: &mut Lander<'_>,
    ) -> Result<()> {
        let old = OldHome::at(Path::new(&review.handle)).map_err(|why| {
            anyhow!("the old system is no longer here ({why}): open its drive again")
        })?;
        let folders = folders(review);
        if !folders.chosen().is_empty() {
            let sink = sink.ok_or_else(|| anyhow!("files have nowhere to go"))?;
            files::land(
                &Local {
                    home: old.home.clone(),
                },
                &folders,
                sink,
                cx.place,
                lander,
            )?;
        }
        let browsers = under(review, BROWSERS);
        if !browsers.chosen().is_empty() {
            super::browsers::land(&old.home, &old.home.join(".config"), &browsers, lander)?;
        }
        let mut mail = under(review, MAIL);
        if !mail.chosen().is_empty() {
            mail.handle = cx
                .place
                .field::<String>(row.key, THUNDERBIRD)?
                .ok_or_else(|| anyhow!("review the disk again: its Thunderbird profile moved"))?;
            super::thunderbird::land(&old.home, &mail, lander)?;
        }
        Ok(())
    }

    fn landed(&self, _: &Cx<'_>, _: &'static Row, journal: &Journal) -> Result<usize> {
        Ok(journal
            .brought
            .values()
            .sum::<usize>()
            .saturating_add(journal.already_there))
    }
}

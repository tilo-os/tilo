//! The settings and hidden files of this computer's home, as a migration source: every hidden
//! file and folder at the top of the home (`.config`, `.local`, `.ssh`, `.bashrc`, the app data
//! Flatpak keeps under `.var`, …), so a whole home comes over with the familiar folders
//! ([`super::files::Folders`]) beside it. They are copied by the files engine into the
//! workspace's files, under `Home settings`, as any folder here is; nothing is chosen until the
//! person chooses, as some hold keys.

use std::path::Path;

use anyhow::{Result, anyhow};

use super::files::{self, Local, Tree as _};
use super::registry::Row;
use super::state::{Item, Journal, Review};
use super::{Cx, Here, Lander, Source};

/// The folder of the workspace's files what this source brings goes under.
const UNDER: &str = "Home settings";

/// What a home keeps that is no setting: caches, rebuilt by the programs that made them.
const LEFT_OUT: &[&str] = &[".cache", ".Trash", ".Trash-1000", ".thumbnails"];

/// What some hidden folders hold, for the person deciding.
fn note_of(name: &str) -> Option<&'static str> {
    Some(match name {
        ".ssh" => "Your SSH keys: whoever reads the workspace's files can use them",
        ".gnupg" => "Your GnuPG keys: whoever reads the workspace's files can use them",
        ".config" => "Every program's settings, Tilo's own among them",
        ".local" => "Programs' data: fonts, desktop entries, game saves, Tilo's own state",
        ".var" => "The data of the Flatpak apps on this computer",
        ".mozilla" | ".thunderbird" => "Also brought by the browsers and thunderbird sources",
        _ => return None,
    })
}

/// The hidden files and folders of this computer's home.
pub struct Settings;

impl Source for Settings {
    fn here(&self, _: &Cx<'_>, row: &'static Row) -> Here {
        Here {
            next: Some(format!("tilo migrate review {}", row.key)),
            ..Here::default()
        }
    }

    fn review(&self, cx: &Cx<'_>, row: &'static Row, _: Option<&str>) -> Result<Review> {
        let mut review = review(cx.home, row.key)?;
        super::keep_chosen(&mut review, cx.place.review(row.key)?.as_ref());
        Ok(review)
    }

    fn summary(&self, row: &'static Row, review: &Review) -> String {
        files::summary(review, row.title)
    }

    fn files_under(&self, _: &'static Row, _: &super::state::Review) -> Option<&'static str> {
        Some(UNDER)
    }

    fn land(
        &self,
        cx: &Cx<'_>,
        _: &'static Row,
        review: &Review,
        sink: Option<&files::Sink>,
        lander: &mut Lander<'_>,
    ) -> Result<()> {
        let sink = sink.ok_or_else(|| anyhow!("files have nowhere to go"))?;
        files::land(
            &Local {
                home: cx.home.to_path_buf(),
            },
            review,
            sink,
            cx.place,
            lander,
        )
    }

    fn landed(&self, _: &Cx<'_>, _: &'static Row, journal: &Journal) -> Result<usize> {
        Ok(journal
            .brought
            .values()
            .sum::<usize>()
            .saturating_add(journal.already_there))
    }
}

/// Every hidden file and folder at the top of `home`, counted.
fn review(home: &Path, source: &str) -> Result<Review> {
    let local = Local {
        home: home.to_path_buf(),
    };
    let mut names: Vec<String> = std::fs::read_dir(home)
        .map_err(|error| anyhow!("{} could not be read: {error}", home.display()))?
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() || kind.is_file())
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.') && !LEFT_OUT.contains(&name.as_str()))
        .collect();
    names.sort();
    let mut unreadable = Vec::new();
    let items = names
        .into_iter()
        .map(|name| {
            let (count, bytes) = match local.files(&name) {
                Ok((found, unread)) => {
                    unreadable.extend(
                        unread
                            .iter()
                            .map(|problem| format!("{}: {}", problem.name, problem.why)),
                    );
                    (found.len(), found.iter().map(|entry| entry.size).sum())
                }
                Err(error) => {
                    unreadable.push(format!("{name}: {error:#}"));
                    (0, 0)
                }
            };
            Item {
                note: note_of(&name).map(str::to_owned),
                key: name.clone(),
                kind: "folder".to_owned(),
                title: name,
                count,
                counts: format!("files, {}", super::size(bytes)),
                within: None,
                chosen: false,
            }
        })
        .collect();
    Ok(Review {
        source: source.to_owned(),
        title: format!("Settings and hidden files in {}", home.display()),
        handle: home.display().to_string(),
        items,
        unreadable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_home_lists_its_hidden_files_and_leaves_its_caches_out() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".config/app")).unwrap();
        std::fs::write(home.path().join(".config/app/settings.ini"), "x=1").unwrap();
        std::fs::write(home.path().join(".bashrc"), "alias l=ls").unwrap();
        std::fs::create_dir_all(home.path().join(".cache/big")).unwrap();
        std::fs::create_dir_all(home.path().join("Documents")).unwrap();
        let review = review(home.path(), "settings").unwrap();
        let keys: Vec<&str> = review.items.iter().map(|item| item.key.as_str()).collect();
        assert_eq!(keys, [".bashrc", ".config"]);
        assert_eq!(review.items[1].count, 1);
        assert!(review.chosen().is_empty());
    }
}

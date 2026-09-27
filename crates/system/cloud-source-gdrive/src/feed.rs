//! The cursor change feed over `files.list` and `changes.list`.
//!
//! Drive's `changes.list` starts "now" (`changes.getStartPageToken`) and
//! covers the whole account, so a [`FeedCursor`] is more than a page token:
//!
//! - `token`: the `changes.list` page token to continue from;
//! - `queue`/`batch`/`page`: folders whose children still have to be
//!   listed before the feed continues. A fresh cursor queues the root (a
//!   full enumeration); a folder that moves into the subtree is queued too,
//!   because Drive reports the folder but not its unchanged contents.
//!
//! Folders are listed several at a time (`'a' in parents or 'b' in
//! parents …`), one `files.list` page per call.

use std::time::{Duration, Instant};

use cloud_source::{Change, ChangePage, Cursor, Error, ItemId, Result, Wait};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::api::{self, ChangeList, StartPageToken};
use crate::{Inner, lock};

/// Folders per enumeration query; keeps the query string well under
/// Drive's limit.
const BATCH: usize = 20;
/// How deep the ancestry walk goes before an item counts as outside.
const MAX_DEPTH: usize = 128;

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct FeedCursor {
    #[serde(rename = "t")]
    token: String,
    #[serde(rename = "q", default, skip_serializing_if = "Vec::is_empty")]
    queue: Vec<String>,
    #[serde(rename = "b", default, skip_serializing_if = "Vec::is_empty")]
    batch: Vec<String>,
    #[serde(rename = "p", default, skip_serializing_if = "Option::is_none")]
    page: Option<String>,
}

impl FeedCursor {
    fn parse(cursor: &Cursor) -> Result<Self> {
        serde_json::from_str(&cursor.0).map_err(|_| Error::CursorExpired)
    }

    fn enumerating(&self) -> bool {
        !self.queue.is_empty() || !self.batch.is_empty()
    }

    fn enqueue(&mut self, folder: &str) {
        if !self.queue.iter().chain(&self.batch).any(|q| q == folder) {
            self.queue.push(folder.to_owned());
        }
    }

    fn to_cursor(&self) -> Result<Cursor> {
        serde_json::to_string(self)
            .map(Cursor)
            .map_err(|_| Error::InvalidResponse("cursor"))
    }
}

impl Inner {
    fn start_page_token(&self) -> Result<String> {
        let mut params = vec![("supportsAllDrives", "true")];
        if let Some(drive) = &self.config.shared_drive {
            params.push(("driveId", drive.as_str()));
        }
        let token: StartPageToken = Self::json(self.call(
            Method::GET,
            &["drive", "v3", "changes", "startPageToken"],
            &params,
            None,
        )?)?;
        if token.start_page_token.is_empty() {
            return Err(Error::InvalidResponse("startPageToken"));
        }
        Ok(token.start_page_token)
    }

    fn changes_page(&self, token: &str, page_size: &str, fields: &str) -> Result<ChangeList> {
        let mut params = self.scope();
        // `changes.list` takes `driveId` but not `corpora`.
        params.retain(|(name, _)| *name != "corpora");
        params.extend([
            ("pageToken", token.to_owned()),
            ("pageSize", page_size.to_owned()),
            ("includeRemoved", "true".to_owned()),
            ("fields", fields.to_owned()),
        ]);
        let params: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let url = self.url(&["drive", "v3", "changes"], &params)?;
        let response = Self::send(self.request(Method::GET, url)?)?;
        if !response.status().is_success() {
            return Err(self.failure(response, true));
        }
        Self::json(response)
    }

    pub(crate) fn changes(&self, cursor: Option<&Cursor>) -> Result<ChangePage> {
        let (mut state, reset) = match cursor {
            None => (
                FeedCursor {
                    token: self.start_page_token()?,
                    queue: vec![self.real_root()?],
                    ..FeedCursor::default()
                },
                true,
            ),
            Some(cursor) => (FeedCursor::parse(cursor)?, false),
        };
        let (changes, has_more) = if state.enumerating() {
            // Whatever follows (more folders or the feed's catch-up) is
            // available now.
            (self.enumerate(&mut state)?, true)
        } else {
            self.follow(&mut state)?
        };
        Ok(ChangePage {
            changes,
            cursor: state.to_cursor()?,
            has_more,
            reset,
        })
    }

    /// One `files.list` page over the next batch of queued folders.
    fn enumerate(&self, state: &mut FeedCursor) -> Result<Vec<Change>> {
        if state.batch.is_empty() {
            let take = BATCH.min(state.queue.len());
            state.batch = state.queue.drain(..take).collect();
            state.page = None;
        }
        let parents: Vec<String> = state
            .batch
            .iter()
            .map(|id| format!("{} in parents", api::quote(id)))
            .collect();
        let query = format!("({}) and trashed = false", parents.join(" or "));
        let list = self.list_page(&query, state.page.as_deref())?;
        let root = self.real_root()?;
        let mut changes = Vec::with_capacity(list.files.len());
        for file in &list.files {
            if file.id == root {
                continue;
            }
            if file.is_folder() {
                state.enqueue(&file.id);
            }
            changes.push(Change::Upsert(self.to_item(file)?));
        }
        state.page = list.next_page_token;
        if state.page.is_none() {
            state.batch.clear();
        }
        Ok(changes)
    }

    /// One `changes.list` page, filtered to the subtree.
    fn follow(&self, state: &mut FeedCursor) -> Result<(Vec<Change>, bool)> {
        let fields = format!(
            "nextPageToken,newStartPageToken,changes(changeType,removed,fileId,file({}))",
            api::FILE_FIELDS
        );
        let page = self.changes_page(&state.token, "1000", &fields)?;
        let root = self.real_root()?;
        let mut changes = Vec::with_capacity(page.changes.len());
        for entry in page.changes {
            if entry.change_type.as_deref() == Some("drive") {
                continue;
            }
            let Some(id) = entry
                .file_id
                .clone()
                .or_else(|| entry.file.as_ref().map(|f| f.id.clone()))
            else {
                continue;
            };
            if id == root {
                continue;
            }
            match entry.file {
                Some(file) if !entry.removed && !file.trashed => {
                    let was_inside = if file.is_folder() {
                        self.known_inside(&id, &root)?
                    } else {
                        None
                    };
                    if file.is_folder() {
                        self.remember_folder(&id, file.parents.clone());
                    }
                    if self.inside(&file.parents, &root, MAX_DEPTH)? {
                        if file.is_folder() && was_inside != Some(true) {
                            state.enqueue(&id);
                        }
                        changes.push(Change::Upsert(self.to_item(&file)?));
                    } else {
                        changes.push(Change::Remove(ItemId::new(id)));
                    }
                }
                _ => {
                    // Trashed or gone; a folder no longer leads anywhere.
                    self.remember_folder(&id, Vec::new());
                    changes.push(Change::Remove(ItemId::new(id)));
                }
            }
        }
        let more_feed = page.next_page_token.is_some();
        state.token = page
            .next_page_token
            .or(page.new_start_page_token)
            .ok_or(Error::InvalidResponse("changes page without a token"))?;
        Ok((changes, more_feed || state.enumerating()))
    }

    /// Whether a folder already known was inside the subtree, without
    /// asking the server about the folder itself.
    fn known_inside(&self, folder: &str, root: &str) -> Result<Option<bool>> {
        let parents = lock(&self.folders).get(folder).cloned();
        match parents {
            None => Ok(None),
            Some(parents) => self.inside(&parents, root, MAX_DEPTH).map(Some),
        }
    }

    /// Whether an item with `parents` lies below `root`, walking up through
    /// known folders and asking the server about unknown ones.
    fn inside(&self, parents: &[String], root: &str, depth: usize) -> Result<bool> {
        if parents.iter().any(|p| p == root) {
            return Ok(true);
        }
        let Some(depth) = depth.checked_sub(1) else {
            return Ok(false);
        };
        for parent in parents {
            let known = lock(&self.folders).get(parent).cloned();
            let grandparents = if let Some(known) = known {
                known
            } else {
                let fetched = match self.get_file(parent, "id,parents,trashed") {
                    Ok(file) if !file.trashed => file.parents,
                    Ok(_) | Err(Error::NotFound | Error::Forbidden) => Vec::new(),
                    Err(error) => return Err(error),
                };
                self.remember_folder(parent, fetched.clone());
                fetched
            };
            if self.inside(&grandparents, root, depth)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Poll the feed until it has entries after `cursor` or `timeout`
    /// passes. Wakes for any change in the account (or shared drive); the
    /// next [`Inner::changes`] call filters.
    pub(crate) fn wait(&self, cursor: Option<&Cursor>, timeout: Duration) -> Result<Wait> {
        let Some(cursor) = cursor else {
            return Ok(Wait::Changed);
        };
        let state = FeedCursor::parse(cursor)?;
        if state.enumerating() {
            return Ok(Wait::Changed);
        }
        let started = Instant::now();
        loop {
            match self.changes_page(
                &state.token,
                "1",
                "nextPageToken,newStartPageToken,changes(fileId)",
            ) {
                Ok(page) if !page.changes.is_empty() || page.next_page_token.is_some() => {
                    return Ok(Wait::Changed);
                }
                Ok(_) => {}
                Err(Error::RateLimited {
                    retry_after: Some(delay),
                }) => return Ok(Wait::Backoff(delay)),
                Err(error) => return Err(error),
            }
            let elapsed = started.elapsed();
            let Some(left) = timeout.checked_sub(elapsed).filter(|d| !d.is_zero()) else {
                return Ok(Wait::Idle);
            };
            std::thread::sleep(left.min(self.config.poll_interval));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_garbage_expires() {
        let state = FeedCursor {
            token: "8930".into(),
            queue: vec!["1a".into()],
            batch: vec!["0root".into()],
            page: Some("~!!~AI9FV7T".into()),
        };
        let cursor = state.to_cursor().unwrap();
        assert_eq!(FeedCursor::parse(&cursor).unwrap(), state);
        let idle = FeedCursor {
            token: "8931".into(),
            ..FeedCursor::default()
        };
        assert_eq!(idle.to_cursor().unwrap().0, r#"{"t":"8931"}"#);
        assert!(matches!(
            FeedCursor::parse(&Cursor("8930".into())),
            Err(Error::CursorExpired)
        ));
    }
}

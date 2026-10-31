//! Records held by the workspaces on this device, found by name.
//!
//! A workspace starts empty. Moving a record into it costs one step: type
//! the record's name, the row says which workspace it lives in, and Enter
//! **brings it here**.
//!
//! Bringing is referencing, not copying: the row's address names the
//! record's id and kind, and what it runs adds this workspace's reference
//! to that record in the workspace's record index — no bytes move and no id
//! is minted. Copying a record is a different, deliberate verb
//! (`ui::record::room::duplicate`).
//!
//! The rows come from the workspaces' record indexes: the source only ranks
//! the list a host collected, so it never reads a workspace's state itself.

use ui::record::{RecordCheckout, RecordId, RecordKind};
use ui::search::system::ResourceKind;

use crate::model::{DESKTOP_APP, Hit, InvokeTarget, Source, Target};

/// The resource the session answers for the records it holds, and the
/// action in it naming `record` as held by the workspace it runs in.
pub const RECORDS_RESOURCE: &str = "desktop:records";
pub const BRING_HERE: &str = "bring-here";
/// The parameters carrying what is brought here: the record's id, its kind
/// and the app whose record index names it.
pub const RECORD_PARAM: &str = "record";
pub const KIND_PARAM: &str = "kind";
pub const PACKAGE_PARAM: &str = "package";

/// Rows one query contributes; a long tail here only costs memory, not the
/// palette's shared row budget.
const MAX_RESULTS: usize = 20;

/// A record a workspace holds, as that workspace's record index names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldRecord {
    /// The record's identity, as the collaboration wire carries it.
    pub id: String,
    /// What kind of record it is; empty when the index does not say.
    pub kind: String,
    /// The app whose record index names it: what the reference is written
    /// into when the record is brought here.
    pub package: String,
    /// Whether the workspace search was opened in holds it.
    pub here: bool,
    /// The name of the workspace holding it, when that is another one; a
    /// record of a workspace this device no longer shows has no name.
    pub workspace: Option<String>,
}

impl HeldRecord {
    /// The record, as its index names it.
    #[must_use]
    pub fn checkout(&self) -> Option<RecordCheckout> {
        Some(RecordCheckout::new(
            RecordId::parse(&self.id)?,
            RecordKind::new(self.kind.clone()),
        ))
    }

    /// What people call the record. Its identity names a file when it is
    /// one: a file record's leaf is its name, and a minted id (`uuid:…`)
    /// has none, so its kind stands for it.
    #[must_use]
    pub fn title(&self) -> String {
        let Some(id) = RecordId::parse(&self.id) else {
            return self.id.clone();
        };
        if let Some(path) = id.file_path() {
            return path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().to_string(),
            );
        }
        if let Some(rest) = self.id.strip_prefix("derived:")
            && let Some((role, target)) = rest.split_once('@')
        {
            return format!("{role} of {target}");
        }
        if self.kind.is_empty() {
            self.id.clone()
        } else {
            self.kind.clone()
        }
    }

    /// The line under the title: where this record lives. Not decoration —
    /// without it, two workspaces' `notes.md` are identical rows and
    /// bringing one here is a guess.
    fn facts(&self) -> Vec<String> {
        let where_it_lives = if self.here {
            crate::i18n::t("search-record-in-this-workspace")
        } else {
            match &self.workspace {
                Some(name) => crate::i18n::t_args(
                    "search-record-in-workspace",
                    &[("workspace", name.as_str().into())],
                ),
                // No workspace for this record (one this device stopped
                // showing, or whose name it does not know): say so rather
                // than print a room id.
                None => crate::i18n::t("search-record-in-another-workspace"),
            }
        };
        let mut facts = vec![where_it_lives];
        match RecordId::parse(&self.id).and_then(|id| id.file_path()) {
            Some(path) => facts.push(display_path(&path)),
            None if !self.kind.is_empty() => facts.push(self.kind.clone()),
            None => {}
        }
        facts
    }
}

/// `~`-shortened path for a row's second line: a file's whole path under the
/// person's home crowds out everything the row has to say.
fn display_path(path: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from)
        && !home.as_os_str().is_empty()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

/// The records the workspaces hold, offered in the workspace of `room`.
pub struct Records {
    /// The workspace search was opened in: where a chosen row brings its
    /// record, and which rows say "in this workspace".
    room: String,
    held: Vec<HeldRecord>,
}

impl Records {
    #[must_use]
    pub fn new(room: impl Into<String>, held: Vec<HeldRecord>) -> Self {
        Self {
            room: room.into(),
            held,
        }
    }

    fn hit(&self, needle: &str, record: &HeldRecord) -> Option<Hit> {
        let score = crate::rank::record_match_score(needle, &record.title(), &record.id)?;
        let facts = record.facts();
        let checkout = record.checkout()?;
        let target = InvokeTarget::action(DESKTOP_APP, &self.room, RECORDS_RESOURCE, BRING_HERE)
            .with_argument(RECORD_PARAM, record.id.clone())
            .with_argument(KIND_PARAM, checkout.kind.as_str())
            .with_argument(PACKAGE_PARAM, record.package.clone());
        Some(Hit {
            kind: ResourceKind::Record,
            provider: self.id().into(),
            id: record.id.clone(),
            title: record.title(),
            subtitle: Some(facts.join(", ")),
            facts,
            score,
            icon: None,
            target: Target::Invoke(Box::new(target)),
        })
    }
}

impl Source for Records {
    fn id(&self) -> &'static str {
        "records"
    }

    fn name(&self) -> String {
        crate::i18n::t("search-source-records")
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        // A blank query asks for the browse set, and a record's name is
        // what a person knows it by: there is nothing to browse here.
        if needle.trim().is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<Hit> = self
            .held
            .iter()
            .filter_map(|record| self.hit(needle, record))
            .collect();
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.title.cmp(&b.title)));
        hits.truncate(MAX_RESULTS);
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(id: &str, kind: &str, here: bool, workspace: Option<&str>) -> HeldRecord {
        HeldRecord {
            id: id.into(),
            kind: kind.into(),
            package: "so.tilo.notes".into(),
            here,
            workspace: workspace.map(str::to_owned),
        }
    }

    #[test]
    fn a_record_of_another_workspace_is_found_by_name_and_brought_here() {
        let records = Records::new(
            "here",
            vec![held(
                "file:/work/Quarterly plan.odt",
                "document",
                false,
                Some("Lounge"),
            )],
        );
        let hits = records.query("quarterly");
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        assert_eq!(hit.title, "Quarterly plan.odt");
        assert_eq!(hit.kind, ResourceKind::Record);
        assert_eq!(
            hit.facts[0], "in Lounge",
            "the row says which workspace holds it"
        );
        let Target::Invoke(target) = &hit.target else {
            panic!("a record is brought here by address");
        };
        assert_eq!(target.app, DESKTOP_APP);
        assert_eq!(
            target.address,
            tilo_actions::ActionAddress::new(RECORDS_RESOURCE, BRING_HERE)
        );
        assert_eq!(
            target.arguments[RECORD_PARAM], "file:/work/Quarterly plan.odt",
            "the record travels as its own id: no bytes, no new identity"
        );
        assert_eq!(
            target.arguments[KIND_PARAM], "tilo.rich-text",
            "the row carries the kind as the index keeps it: a former name of \
             a kind is the kind it names"
        );
        assert_eq!(target.arguments[PACKAGE_PARAM], "so.tilo.notes");
        assert_eq!(target.room, "here", "and it is brought into this workspace");
        assert!(records.query("zzz").is_empty());
        assert!(records.query("").is_empty());
    }

    #[test]
    fn a_record_held_here_says_so_and_a_kind_names_a_minted_one() {
        let records = Records::new(
            "here",
            vec![
                held("file:/work/notes.md", "document", true, None),
                held(
                    "uuid:6c0a2f2e-0d5f-4d3e-9f5e-2b8b0b0c9d11",
                    "note",
                    false,
                    None,
                ),
            ],
        );
        let here = records.query("notes");
        assert_eq!(here[0].facts[0], "in this workspace");
        // The minted record's own identity, which no other row carries.
        let minted = records.query("6c0a2f2e");
        assert_eq!(minted.len(), 1);
        assert_eq!(
            minted[0].title, "note",
            "a minted id has no name but its kind"
        );
        assert_eq!(minted[0].facts[0], "in another workspace");
    }
}

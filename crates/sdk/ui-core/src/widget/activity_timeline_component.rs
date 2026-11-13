//! [`ActivityTimeline`] as a described component: the changes of a document as entries in a slot, each
//! what it did, who did it and when, and reverting or opening one reported with the change it stands
//! for.

use std::collections::HashMap;

use crate::component::{Component, Described, Event, Moment, Person, ResourceId, Value};
use crate::time::{Duration, UNIX_EPOCH};
use crate::widget::history::{EntryKind, HistoryAuthor, HistoryEntry, HistoryEntryId};
use crate::widget::{ActivityTimeline, ArtifactKind, Widget};

crate::component::choice!(crate::widget::EntryKind, "EntryKind", {
    Text => "text",
    Move => "move",
    Details => "details",
    Comment => "comment",
    Delete => "delete",
    Restore => "restore",
    Access => "access",
    Compaction => "compaction",
    Redaction => "redaction",
});

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActivityArtifact {
    #[default]
    Note,
    File,
    Cluster,
    Kanban,
    ChatLog,
}

crate::component::choice!(ActivityArtifact, "ActivityArtifact", {
    Note => "note",
    File => "file",
    Cluster => "cluster",
    Kanban => "kanban",
    ChatLog => "chat-log",
});

/// One change of a document: what it did, who did it and when.
#[derive(Clone, Component, Debug, Default)]
#[component(
    name = "ActivityEntry",
    role = "ListItem",
    label = "summary",
    presents = "item"
)]
pub struct ActivityEntryProps {
    /// The change it is: its stable key, what reverting or opening it reports, and where its band and
    /// selection stand across changes.
    #[component(required, positional)]
    pub item: ResourceId,
    /// Who made the change.
    pub author: Option<Person>,
    /// When it was made.
    pub time: Option<Moment>,
    /// What the change did, in a sentence: "Renamed the card", "Moved to Done".
    #[component(required, positional)]
    pub summary: String,
    /// What kind of change it is: it draws the row's glyph and says whether it can be reverted.
    pub kind: Option<EntryKind>,
    /// What kind of resource it changed.
    pub artifact: Option<ActivityArtifact>,
}

crate::component::part_slot!(ActivityEntryProps);

/// The history of a document, grouped: runs of typing fold into one band that opens to its changes,
/// each day's changes sit under the day's heading, and a change's menu offers reverting it.
#[derive(Component, Default)]
#[component(name = "ActivityTimeline", role = "List")]
pub struct ActivityTimelineProps {
    /// The pane takes the height its parent gives it and scrolls inside, rather than being as tall as its
    /// rows inside another scroll.
    pub fill: bool,
    /// Whether it lists changes newest first.
    pub newest_first: bool,
    /// Whether the pane shows the calendar's Today/Yesterday/This week headings.
    pub day_headings: bool,
    /// Whether it offers every person and agent in the history as a filter.
    pub people_filters: bool,
    /// The changes, oldest first.
    #[component(slot(default))]
    pub entries: Vec<ActivityEntryProps>,
    /// A change was reverted: carries the change it is.
    #[component(event(value = ResourceId))]
    pub revert: Event,
    /// A change was opened in the app: carries the change it is.
    #[component(event(value = ResourceId))]
    pub open: Event,
}

/// A described timeline: its timeline, and the change each key stands for.
pub struct DescribedActivityTimeline {
    timeline: ActivityTimeline,
    keys: HashMap<String, HistoryEntryId>,
    face: Face,
}

#[derive(Clone, Debug, PartialEq)]
struct Face {
    fill: bool,
    newest_first: bool,
    day_headings: bool,
    people_filters: bool,
    /// Each change's key, its text, its author and its time: a change of these builds it again.
    entries: Vec<(String, String, Option<String>, Option<i64>)>,
}

impl DescribedActivityTimeline {
    #[must_use]
    pub fn timeline(&self) -> &ActivityTimeline {
        &self.timeline
    }

    pub fn timeline_mut(&mut self) -> &mut ActivityTimeline {
        &mut self.timeline
    }
}

fn change(key: &str) -> Value {
    Value::Resource(key.to_owned())
}

/// The id history gives a change, from its key: the first sixteen bytes of the key's own bytes, so
/// a change the view names is the same change each time the view is built again.
fn entry_id(key: &str) -> HistoryEntryId {
    let mut bytes = [0u8; 16];
    for (slot, byte) in bytes.iter_mut().zip(key.as_bytes()) {
        *slot = *byte;
    }
    HistoryEntryId(bytes)
}

fn entry_of(props: &ActivityEntryProps) -> HistoryEntry {
    let id = entry_id(&props.item.0);
    let author = props.author.clone().unwrap_or_default();
    let timestamp = props.time.map_or(UNIX_EPOCH, |Moment(at)| {
        let at = u64::try_from(at.max(0)).unwrap_or(0);
        UNIX_EPOCH
            .checked_add(Duration::from_secs(at))
            .unwrap_or(UNIX_EPOCH)
    });
    HistoryEntry {
        id,
        author: HistoryAuthor::new(
            author.name.clone(),
            0.0,
            author.identity.chars().take(8).collect::<String>(),
        ),
        entry_kind: props.kind.unwrap_or(EntryKind::Details),
        artifact_kind: artifact_of(props.artifact.unwrap_or_default()),
        timestamp,
        summary: props.summary.clone(),
        text_delta: None,
        diff_payload: None,
        redaction: None,
        context: None,
    }
}

fn artifact_of(kind: ActivityArtifact) -> ArtifactKind {
    match kind {
        ActivityArtifact::Note => ArtifactKind::Note,
        ActivityArtifact::File => ArtifactKind::File,
        ActivityArtifact::Cluster => ArtifactKind::Cluster,
        ActivityArtifact::Kanban => ArtifactKind::Kanban,
        ActivityArtifact::ChatLog => ArtifactKind::ChatLog,
    }
}

impl Described for DescribedActivityTimeline {
    type Props = ActivityTimelineProps;

    fn from_props(_key: &str, props: ActivityTimelineProps) -> Self {
        let ActivityTimelineProps {
            fill,
            newest_first,
            day_headings,
            people_filters,
            entries,
            revert,
            open,
        } = props;
        let keys: HashMap<String, HistoryEntryId> = entries
            .iter()
            .map(|entry| (entry.item.0.clone(), entry_id(&entry.item.0)))
            .collect();
        let mut timeline =
            ActivityTimeline::with_entries(entries.iter().map(entry_of).collect()).with_fill(fill);
        timeline.set_newest_first(newest_first);
        timeline.set_show_day_headings(day_headings);
        timeline.set_filters_people(people_filters);
        let ids: HashMap<HistoryEntryId, String> =
            keys.iter().map(|(key, id)| (*id, key.clone())).collect();
        if !revert.is_none() {
            let ids = ids.clone();
            timeline = timeline.on_revert(move |id: &HistoryEntryId| {
                if let Some(key) = ids.get(id) {
                    revert.fire(Some(Value::Resource(key.clone())));
                }
            });
        }
        if !open.is_none() {
            let ids = ids.clone();
            timeline = timeline.on_activate_entry(move |id: &HistoryEntryId| {
                if let Some(key) = ids.get(id) {
                    open.fire(Some(Value::Resource(key.clone())));
                }
            });
        }
        Self {
            timeline,
            keys,
            face: Face {
                fill,
                newest_first,
                day_headings,
                people_filters,
                entries: entries
                    .iter()
                    .map(|entry| {
                        (
                            entry.item.0.clone(),
                            entry.summary.clone(),
                            entry.author.as_ref().map(|who| who.name.clone()),
                            entry.time.map(|Moment(at)| at),
                        )
                    })
                    .collect(),
            },
        }
    }

    /// Changed changes, and changed headings and order, are taken in place: the band a person folded
    /// and the selection stay.
    fn set_props(&mut self, props: ActivityTimelineProps) -> bool {
        let ActivityTimelineProps {
            fill,
            newest_first,
            day_headings,
            people_filters,
            entries,
            revert: _,
            open: _,
        } = props;
        let face = Face {
            fill,
            newest_first,
            day_headings,
            people_filters,
            entries: entries
                .iter()
                .map(|entry| {
                    (
                        entry.item.0.clone(),
                        entry.summary.clone(),
                        entry.author.as_ref().map(|who| who.name.clone()),
                        entry.time.map(|Moment(at)| at),
                    )
                })
                .collect(),
        };
        if face != self.face {
            return false;
        }
        if entries
            .iter()
            .all(|entry| self.keys.contains_key(&entry.item.0))
            && entries.len() == self.keys.len()
        {
            return true;
        }
        self.keys = entries
            .iter()
            .map(|entry| (entry.item.0.clone(), entry_id(&entry.item.0)))
            .collect();
        self.timeline
            .set_entries(entries.iter().map(entry_of).collect());
        true
    }
}

impl crate::widget::Decorator for DescribedActivityTimeline {
    type Inner = ActivityTimeline;

    fn inner(&self) -> &ActivityTimeline {
        &self.timeline
    }

    fn inner_mut(&mut self) -> &mut ActivityTimeline {
        &mut self.timeline
    }

    fn name(&self) -> &'static str {
        "ActivityTimeline"
    }

    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        self.timeline.take_structure_change()
    }
}

#[allow(dead_code, reason = "the change an event carries is its key")]
fn _report(key: &str) -> Value {
    change(key)
}

#[allow(dead_code, reason = "the author is part of the entry")]
fn _person() -> Option<Person> {
    None
}

//! Items keyed by the ids they carry, values stored as JSON, and the field
//! edits between two values.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "test assertions over exact values written and read back"
)]

use serde::{Deserialize, Serialize};
use tilo_resource::{
    Collection, Counter, Id, Identified, Invoke, Keyed, Model, Path, Replica, Set, resource,
};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Clip {
    pub id: String,
    #[model(label)]
    pub name: String,
    pub start: f64,
    pub curve: Curve,
}

impl Identified for Clip {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }
    fn order(&self, other: &Self) -> std::cmp::Ordering {
        self.start.total_cmp(&other.start)
    }
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Curve {
    pub points: Vec<(f64, f64)>,
}

tilo_resource::json_model!(Curve);

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Timeline {
    pub name: String,
    pub clips: Keyed<Clip>,
    pub tags: Set<String>,
    pub plays: Counter,
    pub notes: Collection<String>,
    pub colour: [f64; 4],
}

#[resource(kind = "test.timeline", label = "Timeline")]
impl Timeline {
    /// Add a clip with an id of its own.
    #[action]
    pub fn add_generated(&mut self, start: f64) {
        self.clips.push(Clip {
            id: tilo_resource::fresh_uuid().to_string(),
            start,
            ..Clip::default()
        });
    }

    /// Add a clip.
    #[action]
    pub fn add_clip(&mut self, id: String, name: String, start: f64) {
        self.clips.push(Clip {
            id,
            name,
            start,
            ..Clip::default()
        });
    }
}

fn clip(id: &str, start: f64) -> Clip {
    Clip {
        id: id.into(),
        name: id.into(),
        start,
        curve: Curve::default(),
    }
}

fn submit(timeline: &mut Replica<Timeline>, edits: &[tilo_resource::Edit<Timeline>]) {
    let members = edits
        .iter()
        .map(|edit| Invoke::edit(edit).unwrap())
        .collect::<Vec<_>>();
    timeline.apply(Invoke::group(members)).unwrap();
}

#[test]
fn keyed_items_sit_under_their_ids_and_merge_per_field() {
    let mut timeline = Replica::<Timeline>::new();
    timeline
        .apply(Timeline::ADD_CLIP.with(("b".to_owned(), "B".to_owned(), 1.0)))
        .unwrap();
    timeline
        .apply(Timeline::ADD_CLIP.with(("a".to_owned(), "A".to_owned(), 5.0)))
        .unwrap();
    let state = timeline.state().unwrap();
    // In memory by their own order: start time.
    assert_eq!(
        state
            .clips
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        ["b", "a"]
    );
    let mut other = timeline.fork();
    let paths = Timeline::paths();
    submit(
        &mut timeline,
        &[paths.clips.item(&Id::from_raw("a")).start.set(0.5)],
    );
    submit(
        &mut other,
        &[paths.clips.item(&Id::from_raw("a")).name.replace("Alpha")],
    );
    timeline.sync_with(&mut other).unwrap();
    let merged = timeline.state().unwrap();
    assert_eq!(merged, other.state().unwrap());
    let a = merged.clips.find(&Id::from_raw("a")).unwrap();
    assert_eq!((a.name.as_str(), a.start), ("Alpha", 0.5));
    assert_eq!(merged.clips[0].id, "a", "re-sorted by start");
    assert_eq!(timeline.live().unwrap(), &merged);
}

#[test]
fn the_edits_between_two_values_turn_one_into_the_other() {
    let mut timeline = Replica::<Timeline>::new();
    let before = timeline.state().unwrap();
    let mut after = before.clone();
    after.name = "Cut".into();
    after.clips.push(clip("x", 2.0));
    after.clips.push(clip("y", 3.0));
    after.tags.insert("draft".into());
    after.plays.add(2);
    after.notes.push(Id::from_raw("n1"), "hello".into());
    submit(&mut timeline, &Path::root().edits(&before, &after));
    let mut read = timeline.state().unwrap();
    read.clips.settle();
    after.clips.settle();
    assert_eq!(read, after);

    let before = read;
    let mut after = before.clone();
    after.clips.find_mut(&Id::from_raw("x")).unwrap().start = 9.0;
    after
        .clips
        .find_mut(&Id::from_raw("x"))
        .unwrap()
        .curve
        .points = vec![(0.0, 1.0)];
    after.clips.take(&Id::from_raw("y"));
    after.tags.remove(&"draft".to_owned());
    let edits = Timeline::paths().path().edits(&before, &after);
    assert_eq!(edits.len(), 4, "{edits:?}");
    submit(&mut timeline, &edits);
    let read = timeline.state().unwrap();
    assert_eq!(read, after);
    assert_eq!(read.clips[0].curve.points, [(0.0, 1.0)]);
    assert!(Timeline::paths().path().edits(&read, &after).is_empty());
}

#[test]
fn ids_made_inside_a_verb_replay_identically_and_surfaces_bind_verbs() {
    let mut origin = Replica::<Timeline>::new();
    let mut peer = origin.fork();
    origin.apply(Timeline::ADD_GENERATED.with(1.0)).unwrap();
    origin.apply(Timeline::ADD_GENERATED.with(2.0)).unwrap();
    for envelope in origin.log().to_vec() {
        peer.replay(&envelope).unwrap();
    }
    assert_eq!(origin.state().unwrap(), peer.state().unwrap());
    let ids: Vec<String> = origin
        .state()
        .unwrap()
        .clips
        .iter()
        .map(|c| c.id.clone())
        .collect();
    assert_ne!(ids[0], ids[1]);
    assert_ne!(tilo_resource::fresh_uuid(), tilo_resource::fresh_uuid());

    let here = Timeline::ADD_CLIP
        .bind(timeline::AddClip::paths().start, 7.5)
        .declaration("add-clip-here", "Add a clip here")
        .unwrap();
    assert_eq!(
        here.params
            .iter()
            .map(|p| p.name.to_string())
            .collect::<Vec<_>>(),
        ["id", "name"]
    );
    let args = tilo_actions::ActionArgs::bind_json(
        &here.params,
        &serde_json::Map::from_iter([
            ("id".to_owned(), serde_json::json!("z")),
            ("name".to_owned(), serde_json::json!("Zed")),
        ]),
    )
    .unwrap();
    let action = (here.build)(&args, 0).unwrap();
    origin.apply_action(&action).unwrap();
    assert_eq!(
        origin
            .state()
            .unwrap()
            .clips
            .find(&Id::from_raw("z"))
            .unwrap()
            .start,
        7.5
    );

    submit(
        &mut origin,
        &[Timeline::paths().colour.set([1.0, 0.5, 0.0, 1.0])],
    );
    assert_eq!(origin.state().unwrap().colour, [1.0, 0.5, 0.0, 1.0]);
}

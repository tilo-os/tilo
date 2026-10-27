#![allow(
    clippy::unwrap_used,
    clippy::float_cmp,
    reason = "test assertions over exact values written and read back"
)]

use serde::{Deserialize, Serialize};
use tilo_resource::{
    Collection, Counter, Fields, Id, Identified, Invoke, Keyed, Model, Ordered, Path, Replica,
    Resource, Set, resource,
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
    pub stack: Ordered<Clip>,
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
    // Confirmed: some remove items, which asks first.
    timeline.apply_confirmed(Invoke::group(members)).unwrap();
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
    let action = here.build_action(&args, 0).unwrap();
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

fn stack_ids(timeline: &Timeline) -> Vec<&str> {
    timeline.stack.iter().map(|c| c.id.as_str()).collect()
}

#[test]
fn ordered_items_keep_the_arranged_order_and_merge_by_identity() {
    let mut timeline = Replica::<Timeline>::new();
    let paths = Timeline::paths();
    submit(
        &mut timeline,
        &[
            paths.stack.insert(0, &clip("a", 9.0)),
            paths.stack.insert(1, &clip("b", 1.0)),
            paths.stack.insert(2, &clip("c", 5.0)),
        ],
    );
    let state = timeline.state().unwrap();
    assert_eq!(
        stack_ids(&state),
        ["a", "b", "c"],
        "the arranged order, not the items' own"
    );

    // One collaborator moves `c` to the top and removes `a`, the other
    // changes `a` and `c` and adds `d` at the end: the moved item keeps
    // its change, the removed one stays removed, and both merge alike.
    let mut other = timeline.fork();
    let mut moved = state.clone();
    let c = moved.stack.take(&Id::from_raw("c")).unwrap();
    moved.stack.insert(0, c);
    moved.stack.take(&Id::from_raw("a"));
    let edits = paths.path().edits(&state, &moved);
    submit(&mut timeline, &edits);
    assert_eq!(stack_ids(&timeline.state().unwrap()), ["c", "b"]);
    submit(
        &mut other,
        &[
            paths.stack.item(&Id::from_raw("c")).start.set(6.0),
            paths.stack.item(&Id::from_raw("a")).name.replace("Gone"),
            paths.stack.insert(3, &clip("d", 0.0)),
        ],
    );
    timeline.sync_with(&mut other).unwrap();
    let merged = timeline.state().unwrap();
    assert_eq!(merged, other.state().unwrap());
    assert_eq!(stack_ids(&merged), ["c", "b", "d"]);
    assert_eq!(merged.stack[0].start, 6.0, "the moved item kept its change");

    // Rewriting the whole value keeps the order keys of the items that did
    // not move: one move writes one key.
    let before = merged;
    let mut after = before.clone();
    after.stack.swap(1, 2);
    let mut doc = tilo_resource::Doc::new();
    before.write_fields(&mut doc, &automerge::ROOT).unwrap();
    doc.commit();
    let heads = doc.get_heads();
    after.write_fields(&mut doc, &automerge::ROOT).unwrap();
    doc.commit();
    let changed: usize = doc
        .get_changes(&heads)
        .iter()
        .map(automerge::Change::len)
        .sum();
    assert_eq!(changed, 1, "one order key rewritten");
    assert_eq!(stack_ids(&Timeline::read(&doc).unwrap()), ["c", "d", "b"]);
    assert!(paths.path().edits(&after, &after).is_empty());
}

thread_local! {
    static KEYS_SERIALISED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn keys_serialised() -> usize {
    KEYS_SERIALISED.with(std::cell::Cell::get)
}

/// Keyframes that count how often they are serialised.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(transparent)]
pub struct Keys(pub Vec<(f64, f64)>);

impl Serialize for Keys {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        KEYS_SERIALISED.with(|serialised| serialised.set(serialised.get().saturating_add(1)));
        self.0.serialize(serializer)
    }
}

tilo_resource::json_model!(Keys);

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Reel {
    pub id: String,
    pub opacity: f64,
    pub keys: Keys,
}

impl Identified for Reel {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Film {
    pub reels: Keyed<Reel>,
}

/// The same reel with its keyframes as a JSON value, which compares values
/// only by their JSON forms.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct FormReel {
    pub id: String,
    pub opacity: f64,
    pub keys: serde_json::Value,
}

impl Identified for FormReel {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct FormFilm {
    pub reels: Keyed<FormReel>,
}

fn film(reels: usize) -> Film {
    Film {
        reels: (0..reels)
            .map(|n| Reel {
                id: format!("r{n:04}"),
                opacity: 1.0,
                keys: Keys(vec![
                    (0.0, 1.0),
                    (f64::from(u32::try_from(n).unwrap()), 0.5),
                ]),
            })
            .collect(),
    }
}

fn form_film(film: &Film) -> FormFilm {
    FormFilm {
        reels: film
            .reels
            .iter()
            .map(|reel| FormReel {
                id: reel.id.clone(),
                opacity: reel.opacity,
                keys: serde_json::to_value(&reel.keys.0).unwrap(),
            })
            .collect(),
    }
}

type EditParts = Vec<(Vec<tilo_resource::Segment>, tilo_resource::EditOp)>;

fn edit_parts<R>(edits: &[tilo_resource::Edit<R>]) -> EditParts {
    edits
        .iter()
        .map(|edit| (edit.path().to_vec(), edit.op().clone()))
        .collect()
}

#[test]
fn items_changed_in_place_compare_their_json_values_in_memory() {
    let before = film(300);
    let id = Id::from_raw("r0150");

    let mut after = before.clone();
    for reel in &mut after.reels {
        if reel.id == "r0150" {
            reel.opacity = 0.5;
        }
    }
    let serialised = keys_serialised();
    let edits = Path::<Film, Film>::root().edits(&before, &after);
    assert_eq!(
        keys_serialised().saturating_sub(serialised),
        0,
        "300 unchanged keyframe tracks compared without serialising one"
    );
    assert_eq!(edits, [Film::paths().reels.item(&id).opacity.set(0.5)]);

    let mut after = before.clone();
    let mut changed = Keys::default();
    for reel in &mut after.reels {
        if reel.id == "r0150" {
            reel.keys.0.push((9.0, 1.0));
            changed = reel.keys.clone();
        }
    }
    let serialised = keys_serialised();
    let edits = Path::<Film, Film>::root().edits(&before, &after);
    assert!(
        keys_serialised().saturating_sub(serialised) <= 4,
        "only the changed track is serialised"
    );
    assert_eq!(edits, [Film::paths().reels.item(&id).keys.set(changed)]);
}

#[test]
fn json_values_compared_in_memory_edit_as_their_forms_do() {
    let mut rng = 17_u64;
    let mut next = |bound: u64| {
        rng = rng
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from((rng >> 33) % bound).unwrap()
    };
    for seed in 0..40 {
        let before = film(30);
        let mut after = before.clone();
        for step in 0..next(6) {
            let target = format!("r{:04}", next(30));
            let at = f64::from(u32::try_from(step).unwrap());
            let change = next(5);
            for reel in &mut after.reels {
                if reel.id != target {
                    continue;
                }
                match change {
                    0 => reel.opacity = at,
                    1 => reel.keys.0.push((at, 0.25)),
                    2 => reel.keys.0.clear(),
                    // Equal in memory, another text: no edit, as by form.
                    3 => {
                        if let Some(first) = reel.keys.0.first_mut() {
                            first.0 = -first.0;
                        }
                    }
                    _ => reel.keys = Keys(vec![(0.0, 1.0)]),
                }
            }
        }
        let by_form =
            Path::<FormFilm, FormFilm>::root().edits(&form_film(&before), &form_film(&after));
        assert_eq!(
            edit_parts(&Path::<Film, Film>::root().edits(&before, &after)),
            edit_parts(&by_form),
            "seed {seed}"
        );
    }
}

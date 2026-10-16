//! What an action may write, checked where it is made, and which units a
//! change touched, read without the app's code.

use tilo_resource::footprint::{RECORD_UNIT, UNIT_INDEX, units_of};
use tilo_resource::{ActionCx, Collection, Id, Model, Replica, Resource, resource};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Thread {
    #[model(label)]
    pub title: String,
    pub replies: u32,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Review {
    pub title: String,
    #[model(unit)]
    pub threads: Collection<Thread>,
    #[model(unit)]
    pub summary: String,
}

#[resource(kind = "test.review", label = "Review")]
impl Review {
    /// Start a thread: writes a thread's unit, so it says so.
    #[action(writes = threads)]
    pub fn start_thread(&mut self, cx: &mut ActionCx, title: String) {
        self.threads.push(cx.id(), Thread { title, replies: 0 });
    }

    /// Start a thread without saying so.
    #[action]
    pub fn sneak_thread(&mut self, cx: &mut ActionCx, title: String) {
        self.threads.push(cx.id(), Thread { title, replies: 0 });
    }

    /// Reply in a thread, and touch the record.
    #[action]
    pub fn reply(&mut self, threads: &Id) {
        if let Some(thread) = self.threads.get_mut(threads) {
            thread.replies = thread.replies.saturating_add(1);
        }
        self.title.push('!');
    }

    /// Summarise from a thread, writing a unit it does not declare.
    #[action]
    pub fn summarize(&mut self, threads: &Id) {
        self.summary = self
            .threads
            .get(threads)
            .map(|thread| thread.title.clone())
            .unwrap_or_default();
    }
}

#[test]
fn a_verb_writes_only_the_units_it_declares() {
    let mut review = Replica::<Review>::new();
    review
        .apply(Review::START_THREAD.with("Typos"))
        .expect("a declared unit");
    let err = review
        .apply(Review::SNEAK_THREAD.with("Hidden"))
        .unwrap_err();
    assert!(err.to_string().contains("writes = field"), "{err}");
    let thread = review
        .state()
        .unwrap()
        .threads
        .ids()
        .next()
        .cloned()
        .unwrap();
    review
        .apply(Review::REPLY.with(thread.clone()))
        .expect("an item verb writes its item and the record");
    assert!(review.apply(Review::SUMMARIZE.with(thread)).is_err());
    let state = review.state().unwrap();
    assert_eq!(state.threads.len(), 1);
    assert_eq!(state.summary, "");

    let surface = Review::surface();
    let writes = |id: &str| {
        surface
            .verbs
            .iter()
            .find(|verb| verb.id == id)
            .unwrap()
            .writes
            .clone()
    };
    assert_eq!(writes("start-thread"), [RECORD_UNIT, "threads.*"]);
    assert_eq!(writes("reply"), [RECORD_UNIT, "threads.*"]);
    assert_eq!(writes("summarize"), [RECORD_UNIT, "threads.*"]);
    assert_eq!(writes("sneak-thread"), [RECORD_UNIT]);
}

#[test]
fn a_change_names_the_units_it_touched_from_the_documents_index() {
    let mut review = Replica::<Review>::new();
    review.apply(Review::START_THREAD.with("Typos")).unwrap();
    let thread = review
        .state()
        .unwrap()
        .threads
        .ids()
        .next()
        .cloned()
        .unwrap();
    assert!(
        automerge::ReadDoc::get(review.doc(), &automerge::ROOT, UNIT_INDEX)
            .unwrap()
            .is_some(),
        "the record keeps its unit index"
    );

    let before = review.heads();
    review.apply(Review::REPLY.with(thread.clone())).unwrap();
    let after = review.heads();
    let patches = review.doc_mut().diff(&before, &after);
    let units = units_of(review.doc(), &patches);
    assert_eq!(
        units.into_iter().collect::<Vec<_>>(),
        [RECORD_UNIT.to_owned(), format!("threads/{thread}")]
    );

    let before = review.heads();
    review
        .apply(Review::paths().summary.set("Fine".into()))
        .unwrap();
    let after = review.heads();
    let patches = review.doc_mut().diff(&before, &after);
    assert_eq!(
        units_of(review.doc(), &patches)
            .into_iter()
            .collect::<Vec<_>>(),
        ["summary"]
    );
}

#[test]
fn a_receiver_reads_kinds_edits_and_footprints_from_the_surface() {
    use tilo_actions::PathStep;
    let surface = Review::surface();
    assert!(surface.verb("reply").is_some());
    assert!(surface.verb("nothing").is_none());
    assert!(surface.editable(&[PathStep::Field("title".into())]));
    assert!(surface.editable(&[
        PathStep::Field("threads".into()),
        PathStep::Item("t1".into()),
        PathStep::Field("replies".into()),
    ]));
    assert!(!surface.editable(&[PathStep::Field("nothing".into())]));
    assert_eq!(
        surface.edit_unit(&[
            PathStep::Field("threads".into()),
            PathStep::Item("t1".into()),
            PathStep::Field("replies".into()),
        ]),
        [
            PathStep::Field("threads".into()),
            PathStep::Item("t1".into())
        ]
    );
    assert_eq!(
        surface.edit_unit(&[PathStep::Field("summary".into())]),
        [PathStep::Field("summary".into())]
    );
    assert!(
        surface
            .edit_unit(&[PathStep::Field("title".into())])
            .is_empty()
    );
    assert_eq!(
        surface.footprint("start-thread", None),
        [
            vec![],
            vec![
                PathStep::Field("threads".into()),
                PathStep::Item("*".into())
            ]
        ]
    );
    assert_eq!(
        surface.footprint("reply", Some("t1")),
        [
            vec![],
            vec![
                PathStep::Field("threads".into()),
                PathStep::Item("t1".into())
            ]
        ]
    );
}

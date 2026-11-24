//! Outside work a chain's parts owe, derived from each part as it is: the
//! device carries out what a part's items ask that the outside does not
//! hold yet, confirms it through the part, and reads again only the part
//! that changed.

use std::cell::{Cell, RefCell};
use tilo_app_test_host::TestApp;

use tilo_app::framework::{Chained, Effected, PartLimits};
use tilo_app::prelude::*;
use tilo_app::resource::DocumentId;
use tilo_app::tilo_resource::Collection;

/// Items a part holds before the next one starts.
const PART_ITEMS: usize = 4;

thread_local! {
    /// What the outside was told, in order.
    static TOLD: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// How many times a part's owed work was read.
    static READS: Cell<usize> = const { Cell::new(0) };
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Shelf {
    pub title: String,
}

#[resource(kind = "test.owed-shelf", catalogue = "tests/locales")]
impl Shelf {
    /// Rename the shelf.
    #[action]
    pub fn rename(&mut self, title: String) {
        self.title = title;
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Item {
    /// What the item says.
    pub text: String,
    /// What the outside holds of it.
    #[model(readonly)]
    pub told: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ShelfPart {
    #[model(readonly)]
    pub items: Collection<Item>,
}

#[resource(kind = "test.owed-shelf-part", catalogue = "tests/locales")]
impl ShelfPart {
    /// Put an item on the shelf.
    #[action]
    pub fn put(&mut self, id: String, text: String) {
        self.items.push(
            Id::from_raw(id),
            Item {
                text,
                told: String::new(),
            },
        );
    }

    /// Change what an item says.
    #[action]
    pub fn say(&mut self, id: &Id, text: String) {
        if let Some(item) = self.items.get_mut(id) {
            item.text = text;
        }
    }

    /// The outside holds `text` of the item.
    #[action(external)]
    pub fn told(&mut self, id: &Id, text: String) {
        if let Some(item) = self.items.get_mut(id) {
            item.told = text;
        }
    }
}

impl Chained for ShelfPart {
    type Base = Shelf;

    fn chain() -> DocumentId {
        DocumentId::derived("shelf", &DocumentId::workspace_default())
    }

    const HELD_WHOLE: bool = true;
}

/// What an item's part owes the outside: its text, where the outside holds
/// another.
fn owed(part: &ShelfPart) -> Vec<(Id, String)> {
    READS.with(|reads| reads.set(reads.get().saturating_add(1)));
    part.items
        .iter()
        .filter(|(_, item)| item.text != item.told)
        .map(|(id, item)| (id.clone(), item.text.clone()))
        .collect()
}

struct ShelfApp;

impl ResourceApp for ShelfApp {
    type Resource = Shelf;
    const ID: &'static str = "so.tilo.test-owed-shelf";
    const TITLE: &'static str = "Shelf";

    fn resources(resources: &mut tilo_app::framework::Resources<'_>) {
        resources.chain::<ShelfPart>();
    }

    fn view(cx: &mut ViewCx<Shelf>) -> Element {
        if let Some(chain) = cx.chain::<ShelfPart>() {
            let confirming = chain.dispatcher();
            cx.owed(
                &chain,
                owed,
                |_| true,
                move |id, text: String, _| {
                    TOLD.with(|told| told.borrow_mut().push(format!("{id}={text}")));
                    // Written after the frame, when the store takes writes.
                    confirming.submit(ShelfPart::TOLD.with(shelf_part::Told {
                        id: id.clone(),
                        text,
                    }));
                    Ok(Effected::done())
                },
            );
        }
        Box::new(Column::new())
    }
}

fn told() -> Vec<String> {
    TOLD.with(|told| told.borrow().clone())
}

fn put(app: &mut TestApp<ShelfApp>, id: &str, text: &str) {
    app.chain_invoke(&ShelfPart::PUT.with(shelf_part::Put {
        id: id.to_owned(),
        text: text.to_owned(),
    }))
    .unwrap();
    app.settle();
}

tilo_test_env::scenario! {
    fn work_a_chain_owes_is_done_once_and_reads_only_the_part_that_changed;

    phase a_chains_resource_stands_at_the_top_of_the_apps_tree;
    // A chain holds the app's items, so its resource is published beside
    // the app's own resource and not under it: a path an agent reads for an
    // item names the chain and the item, nothing in between.
    {
        let mut app = TestApp::<ShelfApp>::new().unwrap();
        app.settle();
        let chain = tilo_app::framework::chain_resource_id(&ShelfPart::chain());
        let forest = app.harness().resources();
        let top: Vec<&str> = forest.iter().map(|resource| resource.id.as_str()).collect();
        assert!(top.contains(&chain.as_str()), "the chain is published: {top:?}");
        assert!(
            forest.iter().all(|resource| resource
                .children
                .iter()
                .all(|child| child.id.as_str() != chain)),
            "the chain is not one of the app resource's children: {top:?}"
        );
    }

    phase each_item_is_told_once_and_confirmed_in_its_part;
    {
        let _parts = PartLimits::scoped(PART_ITEMS, usize::MAX);
        let mut app = TestApp::<ShelfApp>::new().unwrap();
        for n in 0..=PART_ITEMS {
            put(&mut app, &format!("i{n}"), "new");
        }
        app.settle();
        let mut seen = told();
        seen.sort();
        assert_eq!(seen, ["i0=new", "i1=new", "i2=new", "i3=new", "i4=new"]);
        let parts = app.chain_parts::<ShelfPart>();
        assert_eq!(parts.len(), 2);
        assert!(
            parts
                .iter()
                .flat_map(|(_, part)| part.items.iter())
                .all(|(_, item)| item.told == item.text),
            "every item confirmed in its part"
        );

        // A change to the newest part reads only that part again.
        app.settle();
        let before = READS.with(Cell::get);
        app.chain_invoke(&ShelfPart::SAY.with(shelf_part::Say {
            id: Id::from_raw("i4"),
            text: "changed".into(),
        }))
        .unwrap();
        app.settle();
        let reads = READS.with(Cell::get).saturating_sub(before);
        assert!(reads >= 1, "the changed part was read");
        assert!(
            told().contains(&"i4=changed".to_owned()),
            "the new ask is carried out: {:?}",
            told()
        );
        assert_eq!(told().len(), 6, "nothing else ran again");
        // Part 0 never changed after its confirmations: never read again.
        assert!(reads <= 2, "only the changed part is read again, read {reads}");
    }
}

/// A window over a chain held whole, kept on this device in many parts,
/// builds with the head alone: decoding every earlier part as it is built
/// held its first frame (and any click) for all of them. Each earlier part
/// opens in a turn of its own after that, newest first, and the window ends
/// holding everything it held before.
#[test]
fn a_window_over_a_chain_held_whole_opens_its_earlier_parts_a_turn_at_a_time() {
    const PARTS: usize = 6;
    let _parts = PartLimits::scoped(PART_ITEMS, usize::MAX);
    let mut app = TestApp::<ShelfApp>::new().unwrap();
    for n in 0..PARTS * PART_ITEMS {
        put(&mut app, &format!("i{n}"), "new");
    }
    app.settle();
    let before = app.chain_parts::<ShelfPart>();
    assert_eq!(before.len(), PARTS);
    let chain = ShelfPart::chain_beside(&app.checkout().id);
    let open = |app: &TestApp<ShelfApp>| -> Vec<u64> {
        let mut open: Vec<u64> = app
            .open_resources()
            .iter()
            .filter_map(DocumentId::chain_part)
            .filter(|(of, _)| *of == chain)
            .map(|(_, index)| index)
            .collect();
        open.sort_unstable();
        open
    };

    // A turn comes when this test lets the window's background work go on.
    let turns = tilo_app::background::Pumped::new();
    let _turns = turns.background().enter();
    let started = std::time::Instant::now();
    let mut app = app.reopen().unwrap();
    let built = started.elapsed();
    let head = u64::try_from(PARTS - 1).unwrap();
    assert_eq!(open(&app), [head], "the window builds with the head alone");
    app.frame();
    app.idle();
    assert_eq!(open(&app), [head], "no earlier part opens before its turn");

    let mut opened = 1;
    let mut taken = 0;
    while opened < PARTS {
        assert!(taken < 3 * PARTS, "every part opens: {:?}", open(&app));
        taken += 1;
        turns.advance(std::time::Duration::ZERO);
        app.frame();
        let now = open(&app);
        assert!(
            now.len() <= opened + 1,
            "one earlier part opens a turn: {opened} became {now:?}"
        );
        let oldest = head + 1 - u64::try_from(now.len()).unwrap();
        assert_eq!(now.first(), Some(&oldest), "newest first: {now:?}");
        opened = now.len();
    }
    eprintln!(
        "built in {built:?}; every part held in {:?} after {taken} turns",
        started.elapsed()
    );
    app.settle();
    let after = app.chain_parts::<ShelfPart>();
    assert_eq!(after, before, "the window holds what it held before");
}

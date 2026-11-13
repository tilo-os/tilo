//! Concurrent whole-value writes of a text field, through the published
//! field edit: what an agent (`set`, `replace`) and a person typing
//! (`splice`) do to the same field at once.
//!
//! The merge keeps each writer's inserted text in one piece: a `set` is
//! stored as the one splice that turns the current text into the new one,
//! and Automerge never mixes the characters of two concurrent insertions.

use crate::text::splice_between;
use crate::{ActionCx, Collection, Edit, Id, Invoke, Model, Replica, resource};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Pad {
    pub body: String,
    pub items: Collection<Item>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Item {
    pub title: String,
}

#[resource(kind = "test.pad", catalogue = "tests/locales")]
impl Pad {
    /// Add an item.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, title: String) {
        self.items.push(cx.id(), Item { title });
    }
}

fn apply(replica: &mut Replica<Pad>, edit: &Edit<Pad>) {
    replica.apply(Invoke::edit(edit).unwrap()).unwrap();
}

fn body(replica: &Replica<Pad>) -> String {
    replica.state().unwrap().body
}

/// Two peers forked from a pad whose body is `base`, each setting the body
/// whole, merged.
fn merged_sets(base: &str, left: &str, right: &str) -> String {
    let mut origin = Replica::<Pad>::new();
    apply(&mut origin, &Pad::paths().body.set(base.to_owned()));
    let mut ada = origin.fork();
    let mut bo = origin.fork();
    apply(&mut ada, &Pad::paths().body.set(left.to_owned()));
    apply(&mut bo, &Pad::paths().body.set(right.to_owned()));
    ada.sync_with(&mut bo).unwrap();
    assert_eq!(body(&ada), body(&bo), "both peers converge");
    body(&ada)
}

#[test]
fn concurrent_sets_of_different_parts_keep_both() {
    assert_eq!(
        merged_sets("hello world", "hello brave world", "hello world!"),
        "hello brave world!"
    );
}

#[test]
fn concurrent_sets_of_one_part_keep_each_inserted_text_whole() {
    // Both replaced "hello": the text both deleted stays deleted, and each
    // replacement comes through in one piece, one after the other.
    let merged = merged_sets("hello world", "goodbye world", "farewell world");
    assert!(
        merged == "goodbyefarewell world" || merged == "farewellgoodbye world",
        "{merged:?}"
    );
    // Both appended to the same end.
    let merged = merged_sets(
        "Shared from Ada",
        "Shared from Ada and then more",
        "Shared from Ada, edited by Bo",
    );
    assert!(
        merged == "Shared from Ada and then more, edited by Bo"
            || merged == "Shared from Ada, edited by Bo and then more",
        "{merged:?}"
    );
}

/// splitmix64: a seeded generator, so a failing case names its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        let bound = u64::try_from(bound.max(1)).unwrap();
        usize::try_from(self.next().checked_rem(bound).unwrap()).unwrap()
    }

    fn word(&mut self, alphabet: &[char]) -> String {
        (0..=self.below(6))
            .map(|_| alphabet[self.below(alphabet.len())])
            .collect()
    }
}

#[derive(Clone, Copy)]
enum Field<'a> {
    Body,
    Title(&'a Id),
}

impl Field<'_> {
    fn read(self, replica: &Replica<Pad>) -> String {
        let state = replica.state().unwrap();
        match self {
            Self::Body => state.body,
            Self::Title(id) => state.items.get(id).unwrap().title.clone(),
        }
    }

    fn path(self) -> crate::Path<Pad, String> {
        match self {
            Self::Body => Pad::paths().body,
            Self::Title(id) => Pad::paths().items.item(id).title,
        }
    }
}

/// One random write by a peer typing from `alphabet`: a whole-value `set`
/// or `replace` of the text with one or two spans rewritten, or a `splice`.
/// Returns the text it inserted, which must come through in one piece.
fn write(rng: &mut Rng, replica: &mut Replica<Pad>, field: Field<'_>, alphabet: &[char]) -> String {
    let current: Vec<char> = field.read(replica).chars().collect();
    let rewrite = |text: &[char], rng: &mut Rng| {
        let start = rng.below(text.len().saturating_add(1));
        let tail = text.len().saturating_sub(start);
        let end = start.saturating_add(rng.below(tail.saturating_add(1)));
        let word = rng.word(alphabet);
        let mut out: Vec<char> = text[..start].to_vec();
        out.extend(word.chars());
        out.extend_from_slice(&text[end..]);
        (start, end.saturating_sub(start), word, out)
    };
    let (at, delete, word, mut next) = rewrite(&current, rng);
    let path = field.path();
    match rng.below(3) {
        0 => {
            apply(replica, &path.splice(at, delete, word.clone()));
            word
        }
        kind => {
            if rng.below(2) == 0 {
                next = rewrite(&next, rng).3;
            }
            let before: String = current.iter().collect();
            let after: String = next.iter().collect();
            let inserted = match splice_between(&before, &after) {
                Some(crate::TextEdit::Splice { insert, .. }) => insert,
                _ => String::new(),
            };
            if kind == 1 {
                apply(replica, &path.set(after));
            } else {
                apply(replica, &path.replace(after));
            }
            inserted
        }
    }
}

/// Random concurrent writes to one field never mix two writers' characters:
/// after every merge, each peer's inserted text is in the field in one
/// piece.
#[test]
fn concurrent_sets_and_splices_never_interleave() {
    let ada_types: Vec<char> = ('A'..='M').chain([' ']).collect();
    let bo_types: Vec<char> = ('n'..='z').chain([' ']).collect();
    for seed in 0..400_u64 {
        let mut rng = Rng(seed);
        let mut origin = Replica::<Pad>::new();
        apply(
            &mut origin,
            &Pad::paths().body.set("0123 4567 89".to_owned()),
        );
        origin.apply(Pad::ADD.with("item 0123")).unwrap();
        let id = origin.state().unwrap().items.ids().next().unwrap().clone();
        let field = if seed % 2 == 0 {
            Field::Body
        } else {
            Field::Title(&id)
        };
        let mut ada = origin.fork();
        let mut bo = origin.fork();
        for round in 0..4 {
            let from_ada = write(&mut rng, &mut ada, field, &ada_types);
            let from_bo = write(&mut rng, &mut bo, field, &bo_types);
            ada.sync_with(&mut bo).unwrap();
            let merged = field.read(&ada);
            assert_eq!(
                merged,
                field.read(&bo),
                "seed {seed} round {round}: converge"
            );
            for inserted in [&from_ada, &from_bo] {
                assert!(
                    merged.contains(inserted.as_str()),
                    "seed {seed} round {round}: {inserted:?} came apart in {merged:?} \
                     (Ada inserted {from_ada:?}, Bo {from_bo:?})"
                );
            }
        }
    }
}

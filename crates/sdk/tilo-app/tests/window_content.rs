//! Every window has its own content store, handed to its code wherever it
//! runs: two windows of one app in two workspaces, in one process, keep what they
//! store apart — what an action's arguments are kept from where it is invoked,
//! and what the window's views read.

use std::cell::RefCell;

use tilo_app::prelude::*;
use tilo_app::tilo_resource::Bytes;
use tilo_app::tilo_resource::content;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Album {
    pub pictures: Vec<Bytes>,
}

#[resource(kind = "test.album", label = "Album", app = "so.tilo.test-album")]
impl Album {
    /// Keep a picture of the album.
    #[action]
    pub fn attach(&mut self, picture: Bytes) {
        self.pictures.push(picture);
    }
}

thread_local! {
    /// The stores the windows' views found.
    static SEEN: RefCell<Vec<tilo_content::ContentStore>> = const { RefCell::new(Vec::new()) };
}

struct AlbumApp;

impl ResourceApp for AlbumApp {
    type Resource = Album;
    const ID: &'static str = "so.tilo.test-album";
    const TITLE: &'static str = "Album";

    fn view(_: &mut ViewCx<Album>) -> Element {
        if let Some(store) = content::current() {
            SEEN.with(|seen| seen.borrow_mut().push(store));
        }
        Box::new(Text::new("album"))
    }
}

/// A picture too large to live inside the record, so the window it is
/// invoked from keeps it.
fn picture(name: &str) -> Vec<u8> {
    name.as_bytes().repeat(200)
}

#[test]
fn two_windows_in_two_workspaces_keep_their_content_apart() {
    let mut first = TestApp::<AlbumApp>::in_workspace("workspace-first").unwrap();
    let mut second = TestApp::<AlbumApp>::in_workspace("workspace-second").unwrap();
    assert_eq!(
        SEEN.with(|seen| seen.borrow().len()),
        2,
        "each view finds its window's store"
    );

    // Interleaved, as the shell drives its tiles: each invocation keeps the
    // picture it carries in its own window's store, and the record names it
    // there.
    let one = picture("a picture in the first workspace");
    let two = picture("a picture in the second workspace");
    let three = picture("another in the first");
    first
        .invoke(Album::ATTACH.with(Bytes::to_keep(one.clone())))
        .unwrap();
    second
        .invoke(Album::ATTACH.with(Bytes::to_keep(two.clone())))
        .unwrap();
    first
        .invoke(Album::ATTACH.with(Bytes::to_keep(three.clone())))
        .unwrap();

    let firsts = first.state().pictures;
    let seconds = second.state().pictures;
    assert_eq!((firsts.len(), seconds.len()), (2, 1));
    for (stored, kept) in firsts.iter().zip([&one, &three]) {
        assert_eq!(stored.read(&first.content()).as_ref(), Some(kept));
        let reference = stored.reference().expect("kept in a window's store");
        assert!(tilo_content::is_sealed(&reference), "{reference}");
        assert!(
            !second.content().has(&reference),
            "the second window never sees it"
        );
    }
    assert_eq!(seconds[0].read(&second.content()).as_ref(), Some(&two));
    let reference = seconds[0].reference().expect("kept in a window's store");
    assert!(tilo_content::is_sealed(&reference), "{reference}");
    assert!(!first.content().has(&reference));

    // Outside both windows there is no store to reach.
    assert!(content::current().is_none());
}

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! Every window has its own content store, handed to its code wherever it
//! runs: two windows of one app in two rooms, in one process, keep what they
//! store apart — what an action applying stores, what an action's arguments
//! are built from, and what work leaving the window's thread reads.

use std::cell::RefCell;

use tilo_app::prelude::*;
use tilo_app::tilo_resource::content;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Album {
    pub pictures: Vec<String>,
}

#[resource(kind = "test.album", label = "Album")]
impl Album {
    /// Keep `text` as a picture of the album.
    #[action]
    pub fn attach(&mut self, cx: &mut ActionCx, text: String) -> Result<(), Refusal> {
        let store = cx.content().ok_or("the window lent no content store")?;
        let hash = store
            .put(text.as_bytes())
            .map_err(|error| Refusal::new(error.to_string()))?;
        self.pictures.push(hash);
        Ok(())
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

#[test]
fn two_windows_in_two_rooms_keep_their_content_apart() {
    let mut first = TestApp::<AlbumApp>::in_room("room-first").unwrap();
    let mut second = TestApp::<AlbumApp>::in_room("room-second").unwrap();
    assert_eq!(
        SEEN.with(|seen| seen.borrow().len()),
        2,
        "each view finds its window's store"
    );

    // Interleaved, as the shell drives its tiles: each action applies with
    // its own window's store.
    first
        .invoke(Album::ATTACH.with("a picture in the first room"))
        .unwrap();
    second
        .invoke(Album::ATTACH.with("a picture in the second room"))
        .unwrap();
    first
        .invoke(Album::ATTACH.with("another in the first"))
        .unwrap();

    let firsts = first.state().pictures;
    let seconds = second.state().pictures;
    assert_eq!((firsts.len(), seconds.len()), (2, 1));
    for hash in &firsts {
        assert!(first.content().has(hash), "the first window keeps its own");
        assert!(!second.content().has(hash), "and the second never sees it");
    }
    assert!(second.content().has(&seconds[0]));
    assert!(!first.content().has(&seconds[0]));

    // Outside both windows there is no store to reach.
    assert!(content::current().is_none());
}

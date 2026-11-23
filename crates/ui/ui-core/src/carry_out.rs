//! Files of the window's workspace carried out onto the computer it runs on:
//! dragged onto its desktop or file manager, or saved where the person
//! chooses.
//!
//! A window names what it carries by the files' content in the workspace
//! ([`ContentLinkAsk`]); the host it runs in gets the bytes there. In a page
//! that is the workspace's host itself: it serves the files at a short-lived
//! link the browser fetches on its own, so a file of any size goes from the
//! host's store to the computer's disk without passing through the page.
//!
//! - **Dragging out.** A control that can be dragged out says so in its
//!   semantics ([`crate::a11y::AccessNode::carries_out`]) by the key
//!   [`Offers::offer`] gave it. A browser starts a drag onto the computer
//!   only from a real element under the pointer, so the page makes that
//!   control's element in its semantic mirror draggable and asks the host
//!   what it carries ([`offered`]) as the pointer comes near it. A drag that
//!   stays in the page is the window's own, as before; one that leaves it is
//!   the file's.
//! - **Saving.** [`save`] hands the files to the host, which asks where to
//!   save them (or lets the browser download them) and streams them there.
//! - **Showing.** [`show`] hands one file over to be shown in the
//!   browser's own tab, streamed from the link: a film or a recording plays
//!   there without the page holding it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};
use std::rc::Rc;

pub use tilo_actions::remote::{ContentLinkAsk, ContentLinkFile};

/// The host a window's carried files go through: a page's desktop.
pub trait Carrier {
    /// Save `ask`'s files where the person chooses, as one file or one zip.
    /// Called while the window handles the person's click, inside the
    /// gesture a browser asks a Save dialog for.
    fn save(&self, ask: ContentLinkAsk);
    /// Show `ask`'s one file in a tab of the browser's own, which plays or
    /// shows it as it streams; a file it would not show safely is saved.
    /// Called inside the person's click, as [`Carrier::save`] is.
    fn show(&self, ask: ContentLinkAsk) {
        self.save(ask);
    }
}

thread_local! {
    static CARRIER: RefCell<Option<Rc<dyn Carrier>>> = const { RefCell::new(None) };
    /// What each offered key carries, and how many offers hold it.
    static OFFERED: RefCell<HashMap<String, (ContentLinkAsk, usize)>> = RefCell::new(HashMap::new());
}

/// Carry the windows' files on this thread through `host` until the guard
/// drops.
#[must_use = "the host carries files only while the guard lives"]
pub fn serve(carrier: Rc<dyn Carrier>) -> CarryGuard {
    CarryGuard {
        previous: CARRIER.with(|served| served.replace(Some(carrier))),
    }
}

/// Puts back the host served before, when dropped.
pub struct CarryGuard {
    previous: Option<Rc<dyn Carrier>>,
}

impl Drop for CarryGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CARRIER.with(|served| *served.borrow_mut() = previous);
    }
}

/// Whether a host carries files out here: in a page, yes.
#[must_use]
pub fn available() -> bool {
    CARRIER.with(|served| served.borrow().is_some())
}

/// Save `ask`'s files where the person chooses. `false` when no host
/// carries files out here.
pub fn save(ask: ContentLinkAsk) -> bool {
    let Some(carrier) = CARRIER.with(|served| served.borrow().clone()) else {
        return false;
    };
    carrier.save(ask);
    true
}

/// Show `ask`'s one file in the browser's own tab, streamed from the link.
/// `false` when nothing carries files out here.
pub fn show(ask: ContentLinkAsk) -> bool {
    let Some(carrier) = CARRIER.with(|served| served.borrow().clone()) else {
        return false;
    };
    carrier.show(ask);
    true
}

/// What the control whose semantics carry `key` would drag out.
#[must_use]
pub fn offered(key: &str) -> Option<ContentLinkAsk> {
    OFFERED.with(|offered| offered.borrow().get(key).map(|(ask, _)| ask.clone()))
}

/// The key `ask` is offered under: the same files under the same name are
/// one key, whoever offers them.
fn key_of(ask: &ContentLinkAsk) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ask.name.hash(&mut hasher);
    ask.zip.hash(&mut hasher);
    for file in &ask.files {
        file.path.hash(&mut hasher);
        file.hash.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// The files a widget's controls can be dragged out with, held while it
/// shows them.
#[derive(Default)]
pub struct Offers {
    keys: RefCell<Vec<String>>,
}

impl Offers {
    /// Offer `ask`: the key a control's semantics name it by.
    pub fn offer(&self, ask: ContentLinkAsk) -> String {
        let key = key_of(&ask);
        OFFERED.with(|offered| {
            offered
                .borrow_mut()
                .entry(key.clone())
                .and_modify(|(_, held)| *held = held.saturating_add(1))
                .or_insert((ask, 1));
        });
        self.keys.borrow_mut().push(key.clone());
        key
    }

    /// Let go of everything offered so far.
    pub fn clear(&self) {
        let keys = std::mem::take(&mut *self.keys.borrow_mut());
        OFFERED.with(|offered| {
            let mut offered = offered.borrow_mut();
            for key in keys {
                if let Some((_, held)) = offered.get_mut(&key) {
                    *held = held.saturating_sub(1);
                    if *held == 0 {
                        offered.remove(&key);
                    }
                }
            }
        });
    }
}

impl Drop for Offers {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(name: &str) -> ContentLinkAsk {
        ContentLinkAsk {
            name: name.into(),
            files: vec![ContentLinkFile {
                path: name.into(),
                hash: "ab".repeat(32),
            }],
            zip: false,
        }
    }

    struct Kept(RefCell<Vec<String>>);

    impl Carrier for Kept {
        fn save(&self, ask: ContentLinkAsk) {
            self.0.borrow_mut().push(ask.name);
        }
    }

    /// An offer is found by its key while some widget holds it, and saving
    /// reaches the host served on the thread, only while it is served.
    #[test]
    fn offers_live_while_held_and_saves_reach_the_served_host() {
        let first = Offers::default();
        let second = Offers::default();
        let key = first.offer(ask("plan.md"));
        assert_eq!(
            second.offer(ask("plan.md")),
            key,
            "the same files, the same key"
        );
        let other = first.offer(ask("budget.csv"));
        assert_ne!(other, key);
        assert_eq!(offered(&key).map(|ask| ask.name), Some("plan.md".into()));
        first.clear();
        assert!(offered(&key).is_some(), "the second still offers it");
        assert!(offered(&other).is_none());
        drop(second);
        assert!(offered(&key).is_none());

        assert!(!available());
        assert!(!save(ask("plan.md")), "no host, nothing saved");
        let kept = Rc::new(Kept(RefCell::default()));
        {
            let _served = serve(kept.clone());
            assert!(available());
            assert!(save(ask("plan.md")));
        }
        assert!(!save(ask("after.md")));
        assert_eq!(*kept.0.borrow(), vec!["plan.md".to_owned()]);
    }
}

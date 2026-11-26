//! Workspace emoji as a test sets them, for the tests of whatever draws text with them.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use super::WorkspaceEmoji;
use crate::workspace_emoji::{CustomEmoji, EmojiSource};

/// A window's workspace emoji, as a test sets them.
pub struct Fixed {
    pub emoji: RefCell<Rc<WorkspaceEmoji>>,
    pub revision: Cell<u64>,
}

impl Fixed {
    pub fn new(emoji: WorkspaceEmoji) -> Self {
        Self {
            emoji: RefCell::new(Rc::new(emoji)),
            revision: Cell::new(1),
        }
    }

    /// The workspace's emoji are now `emoji`.
    pub fn set(&self, emoji: WorkspaceEmoji) {
        *self.emoji.borrow_mut() = Rc::new(emoji);
        self.revision.set(self.revision.get() + 1);
    }
}

impl EmojiSource for Fixed {
    fn emoji(&self) -> Rc<WorkspaceEmoji> {
        Rc::clone(&self.emoji.borrow())
    }
    fn revision(&self) -> u64 {
        self.revision.get()
    }
    fn can_manage(&self) -> bool {
        false
    }
    fn add(&self, _: &str, _: &[u8]) -> Result<(), String> {
        Err("fixed".into())
    }
    fn remove(&self, _: &str) -> Result<(), String> {
        Err("fixed".into())
    }
    fn choose_and_add(&self) {}
}

#[expect(
    clippy::unwrap_used,
    reason = "an 8 px square of whole pixels always encodes; a test fixture that cannot is the failure"
)]
fn picture(rgb: [u8; 3]) -> Arc<[u8]> {
    let side = 8;
    let rgba: Vec<u8> = (0..side * side)
        .flat_map(|_| [rgb[0], rgb[1], rgb[2], 255])
        .collect();
    Arc::from(crate::png::encode_rgba8(side, side, &rgba).unwrap())
}

/// A workspace with a few custom emoji of its own, each a plain coloured square.
pub fn workspace() -> WorkspaceEmoji {
    WorkspaceEmoji::new(vec![
        CustomEmoji {
            name: "tilo".to_owned(),
            image: picture([220, 40, 90]),
        },
        CustomEmoji {
            name: "party_parrot".to_owned(),
            image: picture([40, 200, 90]),
        },
        CustomEmoji {
            name: "broken".to_owned(),
            image: Arc::from(&b"not a picture"[..]),
        },
    ])
}

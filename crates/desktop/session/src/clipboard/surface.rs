//! The toolkit's clipboard for the session's own surfaces (the shell's search, its fields, a search
//! row's Copy): what they copy is the person's copy, as an app's is, and they paste what an app
//! would. They run on the loop that serves the selection, so a paste never waits on a read: the
//! loop keeps the selection's text as the history reads it, and a paste asked for during that read
//! is answered once it is over, never with an older selection's text.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;
use ui::clipboard::{ClipboardBackend, ClipboardTarget, InProcessClipboard};

#[derive(Clone)]
pub(crate) struct SurfaceClipboard {
    mirror: Arc<Mutex<Mirror>>,
    /// Wakes the loop to put a copy on the clipboard.
    wake: tilo_service::Wake,
    /// The shell pastes no primary selection into apps: its own stays in the shell.
    primary: Arc<InProcessClipboard>,
}

#[derive(Default)]
struct Mirror {
    /// The selection change `text` is of, and whether its read is still going.
    change: u64,
    reading: bool,
    text: Option<String>,
    waiting: Vec<Sender<Option<String>>>,
    /// What the shell copied since the loop last took it.
    copies: Vec<String>,
}

impl SurfaceClipboard {
    pub(crate) fn new(wake: tilo_service::Wake) -> Self {
        Self {
            mirror: Arc::default(),
            wake,
            primary: Arc::default(),
        }
    }

    /// The selection moved to `change`: a paste waits for its read.
    pub(crate) fn moved(&self, change: u64) {
        let mut mirror = self.mirror.lock_recover();
        if mirror.change != change {
            *mirror = Mirror {
                change,
                reading: true,
                waiting: std::mem::take(&mut mirror.waiting),
                copies: std::mem::take(&mut mirror.copies),
                text: None,
            };
        }
    }

    /// The selection at `change` holds `text`. A read of one that has moved on since is dropped.
    pub(crate) fn read(&self, change: u64, text: Option<String>) {
        let mut mirror = self.mirror.lock_recover();
        if change < mirror.change {
            return;
        }
        for paste in std::mem::take(&mut mirror.waiting) {
            let _ = paste.send(text.clone());
        }
        (mirror.change, mirror.reading, mirror.text) = (change, false, text);
    }

    pub(crate) fn take_copies(&self) -> Vec<String> {
        std::mem::take(&mut self.mirror.lock_recover().copies)
    }
}

impl ClipboardBackend for SurfaceClipboard {
    fn set_text(&self, target: ClipboardTarget, text: &str) -> bool {
        if target == ClipboardTarget::Primary {
            return self.primary.set_text(target, text);
        }
        self.mirror.lock_recover().copies.push(text.to_owned());
        (self.wake)();
        true
    }

    fn text(&self, target: ClipboardTarget) -> Option<String> {
        if target == ClipboardTarget::Primary {
            return self.primary.text(target);
        }
        let mirror = self.mirror.lock_recover();
        mirror.text.clone().filter(|_| !mirror.reading)
    }

    fn request_text(&self, target: ClipboardTarget) -> Receiver<Option<String>> {
        if target == ClipboardTarget::Primary {
            return self.primary.request_text(target);
        }
        let (paste, pasted) = channel();
        let mut mirror = self.mirror.lock_recover();
        if mirror.reading {
            mirror.waiting.push(paste);
        } else {
            let _ = paste.send(mirror.text.clone());
        }
        pasted
    }

    fn name(&self) -> &'static str {
        "shell"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paste_takes_the_selection_once_read_and_a_copy_waits_for_the_loop() {
        let shell = SurfaceClipboard::new(Arc::new(|| {}));
        shell.read(1, Some("first".into()));
        assert_eq!(
            shell.text(ClipboardTarget::Clipboard).as_deref(),
            Some("first")
        );
        shell.moved(2);
        assert_eq!(shell.text(ClipboardTarget::Clipboard), None);
        let pasted = shell.request_text(ClipboardTarget::Clipboard);
        shell.read(1, Some("first".into()));
        assert!(
            pasted.try_recv().is_err(),
            "an older selection's read is not this one's"
        );
        shell.read(2, Some("second".into()));
        assert_eq!(pasted.try_recv().ok().flatten().as_deref(), Some("second"));

        assert!(shell.set_text(ClipboardTarget::Clipboard, "line"));
        assert!(shell.set_text(ClipboardTarget::Primary, "picked"));
        assert_eq!(shell.take_copies(), ["line"]);
        assert_eq!(
            shell.text(ClipboardTarget::Primary).as_deref(),
            Some("picked")
        );
    }
}

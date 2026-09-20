//! Admission for an opt-in native application Quit target.
//! This holds identifiers only; the platform validates live window ownership.

use std::rc::Rc;

#[derive(Debug)]
pub(crate) struct QuitRequest<T> {
    target: Option<T>,
    pending: Option<Rc<()>>,
}

#[derive(Debug)]
pub(crate) struct PendingQuit<T> {
    target: T,
    token: Rc<()>,
}

pub(crate) enum QuitDecision<T> {
    Terminate,
    Cancel,
    Forward(PendingQuit<T>),
}

impl<T: Copy + Eq> QuitRequest<T> {
    pub(crate) fn new() -> Self {
        Self {
            target: None,
            pending: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn target(&self) -> Option<T> {
        self.target
    }

    pub(crate) fn set_target(&mut self, target: Option<T>) {
        self.target = target;
        self.pending = None;
    }

    pub(crate) fn destroyed(&mut self, target: T) {
        if self.target == Some(target) {
            self.set_target(None);
        }
    }

    pub(crate) fn request(&mut self, running: bool) -> QuitDecision<T> {
        let Some(target) = self.target else {
            return QuitDecision::Terminate;
        };
        if !running || self.pending.is_some() {
            return QuitDecision::Cancel;
        }
        let token = Rc::new(());
        self.pending = Some(token.clone());
        QuitDecision::Forward(PendingQuit { target, token })
    }

    /// Clear coalescing before dispatch, so a later Quit after Keep Editing
    /// reaches admission again. Tokens prevent a queued request from targeting
    /// a replacement window even if the native identifier is reused.
    pub(crate) fn take(&mut self, request: PendingQuit<T>) -> Option<T> {
        if self.target == Some(request.target)
            && self
                .pending
                .as_ref()
                .is_some_and(|token| Rc::ptr_eq(token, &request.token))
        {
            self.pending = None;
            Some(request.target)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_is_opt_in_and_coalesces_only_until_delivery() {
        let mut quit = QuitRequest::new();
        assert!(matches!(quit.request(true), QuitDecision::Terminate));
        quit.set_target(Some(1));
        assert!(matches!(quit.request(false), QuitDecision::Cancel));
        let QuitDecision::Forward(first) = quit.request(true) else {
            panic!("queued Quit")
        };
        assert!(matches!(quit.request(true), QuitDecision::Cancel));
        assert_eq!(quit.take(first), Some(1));
        let QuitDecision::Forward(retry) = quit.request(true) else {
            panic!("Quit after Keep Editing")
        };
        assert_eq!(quit.take(retry), Some(1));
    }

    #[test]
    fn destruction_replacement_and_exit_invalidate_queued_quit() {
        let mut quit = QuitRequest::new();
        quit.set_target(Some(1));
        let QuitDecision::Forward(old) = quit.request(true) else {
            panic!("queued Quit")
        };
        quit.destroyed(2);
        assert_eq!(
            quit.target(),
            Some(1),
            "secondary windows do not change the target"
        );
        quit.destroyed(1);
        quit.set_target(Some(1));
        assert_eq!(
            quit.take(old),
            None,
            "reused native ID is a different registration"
        );
        let QuitDecision::Forward(pending) = quit.request(true) else {
            panic!("new Quit")
        };
        quit.set_target(None);
        assert_eq!(
            quit.take(pending),
            None,
            "event-loop exit discards pending Quit"
        );
        assert!(matches!(quit.request(true), QuitDecision::Terminate));
    }
}

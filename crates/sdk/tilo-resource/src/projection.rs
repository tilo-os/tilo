//! What a view remembers about the newest read of its document.
//!
//! A view keeps a projection of its resource — the state it last read whole —
//! and reads the document again whenever it moves. A read can fail: a
//! collaborator wrote a field the model cannot hold, or the document was
//! written by a newer version of the app ([`evolution`](crate::evolution)). That
//! failure is not an empty document, and treating it as one loses what the
//! view held: a rebuilt model reads the record as holding nothing, so a
//! reconciliation with a server adds duplicates, starts from no sync token,
//! or deletes what the document still holds.
//!
//! [`DocumentRead`] records the newest read beside that projection: the view
//! keeps showing what last read whole and says why the newest read did not,
//! and anything derived from the projection — a write, a synchronization, a
//! reconciliation — waits for a read that succeeds. A document that reads as
//! an empty record is a successful read: only a failed one is reported.

use crate::ModelError;

/// The outcome of the newest read of a document, beside the state that last
/// read whole.
///
/// ```
/// use tilo_resource::{DocumentRead, ModelError};
///
/// let mut read = DocumentRead::default();
/// let shown = String::from("what last read whole");
/// let failed: Result<String, ModelError> = Err(ModelError::shape("a map"));
/// assert!(read.adopt(failed).is_none());
/// assert_eq!(shown, "what last read whole");
/// assert!(read.failed());
/// assert!(read.take_report().is_some());
/// ```
#[derive(Debug, Default)]
pub struct DocumentRead {
    /// Why the newest read failed, while the projection keeps the state that
    /// last read whole.
    failure: Option<ModelError>,
    /// The failure already told to the person, so a repeat of the same one
    /// is not said again frame after frame.
    reported: Option<String>,
}

impl DocumentRead {
    /// Take `read`'s result: the state it read whole, or `None` after
    /// keeping why it did not, for the caller to leave its projection as it
    /// was.
    ///
    /// A read that succeeds — including one of a record that is empty —
    /// clears the failure and is adopted, so nothing reported it as
    /// corruption.
    pub fn adopt<T>(&mut self, read: Result<T, ModelError>) -> Option<T> {
        match read {
            Ok(state) => {
                self.failure = None;
                self.reported = None;
                Some(state)
            }
            Err(error) => {
                self.failure = Some(error);
                None
            }
        }
    }

    /// Why the newest read did not read, when it did not.
    #[must_use]
    pub fn failure(&self) -> Option<&ModelError> {
        self.failure.as_ref()
    }

    /// Whether the newest read failed: what a write, a synchronization or a
    /// reconciliation derived from the projection must not run on.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.failure.is_some()
    }

    /// The failure to tell the person, once: the same failure is not said
    /// again until a read succeeds (and a later one fails anew).
    pub fn take_report(&mut self) -> Option<String> {
        let reason = self.failure.as_ref()?.to_string();
        if self.reported.as_deref() == Some(reason.as_str()) {
            return None;
        }
        self.reported = Some(reason.clone());
        Some(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::DocumentRead;
    use crate::ModelError;

    #[test]
    fn a_failed_read_keeps_the_last_state_that_read_whole() {
        let mut read = DocumentRead::default();
        assert!(read.adopt(Ok(String::from("first"))).is_some());
        assert!(!read.failed());

        let state: Option<String> = read.adopt(Err(ModelError::shape("a map")));
        assert!(state.is_none(), "a failed read yields no state to show");
        assert!(read.failed());
        assert_eq!(
            read.failure().map(ToString::to_string).as_deref(),
            Some("expected a map")
        );

        let again = read.adopt(Ok(String::from("second")));
        assert_eq!(again.as_deref(), Some("second"), "the next read is shown");
        assert!(!read.failed(), "a read that succeeds clears the failure");
    }

    #[test]
    fn an_empty_record_that_reads_whole_is_not_a_failure() {
        let mut read = DocumentRead::default();
        let empty: Option<Vec<u8>> = read.adopt(Ok(Vec::new()));
        assert_eq!(empty, Some(Vec::new()));
        assert!(!read.failed());
        assert!(read.failure().is_none());
        assert!(
            read.take_report().is_none(),
            "an empty document is valid, not something to report"
        );
    }

    #[test]
    fn the_same_failure_is_reported_once_until_a_read_succeeds() {
        let mut read = DocumentRead::default();
        assert!(read.adopt::<()>(Err(ModelError::shape("a map"))).is_none());
        assert_eq!(read.take_report().as_deref(), Some("expected a map"));
        assert!(
            read.take_report().is_none(),
            "the same failure is not said again every frame"
        );

        assert!(read.adopt::<()>(Err(ModelError::shape("a map"))).is_none());
        assert!(
            read.take_report().is_none(),
            "a repeat of the same failure stays quiet"
        );

        assert!(read.adopt(Ok(())).is_some());
        assert!(read.adopt::<()>(Err(ModelError::shape("a map"))).is_none());
        assert!(
            read.take_report().is_some(),
            "failing again after a read that succeeded is news"
        );
    }
}

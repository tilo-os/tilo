//! Diagnostic-only directory experiment contract. Never a durability authority.
use std::{collections::BTreeMap, io, path::PathBuf};

#[derive(Clone, Debug)]
pub struct AnchorEvidence {
    pub volume_serial: u64,
    pub file_id: [u8; 16],
    pub establishment_record: String,
    pub storage_profile: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Candidate {
    Direct,
    Staged,
    Replay,
    Collision,
    Reparse,
    Denied,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParentAccess {
    Minimal,
    Writable,
}
#[derive(Clone, Debug)]
pub struct Request {
    pub root: PathBuf,
    pub profile: String,
    pub evidence: AnchorEvidence,
    pub candidate: Candidate,
    pub parent_access: ParentAccess,
    /// Replay chain, or controller-provisioned reparse/denied fixture, relative to root.
    pub components: Vec<String>,
}
impl Request {
    /// Check that the request is complete and bounded before any effect.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` unless the request names an absolute root, the
    /// local NTFS profile, external evidence and safe bounded components.
    pub fn validate(&self) -> io::Result<()> {
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "explicit absolute root, local-ntfs profile, external anchor/storage evidence and safe bounded case components required",
            )
        };
        if !self.root.is_absolute()
            || self.profile != "local-ntfs"
            || self.evidence.establishment_record.trim().is_empty()
            || self.evidence.storage_profile.trim().is_empty()
            || self.components.len() > 16
        {
            return Err(invalid());
        }
        let needs_components = matches!(
            self.candidate,
            Candidate::Replay | Candidate::Reparse | Candidate::Denied
        );
        if needs_components == self.components.is_empty() {
            return Err(invalid());
        }
        for name in &self.components {
            if name.is_empty()
                || name == "."
                || name == ".."
                || name.encode_utf16().count() > 255
                || name.chars().any(|c| matches!(c, '\0' | '/' | '\\' | ':'))
            {
                return Err(invalid());
            }
        }
        if matches!(self.candidate, Candidate::Denied | Candidate::Reparse)
            && self.components.len() != 1
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct Event {
    pub sequence: u64,
    pub operation: String,
    pub fields: BTreeMap<String, String>,
}
/// The sink must flush each event to a stream outside the tested filesystem.
pub type Sink<'a> = dyn FnMut(&Event) -> io::Result<()> + 'a;
#[derive(Clone, Debug)]
pub struct Outcome {
    pub fully_observed: bool,
    pub candidate_calls_succeeded: bool,
    pub terminal: String,
    pub qualification: &'static str,
}
impl Outcome {
    pub fn from_result(result: &io::Result<()>, complete: bool) -> Self {
        Self {
            fully_observed: complete,
            candidate_calls_succeeded: result.is_ok() && complete,
            terminal: format!("{result:?}"),
            qualification: "unqualified",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> Request {
        Request {
            root: std::env::current_dir().unwrap(),
            profile: "local-ntfs".into(),
            evidence: AnchorEvidence {
                volume_serial: 7,
                file_id: [8; 16],
                establishment_record: "external-controller-record-17".into(),
                storage_profile: "disposable-vm, cache=writeback, loss=hard-power-off".into(),
            },
            candidate: Candidate::Direct,
            parent_access: ParentAccess::Writable,
            components: vec![],
        }
    }
    #[test]
    fn missing_or_malformed_prerequisites_refuse_before_native_dispatch() {
        let mut r = request();
        r.root = PathBuf::from("relative");
        assert!(r.validate().is_err());
        let mut r = request();
        r.profile = "auto".into();
        assert!(r.validate().is_err());
        let mut r = request();
        r.evidence.establishment_record.clear();
        assert!(r.validate().is_err());
        let mut r = request();
        r.evidence.storage_profile.clear();
        assert!(r.validate().is_err());
    }
    #[test]
    fn replay_requires_bounded_safe_components() {
        let mut r = request();
        r.candidate = Candidate::Replay;
        assert!(r.validate().is_err());
        for bad in ["..", ".", "x/y", "x\\y", "x:y", "", "x\0y"] {
            r.components = vec![bad.into()];
            assert!(r.validate().is_err(), "{bad:?}");
        }
        r.components = vec!["probe-17".into(), "final".into()];
        assert!(r.validate().is_ok());
        r.components = vec!["x".into(); 17];
        assert!(r.validate().is_err());
    }
    #[test]
    fn successful_calls_without_complete_observation_do_not_pass() {
        let outcome = Outcome::from_result(&Ok(()), false);
        assert!(!outcome.candidate_calls_succeeded);
        assert!(!outcome.fully_observed);
        assert_eq!(outcome.qualification, "unqualified");
    }
}

/// Execute a bounded experiment. Success is operational observation, never qualification.
pub fn run(request: &Request, sink: &mut Sink<'_>) -> Outcome {
    if let Err(error) = request.validate() {
        return Outcome::from_result(&Err(error), false);
    }
    #[cfg(windows)]
    {
        crate::native::atomic::native::run_directory_probe(request, sink)
    }
    #[cfg(not(windows))]
    {
        let result = sink(&Event {
            sequence: 1,
            operation: "native_subject_unavailable".into(),
            fields: BTreeMap::from([("host".into(), std::env::consts::OS.into())]),
        });
        Outcome::from_result(
            &result.and_then(|()| {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "native Windows execution required; no fixture effects attempted",
                ))
            }),
            false,
        )
    }
}

// These decisions are shared by the native caller and host regression tests.
// The outer io::Result means an observation was incomplete. A Refused value
// can only be constructed after a completed failing native return status.
#[cfg(any(windows, test))]
pub(crate) mod observation {
    use super::*;

    pub(crate) enum Directory<T> {
        Opened(T),
        Refused(io::Error),
    }
    pub(crate) fn required<T>(observation: io::Result<Directory<T>>) -> io::Result<T> {
        match observation? {
            Directory::Opened(value) => Ok(value),
            Directory::Refused(error) => Err(error),
        }
    }
    pub(crate) fn ancestor<T>(
        observation: io::Result<Directory<T>>,
        at_anchor: bool,
        log_refusal: impl FnOnce(&io::Error) -> io::Result<()>,
    ) -> io::Result<Option<T>> {
        match observation? {
            Directory::Opened(value) => Ok(Some(value)),
            Directory::Refused(error) => {
                log_refusal(&error)?;
                if at_anchor { Err(error) } else { Ok(None) }
            }
        }
    }
    pub(crate) fn collision<T>(
        observation: io::Result<Directory<T>>,
        expected_refusal: impl FnOnce(&io::Error) -> bool,
        log_refusal: impl FnOnce(&io::Error) -> io::Result<()>,
    ) -> io::Result<bool> {
        match observation? {
            Directory::Opened(_) => Ok(false),
            Directory::Refused(error) => {
                let expected = expected_refusal(&error);
                log_refusal(&error)?;
                Ok(expected)
            }
        }
    }
    pub(crate) fn reparse(observation: io::Result<bool>) -> io::Result<bool> {
        if observation? {
            Ok(true)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "required reparse fixture is not a reparse object",
            ))
        }
    }
    pub(crate) fn flush(status: i32, completion: i32, information: usize) -> io::Result<bool> {
        if status > 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pending or unknown informational flush status: incomplete observation",
            ));
        }
        // A failing native return is a complete rejection even when Windows
        // leaves IOSB untouched. Success requires the actual completion fields.
        if status == 0 && (completion != 0 || information == usize::MAX) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "successful flush has incomplete completion evidence",
            ));
        }
        Ok(status == 0 && completion == 0)
    }
    pub(crate) fn outcome(result: io::Result<bool>) -> Outcome {
        let (complete, result) = match result {
            Ok(true) => (true, Ok(())),
            Ok(false) => (
                true,
                Err(io::Error::other(
                    "candidate operation refused; inspect raw observations",
                )),
            ),
            Err(error) => (false, Err(error)),
        };
        Outcome::from_result(&result, complete)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn incomplete_ancestor_stops_before_fixture_mutation() {
            for error in [
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "pending/malformed native output",
                ),
                io::Error::from_raw_os_error(5),
            ] {
                let mut mutated = false;
                let mut logged_refusal = false;
                let result = (|| {
                    ancestor::<()>(Err(error), false, |_| {
                        logged_refusal = true;
                        Ok(())
                    })?;
                    mutated = true;
                    Ok(true)
                })();
                assert!(!outcome(result).fully_observed);
                assert!(!mutated);
                assert!(
                    !logged_refusal,
                    "incomplete event-sink/admission error is not a native refusal"
                );
            }
        }
        #[test]
        fn incomplete_collision_cannot_be_a_complete_negative() {
            for error in [
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "pending/malformed native output",
                ),
                io::Error::from_raw_os_error(183),
            ] {
                let mut continued = false;
                let result = (|| {
                    let expected =
                        collision::<()>(Err(error), |e| e.raw_os_error() == Some(183), |_| Ok(()))?;
                    continued = true;
                    Ok(expected)
                })();
                let observed = outcome(result);
                assert!(!observed.fully_observed);
                assert!(!observed.candidate_calls_succeeded);
                assert!(!continued);
            }
        }
        #[test]
        fn unknown_flush_status_and_missing_completion_are_incomplete() {
            for (status, done, info) in [
                (1, 0x103, usize::MAX),
                (0x4000_0000, 0, 0),
                (0, 0x103, usize::MAX),
            ] {
                let observed = outcome(flush(status, done, info));
                assert!(!observed.fully_observed, "status={status:#x}");
                assert!(!observed.candidate_calls_succeeded);
            }
        }
        #[test]
        fn incomplete_reparse_snapshot_or_event_never_becomes_refusal_evidence() {
            for error in [
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "security descriptor snapshot failed",
                ),
                io::Error::from_raw_os_error(5),
            ] {
                let observed = outcome(reparse(Err(error)));
                assert!(!observed.fully_observed);
                assert!(!observed.candidate_calls_succeeded);
            }
            assert!(outcome(reparse(Ok(true))).candidate_calls_succeeded);
            assert!(!outcome(reparse(Ok(false))).fully_observed);
        }
        #[test]
        fn completed_native_refusals_remain_observed() {
            let denied = || Ok(Directory::<()>::Refused(io::Error::from_raw_os_error(5)));
            assert!(ancestor(denied(), false, |_| Ok(())).unwrap().is_none());
            assert!(ancestor(denied(), true, |_| Ok(())).is_err());
            let collision = collision(
                Ok(Directory::<()>::Refused(io::Error::from_raw_os_error(183))),
                |e| e.raw_os_error() == Some(183),
                |_| Ok(()),
            );
            assert!(outcome(collision).candidate_calls_succeeded);
            let failed = outcome(flush(0xc000_0022_u32.cast_signed(), 0x103, usize::MAX));
            assert!(failed.fully_observed);
            assert!(!failed.candidate_calls_succeeded);
            assert!(outcome(flush(0, 0, 0)).candidate_calls_succeeded);
            assert_eq!(required(Ok(Directory::Opened(7))).unwrap(), 7);
        }
    }
}

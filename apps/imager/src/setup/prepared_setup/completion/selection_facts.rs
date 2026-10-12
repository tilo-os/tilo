//! Retained canonical Store facts, never reconstructed from selector JSON.
use super::boot_review::Selection;
use super::*;
use crate::setup::home_capture as capture;
use crate::setup::prepared_setup::OriginalSelectionPlan;
// Closed borrowed adapters preserve the distinct live-token and reopened-Store
// types. Neither serializable fields nor a caller-defined implementation can
// supply facts; all variants require an existing genuine private custody value.
pub(super) enum HomeFacts<'a> {
    Store(&'a capture::CommittedOriginalSelection),
    Output(&'a capture::offline_output::CommittedOriginalSelection),
}
pub(super) enum ApplicationsFacts<'a> {
    Store(&'a capture::CommittedOriginalApplicationsSelection),
    Output(&'a capture::offline_output::CommittedOriginalApplicationsSelection),
}
macro_rules! borrow_facts {
    ($adapter:ident, $variant:ident, $ty:ty) => {
        impl<'a> From<&'a $ty> for $adapter<'a> {
            fn from(value: &'a $ty) -> Self {
                Self::$variant(value)
            }
        }
    };
}
borrow_facts!(HomeFacts, Store, capture::CommittedOriginalSelection);
borrow_facts!(
    HomeFacts,
    Output,
    capture::offline_output::CommittedOriginalSelection
);
borrow_facts!(
    ApplicationsFacts,
    Store,
    capture::CommittedOriginalApplicationsSelection
);
borrow_facts!(
    ApplicationsFacts,
    Output,
    capture::offline_output::CommittedOriginalApplicationsSelection
);

pub(super) enum Committed {
    Home(capture::CommittedOriginalSelection),
    Applications(capture::CommittedOriginalApplicationsSelection),
}
impl Committed {
    pub(super) fn selected(&self) -> io::Result<Selection> {
        match self {
            Self::Home(v) => Selection::home(v),
            Self::Applications(v) => Selection::applications(v),
        }
    }
}
impl Selection {
    pub(super) fn home<'a>(value: impl Into<HomeFacts<'a>>) -> io::Result<Self> {
        macro_rules! selected {
            ($v:ident) => {
                Ok(Self {
                    schema: 1,
                    operation: $v.declaration().source().operation_id.clone(),
                    medium: $v.declaration().media().clone(),
                    declaration_sha256: Sha256::digest(serde_json::to_vec($v.declaration())?)
                        .into(),
                    attempt: $v.attempt().into(),
                    pending_setup: $v.pending_setup().clone(),
                    completed_setup: $v.completed_setup().clone(),
                    home: Some($v.home().clone()),
                    applications: $v.applications().cloned(),
                    home_inode: Some($v.home_inode()),
                    applications_inode: None,
                    completed_inode: $v.completed_inode(),
                    marker_sha256: $v.marker_sha256(),
                })
            };
        }
        match value.into() {
            HomeFacts::Store(v) => selected!(v),
            HomeFacts::Output(v) => selected!(v),
        }
    }
    pub(super) fn applications<'a>(value: impl Into<ApplicationsFacts<'a>>) -> io::Result<Self> {
        macro_rules! selected {
            ($v:ident) => {
                Ok(Self {
                    schema: 2,
                    operation: $v.declaration().source().operation_id.clone(),
                    medium: $v.declaration().media().clone(),
                    declaration_sha256: $v.declaration().canonical_digest()?,
                    attempt: $v.attempt().into(),
                    pending_setup: $v.pending_setup().clone(),
                    completed_setup: $v.completed_setup().clone(),
                    home: None,
                    applications: Some($v.applications().clone()),
                    home_inode: None,
                    applications_inode: Some($v.applications_inode()),
                    completed_inode: $v.completed_inode(),
                    marker_sha256: $v.marker_sha256(),
                })
            };
        }
        match value.into() {
            ApplicationsFacts::Store(v) => selected!(v),
            ApplicationsFacts::Output(v) => selected!(v),
        }
    }
    pub(super) fn validate_shape(&self) -> io::Result<()> {
        match (
            self.schema,
            &self.home,
            &self.applications,
            self.home_inode,
            self.applications_inode,
        ) {
            (1, Some(home), _, Some(ino), None) if ino != 0 => home.validate()?,
            (2, None, Some(apps), None, Some(ino)) if ino != 0 => apps.validate()?,
            _ => return Err(invalid()),
        }
        if let Some(apps) = &self.applications {
            apps.validate()?;
        }
        Ok(())
    }
    pub(super) fn payload_inode(&self) -> io::Result<u64> {
        self.validate_shape()?;
        self.home_inode
            .or(self.applications_inode)
            .ok_or_else(invalid)
    }
}
pub(super) fn lookup(
    private: &Arc<PinnedDirectory>,
    declaration: &OriginalSelectionPlan,
) -> io::Result<Option<Committed>> {
    match declaration {
        OriginalSelectionPlan::Home(p) => {
            capture::lookup_committed_original_selection(private, p).map(|v| v.map(Committed::Home))
        }
        OriginalSelectionPlan::Applications(p) => {
            capture::lookup_committed_original_applications_selection(private, p)
                .map(|v| v.map(Committed::Applications))
        }
    }
}
pub(super) fn review(
    private: &Arc<PinnedDirectory>,
    declaration: &OriginalSelectionPlan,
    attempt: &str,
    payload: &Arc<PinnedDirectory>,
    completed: &Arc<PinnedDirectory>,
) -> io::Result<Committed> {
    match declaration {
        OriginalSelectionPlan::Home(p) => {
            capture::review_committed_original_selection(private, p, attempt, payload, completed)
                .map(Committed::Home)
        }
        OriginalSelectionPlan::Applications(p) => {
            capture::review_committed_original_applications_selection(
                private, p, attempt, payload, completed,
            )
            .map(Committed::Applications)
        }
    }
}
pub(super) fn uncommitted(
    private: &Arc<PinnedDirectory>,
    declaration: &OriginalSelectionPlan,
) -> io::Result<()> {
    match declaration {
        OriginalSelectionPlan::Home(p) => {
            capture::review_uncommitted_original_selection(private, p)
        }
        OriginalSelectionPlan::Applications(p) => {
            capture::review_uncommitted_original_applications_selection(private, p)
        }
    }
}

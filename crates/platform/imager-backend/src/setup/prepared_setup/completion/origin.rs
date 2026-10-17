//! Private completion authority; serialized declarations cannot construct this value.
use super::*;
use crate::setup::{
    home_capture::offline_output::{VerifiedOriginalApplications, VerifiedOriginalHome},
    prepared_setup::OriginalSelectionPlan,
};
pub(super) enum Origin {
    Home(VerifiedOriginalHome),
    Applications(VerifiedOriginalApplications),
}
impl Origin {
    pub(super) fn retain(&self) -> io::Result<Self> {
        Ok(match self {
            Self::Home(v) => Self::Home(v.retain()?),
            Self::Applications(v) => Self::Applications(v.retain()?),
        })
    }
    pub(super) fn validate_same_origin(&self, other: &Self) -> io::Result<()> {
        match (self, other) {
            (Self::Home(a), Self::Home(b)) => a.validate_same_origin(b),
            (Self::Applications(a), Self::Applications(b)) => a.validate_same_origin(b),
            _ => Err(invalid()),
        }
    }
    pub(super) fn validate(&self) -> io::Result<()> {
        match self {
            Self::Home(v) => v.validate(),
            Self::Applications(v) => v.validate(),
        }
    }
    pub(super) fn validate_setup(&self, setup: &PreparedSetupReview) -> io::Result<()> {
        match self {
            Self::Home(v) => v.validate_setup(setup),
            Self::Applications(v) => v.validate_setup(setup),
        }
    }
    pub(super) fn declaration(&self) -> OriginalSelectionPlan {
        match self {
            Self::Home(v) => OriginalSelectionPlan::Home(v.declaration().clone()),
            Self::Applications(v) => OriginalSelectionPlan::Applications(v.declaration().clone()),
        }
    }
    pub(super) fn schema(&self) -> u32 {
        match self {
            Self::Home(_) => 1,
            Self::Applications(_) => 2,
        }
    }
    pub(super) fn home(&self) -> Option<&crate::setup::portable_home_media::Expectation> {
        match self {
            Self::Home(v) => Some(v.expected()),
            Self::Applications(_) => None,
        }
    }
    pub(super) fn applications(
        &self,
    ) -> Option<&crate::setup::flatpak_transfer::portable::Expectation> {
        match self {
            Self::Home(v) => v.applications(),
            Self::Applications(v) => Some(v.expected()),
        }
    }
    pub(super) fn attempt(&self) -> &str {
        match self {
            Self::Home(v) => v.attempt(),
            Self::Applications(v) => v.attempt(),
        }
    }
    pub(super) fn pending_setup_expectation(
        &self,
    ) -> io::Result<&crate::imager_model::PreparedSetupExpectation> {
        match self {
            Self::Home(v) => v.pending_setup_expectation(),
            Self::Applications(v) => v.pending_setup_expectation(),
        }
    }
    pub(super) fn with_completed_directory<T>(
        &self,
        callback: impl FnOnce(&Arc<PinnedDirectory>) -> io::Result<T>,
    ) -> io::Result<T> {
        match self {
            Self::Home(v) => v.with_completed_directory(callback),
            Self::Applications(v) => v.with_completed_directory(callback),
        }
    }
    pub(super) fn commit_completed_setup(
        &self,
        setup: &crate::imager_model::PreparedSetupExpectation,
        digest: [u8; 32],
    ) -> io::Result<()> {
        match self {
            Self::Home(v) => v.commit_completed_setup(setup, digest),
            Self::Applications(v) => v.commit_completed_setup(setup, digest),
        }
    }
}

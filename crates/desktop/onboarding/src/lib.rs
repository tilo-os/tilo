//! First use as an ordinary app the shell hosts: what an identity is, the
//! ways to create one or bring an existing one here (a code another of your
//! devices prints, a recovery key, a backup file), the fact that nobody can
//! recover it, its backup, the offer of more media formats, the hand-off into
//! the desktop and the tour.
//!
//! The app shows setup and asks; the shell decides. The shell keeps the
//! identity work, how far setup got, the helpers and every secret's ownership,
//! and lends the app an [`OnboardingState`]: the [`Page`] to show, the
//! buffers secrets are typed into, and a queue for what the person asked
//! ([`OnboardingEffect`]). Every action is published, so setup can be driven
//! without a screen; the ones that hand over a secret or choose where
//! identity material is written need the person.
//!
//! Setup is trusted: the shell draws it in its own process, above every
//! application surface, so nothing an application draws can imitate or
//! cover it.

mod app;
mod ceremony;
mod compose;
pub mod copy;
mod effect;
mod page;
mod state;

pub use app::{
    APP_ID, BackupAcknowledgement, Onboarding, OnboardingAction, OnboardingApp, definition,
    onboarding,
};
pub use effect::{CHOICES, FIELDS, OnboardingEffect, SECRET_FIELDS};
pub use page::{
    Codecs, IdentityProblem, JoinedDevice, KeyCopy, Nearby, OnboardingContext, PackState, Page,
    Screen, SecretText, Step, TourPage, WordsCheck,
};
pub use state::{OnboardingState, SecretBuffer, Secrets, WordMistakes};

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

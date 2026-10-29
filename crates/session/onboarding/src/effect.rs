//! What a person, or whoever acts for them, does in setup. Each is one
//! published action, so setup can be driven without a screen, the way its
//! buttons drive it; what an action means on the page that shows is the
//! shell's to decide.
//!
//! Setup reaches no one else and nothing shared: every action is
//! `own_device`. The ones that hand setup a secret or write identity
//! material where a caller says are `attended`: whoever acts for the person
//! is always asked first. A secret is a `secret` parameter, kept out of logs,
//! history and confirmations, and it never comes back in a result.

// `#[derive(Effect)]` names the crate as `tilo_resource`.
use ui::tilo_resource::{self, Effect};
use zeroize::Zeroize;

/// The ways forward a page may offer besides its main one, by name.
pub const CHOICES: &[&str] = &[
    "create",
    "existing",
    "nearby",
    "recovery-key",
    "backup-file",
    "write-down-recovery-key",
    "another-copy",
    "tour",
    "install-codecs",
    "accept-incomplete",
];

/// The fields a value is entered into that are not secret.
pub const FIELDS: &[&str] = &["backup-file"];

/// The secret fields.
pub const SECRET_FIELDS: &[&str] = &[
    "backup-passphrase",
    "prepared-password",
    "new-passphrase",
    "new-passphrase-confirmation",
    "recovery-key",
    "recovery-key-word-1",
    "recovery-key-word-2",
    "recovery-key-word-3",
];

/// Setup's actions.
#[derive(Clone, PartialEq, Effect)]
#[effect(operation = "so.tilo.onboarding.step")]
pub enum OnboardingEffect {
    /// Continue
    ///
    /// Takes the page's main way forward.
    #[effect(own_device)]
    Continue,
    /// Back
    ///
    /// Goes back a step.
    #[effect(own_device, idempotent)]
    Back,
    /// Choose
    ///
    /// Takes one of the other ways forward the page offers.
    #[effect(own_device)]
    Choose {
        #[param("Which way forward", choices = CHOICES.iter().copied())]
        option: String,
    },
    /// Acknowledge
    ///
    /// Ticks or unticks the page's acknowledgement.
    #[effect(own_device, idempotent)]
    Acknowledge {
        #[param("Whether it is acknowledged")]
        checked: bool,
    },
    /// Enter a value
    #[effect(own_device, idempotent)]
    Enter {
        #[param("The field", choices = FIELDS.iter().copied())]
        field: String,
        #[param("What it holds")]
        value: String,
    },
    /// Enter a secret
    ///
    /// Types a passphrase, password or recovery key into its field.
    #[effect(own_device, attended)]
    EnterSecret {
        #[param("The field", choices = SECRET_FIELDS.iter().copied())]
        field: String,
        #[param(secret, "The secret")]
        value: String,
    },
    /// Choose a file
    ///
    /// Opens the file chooser the page offers.
    #[effect(own_device)]
    Browse,
    /// Pick a file
    ///
    /// Answers the open file chooser with a file.
    #[effect(own_device, attended)]
    Pick {
        #[param("The file")]
        path: String,
    },
    /// Close the file chooser
    #[effect(own_device, idempotent)]
    CloseChooser,
    /// Skip
    ///
    /// Leaves this step for later or skips it.
    #[effect(own_device)]
    Skip,
    /// Try again
    #[effect(own_device)]
    Retry,
    /// Cancel
    ///
    /// Stops the work under way.
    #[effect(own_device)]
    Cancel,
    /// Enter the desktop
    #[effect(own_device)]
    Finish,
}

impl OnboardingEffect {
    /// A choice by name.
    #[must_use]
    pub fn choose(option: &str) -> Self {
        Self::Choose {
            option: option.to_owned(),
        }
    }

    /// Forget a secret this action carries.
    pub fn forget_secret(&mut self) {
        if let Self::EnterSecret { value, .. } = self {
            value.zeroize();
        }
    }
}

impl std::fmt::Debug for OnboardingEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Continue => f.write_str("Continue"),
            Self::Back => f.write_str("Back"),
            Self::Choose { option } => f.debug_struct("Choose").field("option", option).finish(),
            Self::Acknowledge { checked } => f
                .debug_struct("Acknowledge")
                .field("checked", checked)
                .finish(),
            Self::Enter { field, value } => f
                .debug_struct("Enter")
                .field("field", field)
                .field("value", value)
                .finish(),
            Self::EnterSecret { field, .. } => f
                .debug_struct("EnterSecret")
                .field("field", field)
                .finish_non_exhaustive(),
            Self::Browse => f.write_str("Browse"),
            Self::Pick { path } => f.debug_struct("Pick").field("path", path).finish(),
            Self::CloseChooser => f.write_str("CloseChooser"),
            Self::Skip => f.write_str("Skip"),
            Self::Retry => f.write_str("Retry"),
            Self::Cancel => f.write_str("Cancel"),
            Self::Finish => f.write_str("Finish"),
        }
    }
}

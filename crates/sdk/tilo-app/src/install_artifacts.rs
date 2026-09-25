//! Semantic installer-artifact service facade.
//!
//! Applications receive opaque catalog selections, operation tokens, progress,
//! and typed preference outcomes. Native cache providers, transport manifests,
//! preparation receipts, raw handles, and consumption remain service-private.
//! The facade exposes no caller-selected backend, origin, or trusted catalog.
//!
//! ```no_run
//! use tilo_app::install_artifacts::{Client, Error, PreferenceState, ReleaseSelection, Token};
//!
//! let _: fn(&Client, ReleaseSelection, Vec<u8>) -> Result<Token, Error> = Client::start;
//! let _: fn(&Client, ReleaseSelection) -> Result<Token, Error> = Client::resume;
//! let _: fn(&Client) -> PreferenceState = Client::preference;
//! ```
//!
//! Normal SDK builds do not expose the service's development fixture module.
//! Enabling fixture support on a direct service dev dependency does not widen
//! this facade.
pub use tilo_install_artifacts::{
    Availability, Client, Error, ErrorKind, Phase, PreferenceCommit, PreferenceDurability,
    PreferenceFailure, PreferenceNamespace, PreferencePublication, PreferenceState, Progress,
    ReleaseSelection, Status, Token, TransferPhase, availability, bundled_catalog_json,
    bundled_selections, catalog_architecture, verify_local_components,
};
pub use tilo_install_artifacts::{catalog_data, format};

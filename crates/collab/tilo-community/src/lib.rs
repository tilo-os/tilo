//! The Tilo Community, for every host that holds a person's rooms.
//!
//! Everyone is in the community by default (`docs/architecture/feedback.md`):
//! a host joins the public community room by redeeming the release's
//! standing invitation, opens the person's private room with the Tilo team,
//! retries while the community's host cannot be reached, never re-joins a
//! person who left, and takes a refusal by the community's host as final
//! for that invitation. This crate is those rules and the configuration
//! they read; each host observes its own rooms and performs the steps
//! through [`CommunityHost`].
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "test assertions"
    )
)]

pub mod config;
pub mod join;
pub mod keeper;

pub use config::{
    COMMUNITY_CONFIG_ENV, CommunityConfig, CommunityConfigError, DEFAULT_COMMUNITY_CONFIG,
    TeamPrincipal, config_path, load, load_from, team_direct_room,
};
pub use join::{AUTO_JOIN_FIRST_BACKOFF, AUTO_JOIN_MAX_BACKOFF, AutoJoin, Observed, Step};
pub use keeper::{
    COMMUNITY_CHECK_EVERY, CommunityHost, CommunityKeeper, Holding, KeeperNote, StandingCommunity,
    standing,
};

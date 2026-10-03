//! Small ui-toolkit compositions shared across the store surface.
//!
//! Kept minimal — the app's own root widget composes the top-level
//! layout out of `ui::widget::{Column, Row, Container, Text, Button,
//! Scrollable, TextInput, ToggleGroup}`. This module only holds the
//! reusable pieces that show up on more than one screen so the
//! layout code isn't repeated.

pub mod artwork;
pub mod card;
pub mod chips;

pub use artwork::Artwork;
pub use card::{CARD_HEIGHT, CardAction, CardDetails, CardLayout, store_card};
pub use chips::{FilterChipSelection, filter_chip_row};

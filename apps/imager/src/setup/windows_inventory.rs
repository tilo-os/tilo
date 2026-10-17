//! Windows inventory DTOs and semantic intents are backend-owned.
//! Actual-file grouping and destination presentation remain frontend concerns.
mod review;
pub use imager_backend::windows_inventory::*;
pub use review::review_categories;

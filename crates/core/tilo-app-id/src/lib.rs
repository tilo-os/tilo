//! The first-party apps by their ids, the records some of them keep, and the fleet's name for
//! a development build's app. An app id itself is `tilo_member_types::AppId`.

pub mod apps;
pub mod chat;
pub mod passwords;

/// `package_id` as the fleet names it: a development build's `dev.tilo.` app is its `so.tilo.` one.
/// An id an app was published under before is the policy's to resolve, from what the app's signed
/// package declares when it loads (`tilo_action_policy::earlier`).
#[must_use]
pub fn fleet_id(package_id: &str) -> std::borrow::Cow<'_, str> {
    package_id
        .strip_prefix("dev.tilo.")
        .map_or(std::borrow::Cow::Borrowed(package_id), |name| {
            std::borrow::Cow::Owned(format!("so.tilo.{name}"))
        })
}

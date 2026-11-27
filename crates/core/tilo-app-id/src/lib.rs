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

/// The app the device's development build `package_id` is a build of, or `package_id` itself.
///
/// A device's own build of an app runs under `<app>.dev.<device key>` (the mark
/// `tilo_app_package::dev_app_id` writes): a different id from the published app's, so it never
/// takes that app's install or grants. The work the device does for the app — its engine, its
/// connectors — is the app's own and is registered under the app's published id, so a build asks
/// for it by the id it is a build of.
#[must_use]
pub fn build_of(package_id: &str) -> &str {
    package_id
        .rsplit_once(".dev.")
        .filter(|(_, device)| {
            device.len() == 16
                && device
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
        .map_or(package_id, |(app, _)| app)
}

#[cfg(test)]
mod tests {
    use super::build_of;

    #[test]
    fn a_development_build_is_the_app_it_is_a_build_of() {
        assert_eq!(
            build_of("so.tilo.store.dev.2f8a8fd7d925ddd2d"),
            "so.tilo.store"
        );
        assert_eq!(
            build_of("local.dev.reading.list.dev.0123456789abcdef"),
            "local.dev.reading.list",
            "only the device's mark at the end names a build"
        );
        for app in [
            "so.tilo.store",
            "so.tilo.dev.guide",
            "so.tilo.store.dev.nothex0123456",
            "so.tilo.store.dev.0123456789ABCDEF",
        ] {
            assert_eq!(build_of(app), app, "{app} is an app's own id");
        }
    }
}

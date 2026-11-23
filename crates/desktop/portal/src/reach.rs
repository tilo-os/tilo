//! Which backend interfaces an app its manifest confines may reach.
//!
//! A package names the portals it may use in its manifest's
//! `[sandbox.portals]`; its bus proxy lets it reach only those. The backend
//! holds the same line on its own: a call the frontend forwards for a
//! confined app ([`crate::PortalCaller::portals`]) is refused unless the
//! interface it arrives on serves a portal the app may use, so a crafted or
//! misrouted call to a denied portal gets nothing either.

/// Each backend interface a manifest can open, with the manifest's names
/// for the portals it serves. `Access` asks the person on behalf of the
/// camera and location portals.
const SERVED: &[(&str, &[&str])] = &[
    (
        "org.freedesktop.impl.portal.Access",
        &["camera", "location"],
    ),
    ("org.freedesktop.impl.portal.AppChooser", &["open-uri"]),
    ("org.freedesktop.impl.portal.FileChooser", &["file-chooser"]),
    ("org.freedesktop.impl.portal.ScreenCast", &["screencast"]),
    ("org.freedesktop.impl.portal.Secret", &["secrets"]),
    ("org.freedesktop.impl.portal.Wallpaper", &["wallpaper"]),
];

/// Every app with a window may post notifications without naming them.
const OPEN: &[&str] = &["org.freedesktop.impl.portal.Notification"];

/// Whether an app that may use the portals `allowed` (manifest names) may
/// call `interface`: one of them is served there, or the interface is open
/// to every app. Any other interface (a screenshot, the remote desktop) no
/// manifest can name, so a confined app never reaches it.
#[must_use]
pub fn reaches(allowed: &std::collections::BTreeSet<String>, interface: &str) -> bool {
    OPEN.contains(&interface)
        || SERVED
            .iter()
            .find(|(served, _)| *served == interface)
            .is_some_and(|(_, portals)| portals.iter().any(|portal| allowed.contains(*portal)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_confined_app_reaches_only_the_interfaces_of_the_portals_it_may_use() {
        let allowed = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
        let screencast = "org.freedesktop.impl.portal.ScreenCast";
        let chooser = "org.freedesktop.impl.portal.FileChooser";
        let notification = "org.freedesktop.impl.portal.Notification";
        let none = allowed(&[]);
        assert!(reaches(&none, notification));
        assert!(!reaches(&none, screencast));
        let choosing = allowed(&["file-chooser"]);
        assert!(reaches(&choosing, chooser));
        assert!(!reaches(&choosing, screencast));
        assert!(reaches(&allowed(&["screencast"]), screencast));
        // No manifest name opens these.
        let every = allowed(
            &SERVED
                .iter()
                .flat_map(|(_, names)| names.iter().copied())
                .collect::<Vec<_>>(),
        );
        for interface in [
            "org.freedesktop.impl.portal.Screenshot",
            "org.freedesktop.impl.portal.RemoteDesktop",
            "org.freedesktop.impl.portal.InputCapture",
        ] {
            assert!(!reaches(&every, interface), "{interface}");
        }
    }

    /// Every name the table opens an interface with is one a manifest can declare.
    #[test]
    fn every_portal_named_here_is_a_manifest_portal() {
        for (_, names) in SERVED {
            for name in *names {
                assert!(
                    tilo_app_package::manifest::PORTALS
                        .iter()
                        .any(|(portal, _)| portal == name),
                    "{name}"
                );
            }
        }
    }
}

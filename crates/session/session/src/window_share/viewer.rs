//! Who views this device's windows, as the bar names them.
//!
//! A viewer reaches a share as a device: the endpoint its ask came from. The bar names people and
//! devices, so each viewer carries what its workspace calls it ([`called`]): another person by the
//! name they go by, the person's own other device by what that device calls itself. A viewer
//! nothing names carries no name, and the bar says so in words: its handle and its key are never
//! a name.

use collab_types::presence::Shown;
use iroh::EndpointId;
use tilo_desktop::chrome::ActiveExposure;

use super::WindowShares;
use crate::sync::Client;

impl WindowShares {
    /// What the bar says is being shared: each member viewing a window of this device, what the
    /// workspace calls it, and whether it may control the window.
    pub(crate) fn exposures(&self) -> Vec<ActiveExposure> {
        let hosted = self.hosted.iter().map(|(tab, hosted)| (*tab, Some(hosted)));
        // A window whose page is published is drawn by its app: nobody controls it from here.
        let pages = self.pages.keys().map(|tab| (*tab, None));
        hosted
            .chain(pages)
            .flat_map(|(tab, hosted)| {
                self.asked
                    .iter()
                    .filter(move |((asking, _), _)| *asking == tab)
                    .map(move |((_, peer), asked)| ActiveExposure {
                        peer: *peer,
                        surface: tab,
                        can_control: hosted
                            .is_some_and(|hosted| hosted.open || hosted.controlled.contains(peer)),
                        name: asked.name.clone(),
                    })
            })
            .collect()
    }
}

/// What `workspace` calls the viewer on `device` , as this device heard it there: nothing while
/// nothing names it.
pub(super) fn name_of(sync: &Client, workspace: &str, device: EndpointId) -> String {
    let own = sync.person().map(hex::encode);
    sync.workspace_device(workspace, device)
        .map(|heard| called(&heard, own.as_deref()))
        .unwrap_or_default()
}

/// What the person at this device ( `own` , their user root in hex) calls the viewer `heard` : a
/// device of their own by its name, anyone else by the name they go by.
fn called(heard: &Shown, own: Option<&str>) -> String {
    let own_device = own.is_some_and(|own| heard.user.as_deref() == Some(own));
    let name = if own_device {
        tilo_user_identity::device_name(&heard.device)
    } else {
        ui::presence::shown_name(&heard.label)
    };
    name.unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: &str = "5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d";
    const OTHER: &str = "a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7";

    /// A device heard in the workspace: whose it is, what it calls itself and what its person goes
    /// by.
    fn heard(user: Option<&str>, device: &str, label: &str) -> Shown {
        Shown {
            user: user.map(str::to_owned),
            device: device.to_owned(),
            label: label.to_owned(),
            ..Shown::default()
        }
    }

    #[test]
    fn the_persons_own_other_device_goes_by_its_device_name() {
        assert_eq!(
            called(&heard(Some(OWN), "Framework", "Xaver"), Some(OWN)),
            "Framework",
            "their own name would say nothing of which device watches"
        );
    }

    #[test]
    fn another_persons_device_goes_by_the_persons_name() {
        assert_eq!(
            called(&heard(Some(OTHER), "Studio", "Ada"), Some(OWN)),
            "Ada"
        );
        assert_eq!(
            called(&heard(None, "", "Ada"), Some(OWN)),
            "Ada",
            "a person no certificate named yet goes by the name their presence says"
        );
        assert_eq!(
            called(&heard(Some(OTHER), "Studio", "Ada"), None),
            "Ada",
            "a session with no person of its own has no other device"
        );
    }

    #[test]
    fn a_viewer_nothing_names_goes_by_no_name() {
        // A device its person never named carries the stand-in it was set up with, which is no
        // name of theirs.
        for unnamed in ["", " ", tilo_user_identity::UNNAMED_PRIMARY_DEVICE] {
            assert_eq!(called(&heard(Some(OWN), unnamed, "Xaver"), Some(OWN)), "");
        }
        assert_eq!(
            called(&heard(Some(OTHER), "Studio", OTHER), Some(OWN)),
            "",
            "a key is no name"
        );
        assert_eq!(called(&Shown::default(), Some(OWN)), "");
    }
}

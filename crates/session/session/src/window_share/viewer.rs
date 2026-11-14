//! Who views this device's windows, as the bar names them.
//!
//! A viewer reaches a share as a device: the endpoint its ask came from. The bar names people and
//! devices, so each viewer carries what its workspace calls it ([`called`]): another person by the
//! name they go by, the person's own other device by what that device calls itself. A viewer
//! nothing names carries no name, and the bar says so in words: its handle and its key are never
//! a name.
//!
//! A viewer is a device that asked to see a window, which is not yet one that sees it. Whether
//! anything of a window goes out is answered in one place ([`WindowShares::goes_out`]): the
//! share's reading says it (`streaming`), the window's menu reads it there, and the bar is told it
//! with each viewer. So the bar and the menu say that a viewer watches only a window the reading
//! says is sent, and until then that it waits.

use collab_types::presence::Shown;
use iroh::EndpointId;
use tilo_desktop::chrome::ActiveExposure;
use tilo_desktop_model::TabId;

use super::WindowShares;
use crate::sync::Client;

impl WindowShares {
    /// Whether anything of the window of `tab` goes out to the devices showing it now: its
    /// pictures while a member's desk asks the workspace's plane for them, its page once its app
    /// handed a whole one over for a desk that asks. Both are noted in one place, once a pass
    /// ([`Self::follow_streams`]), and this is the one answer the share's reading, the window's
    /// menu and the bar give.
    pub(super) fn goes_out(&self, tab: TabId) -> bool {
        self.streaming.contains(&tab)
    }

    /// What the bar says is being shared: each member that asked to see a window of this device,
    /// what the workspace calls it, whether it may control the window, and whether anything of
    /// the window goes out, which is when the bar says the member watches it.
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
                        streaming: self.goes_out(tab),
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

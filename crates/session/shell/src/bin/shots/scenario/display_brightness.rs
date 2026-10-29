//! A display's brightness readout across several displays: on the display
//! that changed and on the one in use, then on every display with Shift.

use shell::shell::chrome_state::ChromeState;

use super::super::driver::ShellDriver;
use super::{Action, Scenario};

/// A display's brightness readout as the keys raise it across `--outputs`:
/// the display not in use dimmed (`brightness_other_display`), or every
/// display with Shift (`brightness_all_displays`). Headless outputs have no
/// connectors, so they are named the way a laptop with desk monitors names
/// them (`eDP-1`, then `DP-1`, ...) first.
pub(super) fn seed(driver: &mut ShellDriver, preset: &str) {
    use shell::shell::osd::{Osd, OsdDisplays};
    if driver
        .outputs()
        .iter()
        .any(|output| output.identity.connector.is_empty())
    {
        let mut outputs = driver.outputs().to_vec();
        for (index, output) in outputs.iter_mut().enumerate() {
            output.identity.connector = if index == 0 {
                "eDP-1".to_owned()
            } else {
                format!("DP-{index}")
            };
        }
        driver.set_outputs(&outputs);
    }
    let focused = driver.host().focused_connector();
    let connectors: Vec<String> = driver
        .host()
        .output_geometry()
        .iter()
        .map(|output| output.connector.clone())
        .collect();
    if preset == "brightness_all_displays" {
        let all = shell::shell::i18n::t("osd-all-displays");
        let osd = Osd::display_brightness(0.6, Some(&all))
            .on_displays(OsdDisplays::of(connectors.iter().map(String::as_str)));
        driver.with_chrome(|st: &mut ChromeState| st.show_osd(osd));
        return;
    }
    let other = connectors
        .iter()
        .find(|connector| Some(connector.as_str()) != focused.as_deref())
        .map_or("", String::as_str);
    let name = if other == "eDP-1" {
        "Built-in display"
    } else {
        "Dell U2723"
    };
    let osd = Osd::display_brightness(0.4, Some(name)).on_displays(OsdDisplays::of([other]));
    driver.with_chrome(|st: &mut ChromeState| st.show_osd(osd));
}

/// The readout across displays, for `--outputs` runs.
pub(super) fn scenario() -> Scenario {
    Scenario {
        name: "display_brightness_osd",
        description: "A display's brightness readout across displays (run with --outputs): on the display that changed and the one in use, then on every display with Shift.",
        actions: vec![
            Action::Wait(4),
            Action::SeedOsd("brightness_other_display"),
            Action::Wait(14),
            Action::Snapshot("01-other-display"),
            Action::SeedOsd("brightness_all_displays"),
            Action::Wait(14),
            Action::Snapshot("02-all-displays"),
        ],
    }
}

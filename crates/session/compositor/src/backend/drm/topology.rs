//! The journal lines that say which GPU drives which display.
//!
//! The DRM backend drives every KMS card the session can open, so a hybrid
//! laptop's internal panel (on the integrated GPU) and its HDMI or DP
//! outputs (often wired to the discrete GPU) are lit by different cards. When
//! one of them stays dark, the session log has to answer which cards were
//! tried, which connectors each one attempted and how that went, and what the
//! backend ended up driving. Each bring-up attempt is one line, and the driven
//! topology is one line written whenever it changes. Both are plain
//! `key=value` text on stderr, which the session log and the journal keep.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// One output being driven, as the topology line lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Driven {
    /// The connector name (`eDP-1`).
    pub connector_name: String,
    /// The card's device number.
    pub device: u64,
    /// The mode in physical pixels.
    pub size: (i32, i32),
    pub refresh_mhz: i32,
    /// The top-left corner in the logical layout.
    pub logical_position: (i32, i32),
    pub scale: f64,
}

/// How one connector's bring-up went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectorAttempt<'a> {
    Lit,
    /// The display configuration turned this output off.
    Disabled,
    Failed(&'a str),
}

/// The journal line for one connector bring-up attempt on `card`.
pub fn connector_attempt_line(
    card: &Path,
    connector: &str,
    mode: (u16, u16),
    attempt: ConnectorAttempt<'_>,
) -> String {
    let (width, height) = mode;
    let mut line = format!(
        "tilo compositor: DRM connector attempt card={} connector={connector} mode={width}x{height}",
        card.display()
    );
    let _ = match attempt {
        ConnectorAttempt::Lit => write!(line, " result=lit"),
        ConnectorAttempt::Disabled => write!(line, " result=disabled"),
        ConnectorAttempt::Failed(reason) => write!(line, " result=failed reason={reason:?}"),
    };
    line
}

/// The journal line for the topology being driven: every held card with
/// how many outputs it lights, then every output in layout order (the
/// primary first) with the card driving it.
///
/// A card without outputs is listed too: a hybrid laptop's discrete GPU
/// with nothing plugged in is held for a later hotplug, and seeing it here
/// tells "not opened" apart from "nothing connected".
pub fn topology_line(cards: &BTreeMap<u64, PathBuf>, live: &[Driven]) -> String {
    let card_name = |device: u64| {
        cards.get(&device).map_or_else(
            || format!("dev:{device}"),
            |path| path.display().to_string(),
        )
    };
    let outputs_on = |device: u64| live.iter().filter(|l| l.device == device).count();
    let driven_cards = cards
        .keys()
        .filter(|&&device| outputs_on(device) > 0)
        .count();
    let mut journal = format!(
        "tilo compositor: DRM topology cards={} driven_cards={driven_cards} outputs={}",
        cards.len(),
        live.len()
    );
    for &device in cards.keys() {
        let _ = write!(
            journal,
            "; card {} outputs={}",
            card_name(device),
            outputs_on(device)
        );
    }
    for entry in live {
        let refresh = entry.refresh_mhz;
        let _ = write!(
            journal,
            "; output {} on {} {}x{}@{}.{:03}Hz at {},{} scale {}",
            entry.connector_name,
            card_name(entry.device),
            entry.size.0,
            entry.size.1,
            refresh / 1000,
            refresh % 1000,
            entry.logical_position.0,
            entry.logical_position.1,
            entry.scale,
        );
    }
    journal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn driven(device: u64, size: (i32, i32), name: &str) -> Driven {
        Driven {
            connector_name: name.to_owned(),
            device,
            size,
            refresh_mhz: 60_000,
            logical_position: (0, 0),
            scale: 1.0,
        }
    }

    #[test]
    fn a_connector_attempt_names_its_card_connector_and_outcome() {
        let card = Path::new("/dev/dri/card1");
        assert_eq!(
            connector_attempt_line(card, "eDP-1", (2880, 1800), ConnectorAttempt::Lit),
            "tilo compositor: DRM connector attempt card=/dev/dri/card1 connector=eDP-1 mode=2880x1800 result=lit"
        );
        assert_eq!(
            connector_attempt_line(card, "HDMI-A-1", (1920, 1080), ConnectorAttempt::Disabled),
            "tilo compositor: DRM connector attempt card=/dev/dri/card1 connector=HDMI-A-1 mode=1920x1080 result=disabled"
        );
        assert_eq!(
            connector_attempt_line(
                card,
                "DP-2",
                (3840, 2160),
                ConnectorAttempt::Failed("no free crtc can drive this connector")
            ),
            "tilo compositor: DRM connector attempt card=/dev/dri/card1 connector=DP-2 mode=3840x2160 result=failed reason=\"no free crtc can drive this connector\""
        );
    }

    #[test]
    fn the_topology_lists_every_held_card_and_each_output_with_its_card() {
        let cards = BTreeMap::from([
            (226, PathBuf::from("/dev/dri/card0")),
            (227, PathBuf::from("/dev/dri/card1")),
        ]);
        let mut panel = driven(227, (2880, 1800), "eDP-1");
        panel.scale = 2.0;
        let mut hdmi = driven(226, (1920, 1080), "HDMI-A-1");
        hdmi.logical_position = (1440, 0);
        assert_eq!(
            topology_line(&cards, &[panel, hdmi]),
            "tilo compositor: DRM topology cards=2 driven_cards=2 outputs=2\
             ; card /dev/dri/card0 outputs=1; card /dev/dri/card1 outputs=1\
             ; output eDP-1 on /dev/dri/card1 2880x1800@60.000Hz at 0,0 scale 2\
             ; output HDMI-A-1 on /dev/dri/card0 1920x1080@60.000Hz at 1440,0 scale 1"
        );
    }

    #[test]
    fn a_held_card_without_outputs_is_listed_as_driving_none() {
        let cards = BTreeMap::from([
            (226, PathBuf::from("/dev/dri/card0")),
            (227, PathBuf::from("/dev/dri/card1")),
        ]);
        let panel = driven(227, (1920, 1200), "eDP-1");
        let line = topology_line(&cards, &[panel]);
        assert!(
            line.starts_with("tilo compositor: DRM topology cards=2 driven_cards=1 outputs=1; card /dev/dri/card0 outputs=0; card /dev/dri/card1 outputs=1;"),
            "{line}"
        );
        assert_eq!(
            topology_line(&cards, &[]),
            "tilo compositor: DRM topology cards=2 driven_cards=0 outputs=0\
             ; card /dev/dri/card0 outputs=0; card /dev/dri/card1 outputs=0"
        );
    }
}

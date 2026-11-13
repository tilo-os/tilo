//! The order of the Wi-Fi list and how much of it the pane shows.

use os_actions::WifiNetworkInfo;

pub(crate) const OTHERS_SHOWN: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Standing {
    Connected,
    Known,
    Other,
}

fn standing(network: &WifiNetworkInfo, known: &impl Fn(&WifiNetworkInfo) -> bool) -> Standing {
    if network.connected {
        Standing::Connected
    } else if known(network) {
        Standing::Known
    } else {
        Standing::Other
    }
}

pub(crate) fn arranged(
    networks: &[WifiNetworkInfo],
    known: impl Fn(&WifiNetworkInfo) -> bool,
) -> Vec<&WifiNetworkInfo> {
    let mut order: Vec<&WifiNetworkInfo> = networks.iter().collect();
    order.sort_by(|a, b| {
        standing(a, &known)
            .cmp(&standing(b, &known))
            .then(b.signal.cmp(&a.signal))
            .then_with(|| a.ssid.to_lowercase().cmp(&b.ssid.to_lowercase()))
            .then_with(|| a.ssid.cmp(&b.ssid))
    });
    order
}

pub(crate) fn shortlist<'a>(
    arranged: &[&'a WifiNetworkInfo],
    known: impl Fn(&WifiNetworkInfo) -> bool,
) -> Vec<&'a WifiNetworkInfo> {
    let mut others = 0;
    arranged
        .iter()
        .copied()
        .filter(|network| {
            if standing(network, &known) != Standing::Other {
                return true;
            }
            others += 1;
            others <= OTHERS_SHOWN
        })
        .collect()
}

#[derive(Debug, Default)]
pub(crate) struct HeldList {
    shown: Vec<WifiNetworkInfo>,
}

impl HeldList {
    /// The rows to show for `fresh`, the list as it stands now. Not
    /// `held`, that is `fresh`. Held, the rows shown last keep their
    /// places with their new facts (a network out of range keeps its last
    /// ones until the pointer leaves), and networks new to the list follow
    /// them.
    pub(crate) fn rows(&mut self, fresh: Vec<WifiNetworkInfo>, held: bool) -> Vec<WifiNetworkInfo> {
        if !held || self.shown.is_empty() {
            self.shown = fresh;
            return self.shown.clone();
        }
        let mut rows: Vec<WifiNetworkInfo> = self
            .shown
            .iter()
            .map(|old| {
                fresh
                    .iter()
                    .find(|network| network.ssid == old.ssid)
                    .cloned()
                    .unwrap_or_else(|| old.clone())
            })
            .collect();
        rows.extend(
            fresh
                .into_iter()
                .filter(|network| !self.shown.iter().any(|old| old.ssid == network.ssid)),
        );
        self.shown.clone_from(&rows);
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(ssid: &str, signal: u8) -> WifiNetworkInfo {
        WifiNetworkInfo {
            ssid: ssid.into(),
            signal,
            secure: true,
            security: os_actions::WifiSecurity::Wpa2,
            connected: false,
            known: false,
        }
    }

    fn names(networks: &[&WifiNetworkInfo]) -> Vec<String> {
        networks
            .iter()
            .map(|network| network.ssid.clone())
            .collect()
    }

    fn known_of(saved: &'static [&'static str]) -> impl Fn(&WifiNetworkInfo) -> bool {
        move |network| saved.contains(&network.ssid.as_str())
    }

    #[test]
    fn the_network_in_use_leads_then_known_networks_then_the_rest_strongest_first() {
        let mut home = net("Home", 2);
        home.connected = true;
        let networks = vec![
            net("cafe", 3),
            net("Office", 4),
            home,
            net("Library", 1),
            net("Bakery", 3),
            net("Attic", 4),
        ];
        let order = arranged(&networks, known_of(&["Library", "Attic"]));
        assert_eq!(
            names(&order),
            ["Home", "Attic", "Library", "Office", "Bakery", "cafe"],
            "equal levels sort by name, case aside"
        );
    }

    /// `NetworkManager` lists networks by their raw strength, which drifts
    /// on every scan; within one level the drift reorders its list but
    /// never the pane's.
    #[test]
    fn drifting_raw_strength_within_a_level_moves_no_row() {
        let first = vec![net("Alpha", 3), net("Beta", 3), net("Gamma", 2)];
        let rescanned = vec![net("Gamma", 2), net("Beta", 3), net("Alpha", 3)];
        let known = known_of(&[]);
        assert_eq!(
            names(&arranged(&first, &known)),
            names(&arranged(&rescanned, &known))
        );
        let stronger = vec![net("Alpha", 3), net("Beta", 3), net("Gamma", 4)];
        assert_eq!(
            names(&arranged(&stronger, &known)),
            ["Gamma", "Alpha", "Beta"],
            "a new level moves it"
        );
    }

    #[test]
    fn the_short_list_keeps_every_known_network_and_five_others() {
        let mut networks: Vec<WifiNetworkInfo> =
            (0..9).map(|at| net(&format!("Other {at}"), 2)).collect();
        networks.push(net("Saved far", 1));
        networks.push(net("Saved near", 4));
        let mut using = net("In use", 1);
        using.connected = true;
        networks.push(using);
        let known = known_of(&["Saved far", "Saved near"]);
        let order = arranged(&networks, &known);
        let short = shortlist(&order, &known);
        assert_eq!(short.len(), 3 + OTHERS_SHOWN);
        assert_eq!(
            names(&short[..3]),
            ["In use", "Saved near", "Saved far"],
            "the network in use and every known one are always listed"
        );
        assert_eq!(order.len(), 12, "the full list is what Show all counts");
        let few = vec![net("One", 2), net("Two", 3)];
        let few_order = arranged(&few, &known);
        assert_eq!(shortlist(&few_order, &known).len(), 2);
    }

    #[test]
    fn a_held_list_keeps_its_rows_in_place_until_the_pointer_leaves() {
        let mut list = HeldList::default();
        let shown = list.rows(vec![net("Alpha", 4), net("Beta", 3)], false);
        assert_eq!(shown.len(), 2);
        let mut alpha = net("Alpha", 1);
        alpha.connected = true;
        let held = list.rows(vec![net("Beta", 3), net("Gamma", 2), alpha.clone()], true);
        let held_names: Vec<&str> = held.iter().map(|network| network.ssid.as_str()).collect();
        assert_eq!(held_names, ["Alpha", "Beta", "Gamma"], "new ones follow");
        assert!(held[0].connected, "a held row still takes its new facts");
        let gone = list.rows(vec![net("Gamma", 2)], true);
        assert_eq!(gone.len(), 3, "a row out of range stays while held");
        let released = list.rows(vec![net("Gamma", 2), alpha], false);
        let released: Vec<&str> = released
            .iter()
            .map(|network| network.ssid.as_str())
            .collect();
        assert_eq!(released, ["Gamma", "Alpha"], "let go, it is the list now");
    }
}

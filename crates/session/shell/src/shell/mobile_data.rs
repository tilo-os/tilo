//! This device's mobile data, counted where the modem is read: the session
//! reads the modems' traffic totals and counts them into the person's
//! device-held `mobile-data-usage` section through its `count-data-used`
//! verb, whatever window is open. So the count is a fact of the device that
//! agents, scripts and Settings read alike, not something a page keeps up
//! while it is shown.

use os_actions::modem::Traffic;
use settings_store::InvokingAction;

/// How often the modems are read while this device has one.
const WITH_MODEM: std::time::Duration = std::time::Duration::from_secs(60);
/// How often a device without a modem looks again for one.
const WITHOUT_MODEM: std::time::Duration = std::time::Duration::from_secs(600);

/// The section's kind, and its verb that counts a traffic total.
const SECTION: &str = "settings.mobile-data-usage";
const COUNT: &str = "count-data-used";

/// Count this device's mobile data for as long as the session runs.
///
/// # Errors
/// The counting thread could not start.
pub(crate) fn spawn() -> std::io::Result<()> {
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|id| id.trim().to_owned());
    std::thread::Builder::new()
        .name("tilo-mobile-data".into())
        .spawn(move || {
            loop {
                let modems = os_actions::read_mobile()
                    .map(|reading| reading.modems)
                    .unwrap_or_default();
                let traffic: Vec<Option<Traffic>> =
                    modems.iter().map(|modem| modem.traffic).collect();
                if let Some(arguments) =
                    count_arguments(&traffic, boot.as_deref(), tilo_clock::unix_now_i64())
                    && let Err(error) = super::settings_records::change(
                        &super::settings_records::address(SECTION, COUNT),
                        &arguments,
                        true,
                        &InvokingAction::action("mobile-data.count"),
                    )
                {
                    tracing::warn!(%error, "the mobile data used was not counted");
                }
                std::thread::park_timeout(if modems.is_empty() {
                    WITHOUT_MODEM
                } else {
                    WITH_MODEM
                });
            }
        })
        .map(drop)
}

/// The arguments of `count-data-used` for the modems' `traffic` read at `at`
/// in the boot `boot`: their totals summed, or none read. `None` for a
/// device without a modem, which has nothing to count.
fn count_arguments(
    traffic: &[Option<Traffic>],
    boot: Option<&str>,
    at: i64,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    if traffic.is_empty() {
        return None;
    }
    let total = traffic
        .iter()
        .flatten()
        .map(|traffic| traffic.total())
        .reduce(u64::saturating_add);
    let mut arguments = serde_json::Map::new();
    arguments.insert("total".into(), total.into());
    arguments.insert("boot".into(), boot.into());
    arguments.insert("at".into(), at.into());
    Some(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The modems' totals are summed into the verb's arguments; a device
    /// without a modem counts nothing. (Settings' tests count these same
    /// arguments into the section with no page open.)
    #[test]
    fn the_modems_traffic_is_counted_as_one_total() {
        assert!(count_arguments(&[], Some("a"), 10).is_none());
        let traffic = [
            Some(Traffic {
                received: 700,
                sent: 300,
            }),
            None,
        ];
        let arguments = count_arguments(&traffic, Some("a"), 10).unwrap();
        assert_eq!(arguments["total"], 1_000);
        assert_eq!(arguments["boot"], "a");
        assert_eq!(arguments["at"], 10);
        let unread = count_arguments(&[None], None, 10).unwrap();
        assert!(unread["total"].is_null(), "a modem read without traffic");
    }
}

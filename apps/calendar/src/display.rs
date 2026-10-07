//! How a person shows the calendars of their accounts: which are hidden,
//! the colour each is drawn in, which accounts are folded in the sidebar.
//! These are display choices of one person, not calendar content, so they
//! are Personal settings that follow the person to every device and are
//! never seen by collaborators.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ui::storage::{Scope, SettingsSchema};

use crate::account::AccountStore;

/// The person's display choices for their calendars, by calendar and
/// account id. A calendar or account they made no choice about shows as its
/// account says.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarDisplay {
    /// Whether a calendar's events are shown.
    #[serde(default)]
    pub visible: BTreeMap<String, bool>,
    /// The palette slot a calendar is drawn in.
    #[serde(default)]
    pub colors: BTreeMap<String, u8>,
    /// Whether an account's calendars are folded in the sidebar.
    #[serde(default)]
    pub collapsed: BTreeMap<String, bool>,
}

impl SettingsSchema for CalendarDisplay {
    const SCOPE: Scope = Scope::Personal;
}

impl CalendarDisplay {
    /// `accounts` as this person shows them.
    #[must_use]
    pub fn applied_to(&self, accounts: &AccountStore) -> AccountStore {
        let mut shown = accounts.clone();
        for account in &mut shown.accounts {
            if let Some(collapsed) = self.collapsed.get(&account.id) {
                account.collapsed = *collapsed;
            }
            for calendar in &mut account.calendars {
                if let Some(visible) = self.visible.get(&calendar.id) {
                    calendar.visible = *visible;
                }
                if let Some(color) = self.colors.get(&calendar.id) {
                    calendar.color = *color;
                }
            }
        }
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_persons_choices_override_what_the_accounts_say() {
        let accounts = AccountStore::sample();
        let calendar = accounts
            .all_calendars()
            .next()
            .map(|(_, calendar)| calendar.id.clone())
            .expect("a sample calendar");
        let account = accounts.accounts[0].id.clone();
        let display = CalendarDisplay {
            visible: BTreeMap::from([(calendar.clone(), false)]),
            colors: BTreeMap::from([(calendar.clone(), 3)]),
            collapsed: BTreeMap::from([(account, true)]),
        };
        let shown = display.applied_to(&accounts);
        let (_, shown_calendar) = shown
            .all_calendars()
            .find(|(_, item)| item.id == calendar)
            .expect("still listed");
        assert!(!shown_calendar.visible);
        assert_eq!(shown_calendar.color, 3);
        assert!(shown.accounts[0].collapsed);
        assert_eq!(
            CalendarDisplay::default().applied_to(&accounts),
            accounts,
            "without choices the accounts show as configured"
        );
    }
}

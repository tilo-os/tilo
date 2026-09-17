//! Saved alarms, timers and world clocks as a bounded system-search source.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use tilo_actions::{ActionAddress, Resource};
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};

use crate::{ClockState, actions::ClockOperation};

pub const CLOCKS_KIND: &str = "Clocks";

#[derive(Clone)]
pub struct ClocksSearchProvider {
    catalog: SearchCatalog,
    resources: Arc<RwLock<BTreeMap<String, Resource>>>,
}

impl ClocksSearchProvider {
    pub fn new(state: &ClockState) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new(CLOCKS_KIND),
            resources: Arc::new(RwLock::new(BTreeMap::new())),
        };
        provider.refresh(state);
        provider
    }

    pub fn refresh(&self, state: &ClockState) {
        let mut next = BTreeMap::new();
        for clock in &state.world_clocks {
            self.insert(
                &mut next,
                "world_clock",
                clock.id,
                &clock.city,
                format!("World clock · {}", clock.timezone),
                format!("{}\0{}", clock.city, clock.timezone),
                Icon::Globe,
                ClockOperation::OpenWorldClock { id: clock.id },
            );
        }
        for alarm in &state.alarms {
            let time = alarm.local_time.format("%H:%M").to_string();
            self.insert(
                &mut next,
                "alarm",
                alarm.id,
                &alarm.label,
                format!(
                    "Alarm · {time} · {}",
                    if alarm.enabled { "On" } else { "Off" }
                ),
                format!("{}\0{time}", alarm.label),
                Icon::Bell,
                ClockOperation::OpenAlarm { id: alarm.id },
            );
        }
        for timer in &state.timers {
            self.insert(
                &mut next,
                "timer",
                timer.id,
                &timer.label,
                format!("Timer · {}", crate::app::duration(timer.duration_ms)),
                format!(
                    "{}\0{}",
                    timer.label,
                    crate::app::duration(timer.duration_ms)
                ),
                Icon::Timer,
                ClockOperation::OpenTimer { id: timer.id },
            );
        }
        let old: Vec<_> = self
            .resources
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .cloned()
            .collect();
        for id in old {
            if !next.contains_key(&id) {
                self.catalog.remove(&id);
            }
        }
        *self
            .resources
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = next;
    }

    #[allow(clippy::too_many_arguments)]
    fn insert(
        &self,
        resources: &mut BTreeMap<String, Resource>,
        kind: &str,
        id: uuid::Uuid,
        title: &str,
        subtitle: String,
        searchable: String,
        icon: Icon,
        operation: ClockOperation,
    ) {
        let resource = crate::actions::item_resource(kind, id, title, operation);
        let resource_id = resource.id.0.clone();
        let result =
            SearchResult::for_action(CLOCKS_KIND, title, crate::actions::item_address(kind, id))
                .with_subtitle(subtitle)
                .with_icon(icon);
        let _ = self.catalog.upsert(&resource_id, searchable, result);
        resources.insert(resource_id, resource);
    }
}

impl SearchProvider for ClocksSearchProvider {
    fn name(&self) -> &str {
        CLOCKS_KIND
    }
    fn action_directory_generation(&self) -> Option<u64> {
        Some(self.catalog.stats().generation)
    }
    fn action_directory_cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        self.catalog.cursor_is_current(cursor)
    }
    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }
    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.catalog.search_limited(query, limit)
    }
    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        self.catalog
            .query_page(query, cursor, work_budget, result_limit)
    }
    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<Resource> {
        self.resources
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&address.resource)
            .filter(|resource| {
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == address.action)
            })
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveTime;
    use std::collections::BTreeSet;
    use std::time::Duration;

    #[test]
    fn empty_state_has_no_hits() {
        assert!(
            ClocksSearchProvider::new(&ClockState::default())
                .search("")
                .is_empty()
        );
    }

    #[test]
    fn all_durable_types_page_with_canonical_typed_resources() {
        let mut state = ClockState::default();
        state.world_clocks.push(crate::WorldClock {
            id: uuid::Uuid::new_v4(),
            city: "Tokyo".into(),
            timezone: "Asia/Tokyo".into(),
        });
        state.alarms.push(crate::Alarm {
            id: uuid::Uuid::new_v4(),
            label: "Wake up".into(),
            local_time: NaiveTime::from_hms_opt(7, 30, 0).unwrap(),
            weekdays: BTreeSet::new(),
            enabled: true,
            last_fired_local_date: None,
        });
        state
            .timers
            .push(crate::Timer::new("Tea", Duration::from_secs(300)));
        let provider = ClocksSearchProvider::new(&state);
        let first = provider.search_page("", None, 2, 2);
        assert_eq!(first.results.len(), 2);
        assert!(first.inspected <= 2);
        for hit in &first.results {
            let address = hit.action_address().expect("canonical address");
            let resource = provider
                .resolve_action_resource(&address)
                .expect("typed resource");
            assert!(resource.kind.starts_with("clocks."));
            assert_eq!(address.action, "open");
            let encoded = tilo_actions::door::encode(&[resource], &address, None)
                .expect("search activation uses the exported operation");
            assert!(
                crate::actions::decode(&encoded.type_id, &encoded.payload)
                    .expect("valid operation")
                    .is_some()
            );
        }
        assert_eq!(
            provider
                .search_page("", first.next.as_ref(), 2, 2)
                .results
                .len(),
            1
        );
    }

    #[test]
    fn refresh_is_visible_to_existing_clones() {
        let provider = ClocksSearchProvider::new(&ClockState::default());
        let clone = provider.clone();
        let mut state = ClockState::default();
        state
            .timers
            .push(crate::Timer::new("Laundry", Duration::from_secs(900)));
        provider.refresh(&state);
        assert_eq!(clone.search("laundry").len(), 1);
        provider.refresh(&ClockState::default());
        assert!(clone.search("").is_empty());
    }
}

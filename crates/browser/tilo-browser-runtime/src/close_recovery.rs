//! Retain actual component recovery owners until retries and durable writes settle.
use std::collections::HashMap;
use ui::{
    application::ComponentCloseReport,
    portable_storage::{PendingWrite, PortableStorage},
};

#[derive(Default)]
pub(crate) struct ClosedComponents(HashMap<u64, ClosedComponent>);
struct ClosedComponent {
    reports: Vec<ComponentCloseReport>,
    storage: PortableStorage,
}
impl ClosedComponent {
    fn retry(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        for report in &mut self.reports {
            let _ = report.retry_view();
            let _ = report.retry_record();
            errors.extend(
                [
                    &report.view_error,
                    &report.record_error,
                    &report.teardown_error,
                ]
                .into_iter()
                .filter_map(|error| error.clone()),
            );
        }
        errors
    }
}
impl ClosedComponents {
    pub fn retain(
        &mut self,
        id: u64,
        reports: Vec<ComponentCloseReport>,
        storage: PortableStorage,
    ) {
        self.0.insert(id, ClosedComponent { reports, storage });
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn prepare(&mut self, id: u64) -> (Vec<PendingWrite>, Vec<String>) {
        let Some(closed) = self.0.get_mut(&id) else {
            return (vec![], vec![]);
        };
        let errors = closed.retry();
        (closed.storage.pending_writes(), errors)
    }
    pub fn finish(&mut self, id: u64) -> Result<(), String> {
        let Some(closed) = self.0.get_mut(&id) else {
            return Ok(());
        };
        let mut errors = closed.retry();
        if !closed.storage.pending_writes().is_empty() {
            errors.push("component persistence is not durable".into());
        }
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        // Dropping also drops SavedInstanceView and all retained retry owners.
        self.0.remove(&id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        rc::{Rc, Weak},
    };
    use ui::{
        application::{HostContext, LaunchTarget, RestoreInput},
        application_view::InstanceViewStore,
        portable_storage::PortableStorage,
        record::RecordOwners,
    };

    fn closed_component(
        instance: u64,
    ) -> (
        ui::application::ComponentCloseReport,
        PortableStorage,
        Weak<InstanceViewStore>,
    ) {
        let cache = PortableStorage::new("close-test", "device").unwrap();
        let definition = word_processor::definition();
        let records = RecordOwners::portable(cache.clone())
            .open(definition.app_id, "room")
            .unwrap();
        let views = Rc::new(InstanceViewStore::portable(cache.clone(), "room").unwrap());
        let weak = Rc::downgrade(&views);
        let permissions = Rc::new(RefCell::new(
            ui::permissions::PermissionContext::from_capabilities(
                ui::permissions::Actor::person(Some(ui::permissions::Role::Owner), "author"),
                vec![],
            ),
        ));
        let context = HostContext::prepare(
            definition,
            records,
            views,
            "device".into(),
            instance,
            permissions,
            ui::permissions::UserId("author".into()),
            tilo_actions::ActionContext::default(),
            &LaunchTarget::New,
        )
        .unwrap();
        let mut component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let report = component.close();
        drop(component);
        (report, cache, weak)
    }
    #[test]
    fn actual_component_close_retries_both_owners_and_releases_only_after_commit() {
        let (report, cache, weak) = closed_component(1);
        assert!(report.view_retry.is_some() && report.record_retry.is_some());
        let mut closed = ClosedComponents::default();
        closed.retain(1, vec![report], cache.clone());
        assert!(closed.finish(1).is_err());
        assert_eq!(closed.len(), 1);
        assert!(weak.upgrade().is_some());
        let (writes, errors) = closed.prepare(1);
        assert!(!errors.is_empty());
        assert!(
            writes
                .iter()
                .any(|w| matches!(w.key, ui::portable_storage::StorageKey::View { .. }))
        );
        for write in &writes {
            cache
                .acknowledge(write, Err("transaction aborted".into()))
                .unwrap();
        }
        assert!(closed.finish(1).is_err());
        assert_eq!(closed.len(), 1);
        for write in cache.pending_writes() {
            cache.acknowledge(&write, Ok(())).unwrap();
        }
        closed.finish(1).unwrap();
        assert_eq!(closed.len(), 0);
        assert!(weak.upgrade().is_none());
    }
    #[test]
    fn repeated_clean_component_closes_drop_captured_envelopes_and_owners() {
        let mut closed = ClosedComponents::default();
        for id in 1..=12 {
            let (report, cache, weak) = closed_component(id);
            closed.retain(id, vec![report], cache.clone());
            for write in cache.pending_writes() {
                cache.acknowledge(&write, Ok(())).unwrap();
            }
            closed.finish(id).unwrap();
            assert_eq!(closed.len(), 0);
            assert!(weak.upgrade().is_none());
        }
    }
    #[test]
    fn actual_view_store_failure_retries_without_any_idb_abort() {
        let root = tempfile::tempdir().unwrap();
        let definition = word_processor::definition();
        let records = RecordOwners::new(root.path().into())
            .open(definition.app_id, "room")
            .unwrap();
        let views = Rc::new(InstanceViewStore::new(root.path().into(), "device", "room").unwrap());
        let weak = Rc::downgrade(&views);
        let permissions = Rc::new(RefCell::new(
            ui::permissions::PermissionContext::from_capabilities(
                ui::permissions::Actor::person(Some(ui::permissions::Role::Owner), "author"),
                vec![],
            ),
        ));
        let context = HostContext::prepare(
            definition,
            records,
            views,
            "device".into(),
            1,
            permissions,
            ui::permissions::UserId("author".into()),
            tilo_actions::ActionContext::default(),
            &LaunchTarget::New,
        )
        .unwrap();
        let mut component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let blocker = root.path().join("instance-views");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let report = component.close();
        drop(component);
        assert!(report.view_error.is_some() && report.view_retry.is_some());
        let cache = PortableStorage::new("no-idb-writes", "device").unwrap();
        let mut closed = ClosedComponents::default();
        closed.retain(1, vec![report], cache);
        assert!(closed.prepare(1).0.is_empty());
        assert!(closed.finish(1).is_err());
        assert!(weak.upgrade().is_some());
        std::fs::remove_file(blocker).unwrap();
        closed.finish(1).unwrap();
        assert_eq!(closed.len(), 0);
        assert!(weak.upgrade().is_none());
        let restored = InstanceViewStore::new(root.path().into(), "device", "room")
            .unwrap()
            .load(1)
            .unwrap();
        assert_eq!(restored.unwrap().app_id, definition.app_id);
    }

    #[test]
    fn unresolved_component_error_cannot_release_even_with_no_pending_idb_bytes() {
        let (mut report, cache, weak) = closed_component(1);
        report.teardown_error = Some("component teardown unavailable".into());
        let mut closed = ClosedComponents::default();
        closed.retain(1, vec![report], cache.clone());
        for write in cache.pending_writes() {
            cache.acknowledge(&write, Ok(())).unwrap();
        }
        assert!(
            closed
                .finish(1)
                .unwrap_err()
                .contains("teardown unavailable")
        );
        assert_eq!(closed.len(), 1);
        // Retry owners may release after success, but the unresolved report/envelope remains.
        assert!(weak.upgrade().is_none());
    }
}

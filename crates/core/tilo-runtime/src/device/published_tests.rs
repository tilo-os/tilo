//! Two providers behind the daemon's one table: each answers the resources it keeps, a reading
//! is watched and changed where it stands, and a resource neither keeps is refused.

use std::sync::Arc;

use super::*;

/// Two resources the first provider keeps, and one the second does.
const FIRST: [&str; 2] = ["device:first", "device:first-other"];
const SECOND: [&str; 1] = ["device:second"];

/// A provider that keeps `resources`: each reads as its own name, and `set` changes it.
#[derive(Default)]
struct Keeping {
    /// The readings, by resource.
    readings: Mutex<HashMap<String, String>>,
}

impl Keeping {
    fn holding(resources: &[&str]) -> Arc<Self> {
        let keeping = Self::default();
        for resource in resources {
            keeping.readings.lock_recover().insert(
                (*resource).to_owned(),
                format!("{{\"name\":\"{resource}\"}}"),
            );
        }
        Arc::new(keeping)
    }
}

impl PublishedResources for Keeping {
    fn snapshot(
        &self,
        _caller: &Caller,
        resource: &str,
    ) -> oneshot::Receiver<Result<Reading, String>> {
        let (answer, answered) = oneshot::channel();
        let reading = self
            .readings
            .lock_recover()
            .get(resource)
            .map(|reading| reading.as_bytes().to_vec());
        let _ = answer.send(Ok((1, reading)));
        answered
    }

    fn watch(
        &self,
        caller: &Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<ResourceWatch, String> {
        if !self.readings.lock_recover().contains_key(resource) {
            return Err(format!("not keeping `{resource}`"));
        }
        let (revision, reading) = self.snapshot(caller, resource).blocking_recv().unwrap()?;
        let _ = sink.send(runtime_collab::wire::Event::Reading {
            resource: resource.to_owned(),
            revision,
            reading,
        });
        Ok(ResourceWatch::new(7))
    }

    fn unwatch(&self, _watch: ResourceWatch) {}

    fn invoke(
        &self,
        _caller: &Caller,
        resource: &str,
        action: &str,
        arguments: Vec<u8>,
    ) -> oneshot::Receiver<runtime_collab::wire::Receipt> {
        let (answer, answered) = oneshot::channel();
        let receipt = if action != "set" {
            runtime_collab::wire::Receipt::Refused {
                code: "unknown_action".to_owned(),
                message: format!("`{resource}` has no `{action}`"),
            }
        } else {
            let value = String::from_utf8(arguments).unwrap();
            if let Some(reading) = self.readings.lock_recover().get_mut(resource) {
                *reading = value;
                runtime_collab::wire::Receipt::Done(Vec::new())
            } else {
                runtime_collab::wire::Receipt::Refused {
                    code: "unknown_resource".to_owned(),
                    message: format!("not keeping `{resource}`"),
                }
            }
        };
        let _ = answer.send(receipt);
        answered
    }
}

fn caller() -> Caller {
    Caller {
        app_id: "so.tilo.testing".to_owned(),
        workspace: "home".to_owned(),
    }
}

fn reading(published: &Published, resource: &str) -> Option<String> {
    let (_, reading) = published
        .snapshot(&caller(), resource)
        .blocking_recv()
        .unwrap()
        .unwrap();
    reading.map(|bytes| String::from_utf8(bytes).unwrap())
}

#[test]
fn each_provider_answers_the_resources_it_keeps() {
    let first = Keeping::holding(&FIRST);
    let second = Keeping::holding(&SECOND);
    let published = Published::new()
        .serving(&FIRST, first)
        .serving(&SECOND, second);
    assert_eq!(
        reading(&published, "device:first").as_deref(),
        Some(r#"{"name":"device:first"}"#)
    );
    assert_eq!(
        reading(&published, "device:first-other").as_deref(),
        Some(r#"{"name":"device:first-other"}"#)
    );
    assert_eq!(
        reading(&published, "device:second").as_deref(),
        Some(r#"{"name":"device:second"}"#)
    );
    assert_eq!(reading(&published, "device:nothing"), None);
}

#[test]
fn a_prefix_answers_the_resources_no_fixed_list_names() {
    // A vault item's code is published under `vault.item:<id>`, a name no fixed list can hold:
    // its provider answers the prefix, ahead of the one that answers every resource no other
    // names, and a fixed list still wins for the resources it does name.
    const ITEMS: [&str; 2] = ["vault.item:one", "vault.item:two"];
    const LISTED: [&str; 1] = ["device:first"];
    let listed = Keeping::holding(&LISTED);
    let items = Keeping::holding(&ITEMS);
    let rest = Keeping::holding(&["device:rest"]);
    let published = Published::new()
        .serving(&LISTED, listed)
        .serving_prefix("vault.item:", items)
        .serving_the_rest(rest);
    assert_eq!(
        reading(&published, "device:first").as_deref(),
        Some(r#"{"name":"device:first"}"#)
    );
    assert_eq!(
        reading(&published, "vault.item:two").as_deref(),
        Some(r#"{"name":"vault.item:two"}"#)
    );
    assert_eq!(
        reading(&published, "device:rest").as_deref(),
        Some(r#"{"name":"device:rest"}"#),
        "a resource outside the prefix reaches the provider of the rest"
    );
    assert_eq!(reading(&published, "vault.item:nothing"), None);
}

#[test]
fn a_change_reaches_the_provider_that_keeps_the_resource_and_no_other() {
    let first = Keeping::holding(&FIRST);
    let second = Keeping::holding(&SECOND);
    let published = Published::new()
        .serving(&FIRST, first)
        .serving(&SECOND, second);
    let changed = published
        .invoke(
            &caller(),
            "device:second",
            "set",
            br#"{"name":"changed"}"#.to_vec(),
        )
        .blocking_recv()
        .unwrap();
    assert!(matches!(changed, runtime_collab::wire::Receipt::Done(_)));
    assert_eq!(
        reading(&published, "device:second").as_deref(),
        Some(r#"{"name":"changed"}"#)
    );
    assert_eq!(
        reading(&published, "device:first").as_deref(),
        Some(r#"{"name":"device:first"}"#)
    );
    let unknown = published
        .invoke(&caller(), "device:nothing", "set", b"{}".to_vec())
        .blocking_recv()
        .unwrap();
    assert!(matches!(
        unknown,
        runtime_collab::wire::Receipt::Refused { .. }
    ));
}

#[test]
fn a_watch_is_kept_by_the_provider_and_goes_when_the_table_lets_it() {
    let first = Keeping::holding(&FIRST);
    let second = Keeping::holding(&SECOND);
    let published = Published::new()
        .serving(&FIRST, first)
        .serving(&SECOND, second);
    let (sink, mut readings) = mpsc::unbounded_channel();
    let watch = published.watch(&caller(), "device:first", sink).unwrap();
    let sent = readings.try_recv().unwrap();
    assert!(matches!(
        sent,
        runtime_collab::wire::Event::Reading {
            reading: Some(_),
            ..
        }
    ));
    published.unwatch(watch);
    let refused = published
        .watch(&caller(), "device:nothing", mpsc::unbounded_channel().0)
        .expect_err("no provider keeps it");
    assert!(refused.contains("device:nothing"), "{refused}");
}

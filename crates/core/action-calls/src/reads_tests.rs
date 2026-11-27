use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjType, ROOT};

use tilo_actions::document_sync::{chain_head, document_part};

use tilo_actions::remote::{
    ActionCallError, Caller, HeldCopies, ReadingAnswer, ReadsAnswer, ResourceAddress, ResourceAsk,
    ResourceCopy, ResourceReadings, ResourceReads, ResourceWake,
};

fn message(text: &str) -> Vec<u8> {
    let mut doc = AutoCommit::new();
    let messages = doc.put_object(ROOT, "messages", ObjType::List).unwrap();
    let first = doc.insert_object(&messages, 0, ObjType::Map).unwrap();
    let body = doc.put_object(&first, "text", ObjType::Text).unwrap();
    doc.splice_text(&body, 0, 0, text).unwrap();
    doc.put(&first, "likes", automerge::ScalarValue::counter(2))
        .unwrap();
    doc.put(&first, "avatar", vec![1u8, 2, 3]).unwrap();
    doc.save()
}

fn ask(record: &str, joined: bool) -> ResourceAsk {
    ResourceAsk {
        address: ResourceAddress {
            workspace: "w".into(),
            app: "so.tilo.chat".into(),
            record: record.into(),
        },
        joined,
    }
}

/// [`crate::read_resources`], waited for on a runtime of its own.
fn read_resources(
    reads: &(impl ResourceReads + ?Sized),
    caller: &Caller,
    asks: &[ResourceAsk],
    after: Option<&str>,
    deadline: tilo_clock::time::Instant,
) -> Result<ResourceReadings, ActionCallError> {
    tilo_rt::block_on(crate::read_resources(reads, caller, asks, after, deadline)).unwrap()
}

fn values(readings: &ResourceReadings) -> Vec<(String, serde_json::Value)> {
    readings
        .records
        .iter()
        .map(|(record, copy)| (record.clone(), copy.as_ref().unwrap().readable().unwrap()))
        .collect()
}

tilo_test_env::scenario! {
    fn a_record_reads_as_plain_data;

    phase a_record_reads_as_plain_data_with_its_text_counters_and_bytes;
    {
        let mut held = HeldCopies::default();
        held.0.insert("note".into(), message("hello"));
        let caller = Caller::local_person("me");
        let now = tilo_clock::now();
        let read = read_resources(
            &held,
            &caller,
            &[ask("note", false), ask("gone", false)],
            None,
            now,
        )
        .unwrap();
        assert_eq!(
            values(&read),
            [
                (
                    "note".to_owned(),
                    serde_json::json!({"messages": [
                        {"text": "hello", "likes": 2, "avatar": {"bytes": 3}}
                    ]})
                ),
                ("gone".to_owned(), serde_json::Value::Null),
            ]
        );
    }

    phase a_data_enum_reads_with_its_fields_by_name;
    {
        let mut doc = AutoCommit::new();
        let attached = doc.put_object(ROOT, "attached", ObjType::Map).unwrap();
        doc.put(&attached, "$variant", "file").unwrap();
        doc.put(&attached, "$field/file/blob", "ab").unwrap();
        doc.put(&attached, "$field/file/a%2Fb", 3).unwrap();
        assert_eq!(
            ResourceCopy::Document(doc.save()).readable().unwrap(),
            serde_json::json!({"attached": {"$variant": "file", "blob": "ab", "a/b": 3}})
        );
    }

    phase a_rich_text_reads_as_its_paragraphs_in_their_order;
    {
        // A rich text under a field, as the word processor keeps a document's body: three
        // paragraphs, written in another order than they stand in.
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "title", "A poem").unwrap();
        let body = doc.put_object(ROOT, "body", ObjType::Map).unwrap();
        doc.put(&body, "schema", "tilo.richtext").unwrap();
        let blocks = doc.put_object(&body, "blocks", ObjType::Map).unwrap();
        for (id, place, kind, words) in [
            ("p1", vec![192_u8], "list_item", "and so are you"),
            ("p2", vec![64], "heading", "A poem"),
            ("p3", vec![128], "paragraph", "Roses are red"),
        ] {
            let block = doc.put_object(&blocks, id, ObjType::Map).unwrap();
            doc.put(&block, "order", place).unwrap();
            doc.put(&block, "kind", kind).unwrap();
            let text = doc.put_object(&block, "text", ObjType::Text).unwrap();
            doc.splice_text(&text, 0, 0, words).unwrap();
        }
        assert_eq!(
            ResourceCopy::Document(doc.save()).readable().unwrap(),
            serde_json::json!({"title": "A poem", "body": {
                "schema": "tilo.richtext",
                "blocks": [
                    {"id": "p2", "kind": "heading", "text": "A poem"},
                    {"id": "p3", "kind": "paragraph", "text": "Roses are red"},
                    {"id": "p1", "kind": "list_item", "text": "and so are you"},
                ],
            }})
        );
    }

    phase a_chain_reads_joined_up_to_its_last_part_and_a_new_part_changes_its_revision;
    {
        let mut held = HeldCopies::default();
        let part = |index| document_part("chain", index);
        held.0.insert(part(0), message("first"));
        held.0.insert(part(1), message("second"));
        let caller = Caller::local_person("me");
        let now = tilo_clock::now();
        let read = read_resources(&held, &caller, &[ask("chain", true)], None, now).unwrap();
        let texts: Vec<_> = values(&read)
            .into_iter()
            .map(|(record, value)| (record, value["messages"][0]["text"].clone()))
            .collect();
        assert_eq!(
            texts,
            [
                (part(0), serde_json::json!("first")),
                (part(1), serde_json::json!("second")),
            ]
        );
        let same = read_resources(&held, &caller, &[ask("chain", true)], None, now).unwrap();
        assert_eq!(same.revision, read.revision);
        held.0.insert(part(2), message("third"));
        let grown = read_resources(
            &held,
            &caller,
            &[ask("chain", true)],
            Some(&read.revision),
            now,
        )
        .unwrap();
        assert_ne!(grown.revision, read.revision);
        assert_eq!(grown.records.len(), 3);
        // The head is the newest part alone, its revision moved by a new part.
        let newest_part = chain_head("chain");
        let newest = read_resources(&held, &caller, &[ask(&newest_part, false)], None, now).unwrap();
        assert_eq!(
            values(&newest),
            [(
                part(2),
                serde_json::json!({"messages": [
                    {"text": "third", "likes": 2, "avatar": {"bytes": 3}}
                ]})
            )]
        );
        held.0.insert(part(3), message("fourth"));
        let moved = read_resources(&held, &caller, &[ask(&newest_part, false)], None, now).unwrap();
        assert_ne!(moved.revision, newest.revision);
        assert_eq!(moved.records[0].0, part(3));
    }

    phase a_reading_reads_as_its_json_and_a_change_wakes_the_follow;
    {
        /// A resource a provider publishes itself: its JSON, the revision it names, and the
        /// follows watching it, woken when it moves.
        struct Published {
            value: std::sync::Mutex<serde_json::Value>,
            revision: std::sync::Mutex<String>,
            wakers: std::sync::Mutex<Vec<ResourceWake>>,
        }

        impl Published {
            fn move_to(&self, value: serde_json::Value, revision: &str) {
                *self.value.lock().unwrap() = value;
                *self.revision.lock().unwrap() = revision.to_owned();
                let woken = std::mem::take(&mut *self.wakers.lock().unwrap());
                for wake in woken {
                    wake.wake();
                }
            }
        }

        impl ResourceReads for Published {
            fn answer_read(
                self: std::sync::Arc<Self>,
                caller: &Caller,
                correlation: u64,
                read: tilo_actions::remote::ResourceReadRequest,
            ) -> ReadingAnswer
            where
                Self: 'static,
            {
                Box::pin(crate::answer_read(self, caller.clone(), correlation, read))
            }

            fn read<'a>(
                &'a self,
                _: &'a Caller,
                _: &'a ResourceAddress,
            ) -> ReadsAnswer<'a, ResourceCopy> {
                Box::pin(async move {
                Ok(ResourceCopy::reading(
                    self.value.lock().unwrap().clone(),
                    Some(self.revision.lock().unwrap().clone()),
                ))
                })
            }

            fn watch<'a>(
                &'a self,
                _: &'a Caller,
                _: &'a ResourceAddress,
                wake: &'a ResourceWake,
            ) -> ReadsAnswer<'a, ()> {
                Box::pin(async move {
                self.wakers.lock().unwrap().push(wake.clone());
                Ok(())
                })
            }
        }

        let published = std::sync::Arc::new(Published {
            value: std::sync::Mutex::new(serde_json::json!({"playing": false})),
            revision: std::sync::Mutex::new("a".into()),
            wakers: std::sync::Mutex::default(),
        });
        let caller = Caller::local_person("me");
        let asks = [ResourceAsk {
            address: ResourceAddress {
                workspace: "w".into(),
                app: "shell".into(),
                record: "session:media".into(),
            },
            joined: false,
        }];
        let now = tilo_clock::now();
        let read = read_resources(&*published, &caller, &asks, None, now).unwrap();
        assert_eq!(
            values(&read),
            [(
                "session:media".to_owned(),
                serde_json::json!({"playing": false})
            )],
            "a reading reads as the JSON its provider publishes"
        );
        assert!(!read.revision.is_empty(), "the reading has a revision");
        // The first read watched too; only what the follow watches is woken here.
        published.wakers.lock().unwrap().clear();
        let changed = std::thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                read_resources(
                    &*published,
                    &caller,
                    &asks,
                    Some(&read.revision),
                    tilo_clock::now()
                        .checked_add(std::time::Duration::from_secs(30))
                        .unwrap(),
                )
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                if published.wakers.lock().unwrap().is_empty() {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the follow watched nothing"
                    );
                    std::thread::yield_now();
                    continue;
                }
                break;
            }
            published.move_to(serde_json::json!({"playing": true}), "b");
            waiting.join().expect("the follow")
        })
        .unwrap();
        assert_eq!(
            values(&changed),
            [(
                "session:media".to_owned(),
                serde_json::json!({"playing": true})
            )],
            "the change woke the follow, which read the new reading"
        );
        assert_ne!(changed.revision, read.revision);
    }

    phase a_reading_its_provider_names_no_revision_for_is_hashed_by_its_json;
    {
        struct Unversioned(std::sync::Mutex<serde_json::Value>);

        impl ResourceReads for Unversioned {
            fn answer_read(
                self: std::sync::Arc<Self>,
                caller: &Caller,
                correlation: u64,
                read: tilo_actions::remote::ResourceReadRequest,
            ) -> ReadingAnswer
            where
                Self: 'static,
            {
                Box::pin(crate::answer_read(self, caller.clone(), correlation, read))
            }

            fn read<'a>(
                &'a self,
                _: &'a Caller,
                _: &'a ResourceAddress,
            ) -> ReadsAnswer<'a, ResourceCopy> {
                Box::pin(async move {
                Ok(ResourceCopy::reading(
                    self.0.lock().unwrap().clone(),
                    None,
                ))
                })
            }

            fn watch<'a>(
                &'a self,
                _: &'a Caller,
                _: &'a ResourceAddress,
                _: &'a ResourceWake,
            ) -> ReadsAnswer<'a, ()> {
                Box::pin(async move {
                Ok(())
                })
            }
        }

        let published = Unversioned(std::sync::Mutex::new(serde_json::json!({"level": 1})));
        let caller = Caller::local_person("me");
        let asks = [ask("session:volume", false)];
        let now = tilo_clock::now();
        let first = read_resources(&published, &caller, &asks, None, now).unwrap();
        let again = read_resources(&published, &caller, &asks, None, now).unwrap();
        assert_eq!(first.revision, again.revision, "the same JSON is one revision");
        *published.0.lock().unwrap() = serde_json::json!({"level": 2});
        let moved = read_resources(&published, &caller, &asks, None, now).unwrap();
        assert_ne!(moved.revision, first.revision, "another JSON is another");
    }
}

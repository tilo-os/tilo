//! `Bytes` as a record's field, end to end: a host engine writes a run's output, small and
//! large, into a record; the bytes go inside the document when they are small and to the
//! device's content store when they are large; the record reads them back as the bytes they
//! are, on this device and on another device the document synced to; and an action of the app writes
//! them too.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::meta::FieldKind;
    use tilolang_runtime::origin::OriginWrite;
    use tilolang_runtime::store::{BytesData, FieldSpec, Store as _};
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, SimpleAuthority, Value};

    const SOURCE: &str = r#"app so.tilo.bytes "Bytes"

type Run {
    output: Bytes
}

action hold(r: Run, text: Text) {
    r.output = Bytes.from_base64(text)
}

action clear(r: Run) {
    r.output = Bytes.from_base64("")
}

view show(r: Run) {
    text("It printed {r.output.len()} bytes")
}
"#;

    /// This device's content store, in memory: what a large value is kept in, by its hash.
    #[derive(Clone, Default)]
    struct MemoryBlobs(Rc<RefCell<HashMap<String, Vec<u8>>>>);

    impl BlobHost for MemoryBlobs {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(hash).cloned()
        }

        fn choose(&self, _: &FileRequest) -> Result<Vec<FileValue>, String> {
            Err("no chooser here".to_owned())
        }

        fn save(&self, _: &SaveRequest, _: &[u8]) -> Result<(), String> {
            Err("no chooser here".to_owned())
        }

        fn keep(&self, _: &str, _: &str, bytes: &[u8]) -> Result<FileValue, String> {
            let hash = blake3::hash(bytes).to_hex().to_string();
            self.0.borrow_mut().insert(hash.clone(), bytes.to_vec());
            Ok(FileValue::new(
                "bytes",
                "application/octet-stream",
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            ))
        }
    }

    /// A runtime over `store`, the app loaded, and the same content store as every other.
    fn runtime(store: &AmStore, blobs: &MemoryBlobs) -> (Runtime, App) {
        let rt = Runtime::new(store.clone(), SimpleAuthority::new()).unwrap();
        let app = App::compile(SOURCE, &[]);
        app.load(&rt);
        rt.set_blobs(blobs.clone());
        (rt, app)
    }

    /// The id of the record's `output` field, as the app declares it.
    fn output_field(app: &App) -> String {
        app.meta()
            .types
            .iter()
            .find(|ty| ty.name == "Run")
            .and_then(|ty| ty.fields.iter().find(|field| field.name == "output"))
            .expect("the Run's output field")
            .id
            .clone()
    }

    /// What the store keeps under the record's `output`, without reading it as a value.
    fn stored(store: &AmStore, run: &ResourceRef, app: &App) -> BytesData {
        match store
            .read(
                run,
                &FieldSpec {
                    id: output_field(app),
                    kind: FieldKind::Bytes,
                },
            )
            .expect("the field reads")
        {
            tilolang_runtime::store::FieldValue::Bytes(data) => data,
            other => panic!("the field holds {other:?}"),
        }
    }

    /// The host engine writes `output` into `run`, as the engine that runs a command appends what
    /// it printed.
    fn engine_writes(rt: &Runtime, run: &ResourceRef, output: Vec<u8>) {
        rt.write_origin("cmd-engine", None, |change| {
            change.write(vec![OriginWrite::Set {
                target: run.clone(),
                fields: vec![("output".to_owned(), Value::Bytes(Rc::new(output)))],
            }])
        })
        .expect("the engine's write");
    }

    tilo_test_env::scenario! {
        /// A run's output is bytes the record keeps, small beside it and large by reference.
        fn a_runs_output_is_bytes_a_record_keeps;

        phase a_small_output_lives_inside_the_record;
        {
            let blobs = MemoryBlobs::default();
            let store = AmStore::new();
            let (rt, app) = runtime(&store, &blobs);
            let run = rt.create("Run").unwrap();

            let printed = b"primes below 20: 2 3 5 7 11 13 17 19\n".to_vec();
            engine_writes(&rt, &run, printed.clone());

            assert_eq!(
                rt.read(&run, "output").unwrap(),
                Value::Bytes(Rc::new(printed.clone())),
                "the bytes read back as they were written"
            );
            assert!(
                matches!(stored(&store, &run, &app), BytesData::Inline(data) if *data == printed),
                "a small value stays inside the record"
            );
        }

        phase a_large_output_is_kept_by_the_content_store;
        {
            let blobs = MemoryBlobs::default();
            let store = AmStore::new();
            let (rt, app) = runtime(&store, &blobs);
            let run = rt.create("Run").unwrap();

            let printed: Vec<u8> = (0..5000u32).map(|i| u8::try_from(i % 251).unwrap()).collect();
            engine_writes(&rt, &run, printed.clone());

            assert_eq!(
                rt.read(&run, "output").unwrap(),
                Value::Bytes(Rc::new(printed.clone()))
            );
            match stored(&store, &run, &app) {
                BytesData::Kept { hash, len } => {
                    assert_eq!(len, u64::try_from(printed.len()).unwrap());
                    assert_eq!(
                        blobs.read(&hash).as_deref(),
                        Some(printed.as_slice()),
                        "the bytes are in the content store, not the document"
                    );
                }
                BytesData::Inline(_) => panic!("a large value stays inside the record"),
            }
        }

        phase the_reference_reaches_another_device_and_reads_the_bytes;
        {
            let blobs = MemoryBlobs::default();
            let (mut a, mut b) = (AmStore::new(), AmStore::new());
            let (ra, app) = runtime(&a, &blobs);
            let (rb, _) = runtime(&b, &blobs);
            let run = ra.create("Run").unwrap();
            a.sync_with(&mut b).unwrap();

            let printed: Vec<u8> = (0..4096u32).map(|i| u8::try_from(i % 253).unwrap()).collect();
            engine_writes(&ra, &run, printed.clone());
            assert!(
                rb.read(&run, "output").is_err(),
                "the record has not reached the other device yet"
            );

            a.sync_with(&mut b).unwrap();
            assert_eq!(
                rb.read(&run, "output").unwrap(),
                Value::Bytes(Rc::new(printed.clone())),
                "the other device reads the bytes the reference names, from its content store"
            );
            // The document carries the address; the bytes reach the other device through the
            // content store, not through the change.
            assert!(matches!(stored(&a, &run, &app), BytesData::Kept { .. }));
        }

        phase an_action_writes_bytes_and_a_view_counts_them;
        {
            let blobs = MemoryBlobs::default();
            let store = AmStore::new();
            let (rt, app) = runtime(&store, &blobs);
            let run = rt.create("Run").unwrap();

            // `hello`, as the base64 text an action reads it from.
            let out = rt
                .invoke_action(
                    &me(),
                    (app.action("hold").as_str(), vec![Value::resource(&run), Value::text("aGVsbG8=")]),
                )
                .unwrap();
            assert_eq!(out, tilolang_runtime::InvokeOutcome::Done(Value::Unit));
            assert_eq!(
                rt.read(&run, "output").unwrap(),
                Value::Bytes(Rc::new(b"hello".to_vec()))
            );

            let mut view = rt
                .open_view(&me(), &app.view("show"), vec![Value::resource(&run)])
                .expect("the view opens");
            assert!(
                view.tree().find("txt").is_some(),
                "the view reads the bytes' length: {:?}",
                view.tree()
            );

            rt.invoke_action(&me(), (app.action("clear").as_str(), vec![Value::resource(&run)]))
                .unwrap();
            assert_eq!(
                rt.read(&run, "output").unwrap(),
                Value::Bytes(Rc::new(Vec::new())),
                "no bytes read as bytes of nothing"
            );
        }
    }
}

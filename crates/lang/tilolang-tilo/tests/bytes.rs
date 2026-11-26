//! `Bytes` in the platform's store end to end: a host engine writes a run's output, small and
//! large, into a record; a small value stays inside the record's content, a large one is kept by
//! the device's content store and the record holds its address; and the runtime reads the bytes
//! back from either. An action of the app writes them too.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
use tilolang_runtime::meta::FieldKind;
use tilolang_runtime::origin::OriginWrite;
use tilolang_runtime::store::{BytesData, FieldSpec, Store as _};
use tilolang_runtime::value::Value;
use tilolang_runtime::{Actor, InvokeOutcome, ResourceRef};
use tilolang_tilo::host::Host;
use tilolang_tilo::runtime::ids::MemberId;

const SOURCE: &str = r#"app test.bytes "Bytes"

type Board {
    runs: Collection<Run>
}

type Run {
    output: Bytes
}

root Board

default view board for Board

view board(b: Board) {
    for r in b.runs {
        text("It printed {r.output.len()} bytes")
    }
}

action add(b: Board) -> Run {
    b.runs.insert(Run())
}

action hold(r: Run, text: Text) {
    r.output = Bytes.from_base64(text)
}
"#;

fn me() -> Actor {
    Actor::from(MemberId::person("ada"))
}

/// This device's content store, in memory: what a large value is kept in, by its address.
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
        let mut held = self.0.borrow_mut();
        let hash = format!("{:064x}", held.len().saturating_add(1));
        held.insert(hash.clone(), bytes.to_vec());
        Ok(FileValue::new(
            "bytes",
            "application/octet-stream",
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            hash,
        ))
    }
}

/// A host with the app loaded, and the bytes of its records in `blobs`.
fn host(blobs: &MemoryBlobs) -> (Host, tilolang_tilo::host::Package) {
    let host = Host::new(me(), || None).expect("a host");
    let package = host
        .load_builtin(SOURCE, "")
        .unwrap_or_else(|error| panic!("{error}"));
    host.runtime().set_blobs(blobs.clone());
    (host, package)
}

/// The id of `name` in `package`'s actions.
fn action_id(package: &tilolang_tilo::host::Package, name: &str) -> String {
    package
        .meta
        .actions
        .iter()
        .find(|action| action.name == name)
        .unwrap_or_else(|| panic!("no action {name}"))
        .id
        .clone()
}

/// The id of the `Run`'s `output` field.
fn output_field(package: &tilolang_tilo::host::Package) -> String {
    package
        .meta
        .types
        .iter()
        .find(|ty| ty.name == "Run")
        .and_then(|ty| ty.fields.iter().find(|field| field.name == "output"))
        .expect("the Run's output field")
        .id
        .clone()
}

/// The id of the `Board` type, as the app declares it: made by its id, since a name more than
/// one loaded package may declare is ambiguous.
fn board_type(package: &tilolang_tilo::host::Package) -> String {
    package
        .meta
        .types
        .iter()
        .find(|ty| ty.name == "Board")
        .expect("the Board type")
        .id
        .clone()
}

/// A run the app made, of the board it made.
fn made(host: &Host, package: &tilolang_tilo::host::Package) -> ResourceRef {
    let board = host
        .runtime()
        .create_root(&board_type(package))
        .expect("the app's root");
    match host
        .runtime()
        .invoke_action(
            &me(),
            (
                action_id(package, "add").as_str(),
                vec![Value::resource(&board)],
            ),
        )
        .expect("the action runs")
    {
        InvokeOutcome::Done(Value::Ref(run)) => (*run.target).clone(),
        other => panic!("the action makes a run: {other:?}"),
    }
}

/// What the store keeps under the run's `output`, without reading it as a value.
fn stored(host: &Host, run: &ResourceRef, package: &tilolang_tilo::host::Package) -> BytesData {
    match host
        .store()
        .read(
            run,
            &FieldSpec {
                id: output_field(package),
                kind: FieldKind::Bytes,
            },
        )
        .expect("the field reads")
    {
        tilolang_runtime::store::FieldValue::Bytes(data) => data,
        other => panic!("the field holds {other:?}"),
    }
}

/// The host engine writes `output` into `run`, as the engine that runs a command appends what it
/// printed.
fn engine_writes(host: &Host, run: &ResourceRef, output: Vec<u8>) {
    host.runtime()
        .write_origin("cmd-engine", None, |change| {
            change.write(vec![OriginWrite::Set {
                target: run.clone(),
                fields: vec![("output".to_owned(), Value::Bytes(Rc::new(output)))],
            }])
        })
        .expect("the engine's write");
}

tilo_test_env::scenario! {
    /// A run's output is bytes the platform's store keeps, small beside the record and large by
    /// the content store's address.
    fn a_runs_output_is_bytes_the_store_keeps;

    phase a_small_output_lives_inside_the_record;
    {
        let blobs = MemoryBlobs::default();
        let (host, package) = host(&blobs);
        let run = made(&host, &package);

        let printed = b"primes below 20: 2 3 5 7 11 13 17 19\n".to_vec();
        engine_writes(&host, &run, printed.clone());

        assert_eq!(
            host.runtime().read(&run, "output").unwrap(),
            Value::Bytes(Rc::new(printed.clone())),
            "the bytes read back as they were written"
        );
        assert!(
            matches!(stored(&host, &run, &package), BytesData::Inline(data) if *data == printed),
            "a small value stays inside the record"
        );
    }

    phase a_large_output_is_kept_by_the_content_store;
    {
        let blobs = MemoryBlobs::default();
        let (host, package) = host(&blobs);
        let run = made(&host, &package);

        let printed: Vec<u8> = (0..5000u32).map(|i| u8::try_from(i % 251).unwrap()).collect();
        engine_writes(&host, &run, printed.clone());

        assert_eq!(
            host.runtime().read(&run, "output").unwrap(),
            Value::Bytes(Rc::new(printed.clone()))
        );
        match stored(&host, &run, &package) {
            BytesData::Kept { hash, len } => {
                assert_eq!(len, u64::try_from(printed.len()).unwrap());
                assert_eq!(
                    blobs.read(&hash).as_deref(),
                    Some(printed.as_slice()),
                    "the bytes are in the content store, not the record"
                );
            }
            BytesData::Inline(_) => panic!("a large value stays inside the record"),
        }
    }

    phase an_action_writes_bytes_the_store_reads_back;
    {
        let blobs = MemoryBlobs::default();
        let (host, package) = host(&blobs);
        let run = made(&host, &package);

        // `hello`, as the base64 text an action reads it from.
        let out = host
            .runtime()
            .invoke_action(
                &me(),
                (
                    action_id(&package, "hold").as_str(),
                    vec![Value::resource(&run), Value::text("aGVsbG8=")],
                ),
            )
            .expect("the action runs");
        assert_eq!(out, InvokeOutcome::Done(Value::Unit));
        assert_eq!(
            host.runtime().read(&run, "output").unwrap(),
            Value::Bytes(Rc::new(b"hello".to_vec()))
        );
    }
}

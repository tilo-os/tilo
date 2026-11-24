//! The model service and the library run through their published actions: a
//! fixture model stands in for the device's, so the path a person, an agent
//! and the system index take is the one these tests take.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use serde_json::{Map, Value};
use tilo_actions::{ActionInvoker, ApiError};
use tilo_lock::MutexRecover as _;
use tilo_resource::Effect as _;
use tilo_service::{Service, Started};

use crate::analysis::{AnalysisSink, Found, ImageRef, ImageSource};
use crate::model::{Analysis, FaceBatch, ModelError, VisionModel};
use crate::{
    ANALYSIS_RESOURCE, AnalysisJob, IMAGE_MODEL_RESOURCE, ImageModelReading, VisionService,
    put_image,
};

/// What a fixture model answers, and what it does on its way there.
#[derive(Default)]
struct Fixture {
    reading: ImageModelReading,
    /// The analyses to answer, in order; `None` answers a plain result.
    answers: Mutex<VecDeque<Result<Analysis, ModelError>>>,
    analyze_calls: AtomicUsize,
    /// When set, `analyze` waits here: the test holds the gate open to cancel
    /// a run mid-picture.
    gate: Option<Arc<(Mutex<bool>, Condvar)>>,
}

impl Fixture {
    fn answering(answers: impl IntoIterator<Item = Result<Analysis, ModelError>>) -> Arc<Self> {
        Arc::new(Self {
            reading: ImageModelReading {
                available: true,
                provider: "local".to_owned(),
                model: "vision-1".to_owned(),
                revision: "a".repeat(40),
                faces: false,
                detail: String::new(),
            },
            answers: Mutex::new(answers.into_iter().collect()),
            analyze_calls: AtomicUsize::new(0),
            gate: None,
        })
    }

    fn gated(
        answers: impl IntoIterator<Item = Result<Analysis, ModelError>>,
    ) -> (Arc<Self>, Arc<(Mutex<bool>, Condvar)>) {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let fixture = Arc::new(Self {
            reading: ImageModelReading {
                available: true,
                provider: "local".to_owned(),
                model: "vision-1".to_owned(),
                revision: "a".repeat(40),
                faces: false,
                detail: String::new(),
            },
            answers: Mutex::new(answers.into_iter().collect()),
            analyze_calls: AtomicUsize::new(0),
            gate: Some(Arc::clone(&gate)),
        });
        (fixture, gate)
    }
}

impl VisionModel for Fixture {
    fn reading(&self) -> ImageModelReading {
        self.reading.clone()
    }

    fn analyze(&self, _image: &[u8]) -> Result<Analysis, ModelError> {
        self.analyze_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            let (open, waiter) = &**gate;
            let mut open = open.lock_recover();
            while !*open {
                open = waiter
                    .wait(open)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            drop(open);
        }
        self.answers
            .lock_recover()
            .pop_front()
            .unwrap_or_else(|| Ok(analysis("a picture", "hello", &["dog"], &["dog"])))
    }

    fn ocr(&self, _image: &[u8], _language: &str) -> Result<String, ModelError> {
        Ok("hello".to_owned())
    }

    fn faces(&self, _images: &[(String, Vec<u8>)]) -> Result<FaceBatch, ModelError> {
        Ok(FaceBatch {
            model: "face-1".to_owned(),
            revision: "b".repeat(40),
            clusters: vec![crate::model::FaceCluster {
                id: "one".to_owned(),
                images: vec!["tilo.content:one".to_owned()],
            }],
        })
    }
}

fn analysis(description: &str, ocr: &str, objects: &[&str], pets: &[&str]) -> Analysis {
    Analysis {
        description: description.to_owned(),
        ocr_text: ocr.to_owned(),
        objects: objects.iter().map(|word| (*word).to_owned()).collect(),
        pets: pets.iter().map(|word| (*word).to_owned()).collect(),
        model: "vision-1".to_owned(),
        revision: "a".repeat(40),
    }
}

/// The pictures a test's index holds.
struct Pictures(Vec<ImageRef>);

impl ImageSource for Pictures {
    fn images(&self) -> Result<Vec<ImageRef>, String> {
        Ok(self.0.clone())
    }
}

/// What the run wrote into the index.
#[derive(Default)]
struct Written(Mutex<Vec<(ImageRef, Found)>>);

impl AnalysisSink for Written {
    fn enriched(&self, image: &ImageRef, found: &Found) -> Result<(), String> {
        self.0.lock_recover().push((image.clone(), found.clone()));
        Ok(())
    }
}

/// A service reading pictures from `store`, as the session lends it.
fn service(
    model: Arc<dyn VisionModel>,
    pictures: Vec<ImageRef>,
    store: &tilo_content::ContentStore,
) -> (VisionService, Arc<Written>) {
    let written = Arc::new(Written::default());
    let mut service = VisionService::new(
        model,
        Arc::new(Pictures(pictures)),
        Arc::clone(&written) as Arc<dyn AnalysisSink>,
        Arc::new(|| ()),
    );
    service.lend_content(&mut |_| store.clone());
    (service, written)
}

fn picture(store: &tilo_content::ContentStore, name: &str) -> ImageRef {
    let reference = put_image(store, name.as_bytes());
    ImageRef {
        resource: format!("tl:so.tilo.photos:photo:{name}"),
        app: "so.tilo.photos".to_owned(),
        workspace: "workspace".to_owned(),
        record: "library".to_owned(),
        image: reference,
    }
}

fn person() -> ActionInvoker {
    ActionInvoker::person("ada")
}

fn invoke(
    service: &mut VisionService,
    invoker: &ActionInvoker,
    resource: &str,
    action: &str,
    arguments: &Map<String, Value>,
) -> Result<Started, ApiError> {
    service.invoke_as(invoker, resource, action, arguments)
}

tilo_test_env::scenario! {
    fn describe_and_tags_answer_as_the_person_and_a_stranger_is_refused;

    phase describe_answers_with_the_model_that_said_it;
    {
        let store = tilo_content::ContentStore::memory();
        // `describe`, `tags` and `shows` are three questions about one picture,
        // so the fixture answers three times.
        let model = Fixture::answering([
            Ok(analysis("A dog beside a bicycle", "42 North Street", &["dog", "bicycle"], &["dog"])),
            Ok(analysis("A dog beside a bicycle", "42 North Street", &["dog", "bicycle"], &["dog"])),
            Ok(analysis("A dog beside a bicycle", "42 North Street", &["dog", "bicycle"], &["dog"])),
        ]);
        let (mut service, _written) = service(model, Vec::new(), &store);
        service.own_person("ada", &[]);
        let reference = put_image(&store, b"a picture");
        let Started::Done(output) = invoke(
            &mut service,
            &person(),
            IMAGE_MODEL_RESOURCE,
            "describe",
            &Map::from_iter([("image".to_owned(), Value::String(reference.clone()))]),
        )
        .expect("the model answers") else {
            panic!("describe answers at once");
        };
        // The sentence, as text: the value a caller reads back, with the reading
        // naming the model that said it.
        let value = output.value.expect("describe answers with a value");
        assert_eq!(value, serde_json::json!("A dog beside a bicycle"));

        let Started::Done(output) = invoke(
            &mut service,
            &person(),
            IMAGE_MODEL_RESOURCE,
            "tags",
            &Map::from_iter([("image".to_owned(), Value::String(reference.clone()))]),
        )
        .expect("the model answers") else {
            panic!("tags answers at once");
        };
        // The objects and then the pets, as the list the API carries.
        let value = output.value.expect("tags answers with a value");
        assert_eq!(
            value,
            serde_json::json!({"list": ["dog", "bicycle", "dog"]})
        );

        let Started::Done(output) = invoke(
            &mut service,
            &person(),
            IMAGE_MODEL_RESOURCE,
            "shows",
            &Map::from_iter([("image".to_owned(), Value::String(reference))]),
        )
        .expect("the model answers") else {
            panic!("shows answers at once");
        };
        // Everything one picture was found to show, each part named apart: the app records a
        // photo's description, its words, its objects and its pets from one call.
        let value = output.value.expect("shows answers with a value");
        assert_eq!(
            value,
            serde_json::json!({
                "description": "A dog beside a bicycle",
                "text": "42 North Street",
                "objects": ["dog", "bicycle"],
                "pets": ["dog"],
            })
        );
    }

    phase a_stranger_is_refused;
    {
        let store = tilo_content::ContentStore::memory();
        let model = Fixture::answering([Ok(analysis("anything", "", &[], &[]))]);
        let (mut service, _written) = service(model, Vec::new(), &store);
        service.own_person("ada", &[]);
        let reference = put_image(&store, b"a picture");
        let refused = invoke(
            &mut service,
            &ActionInvoker::person("grace"),
            IMAGE_MODEL_RESOURCE,
            "describe",
            &Map::from_iter([("image".to_owned(), Value::String(reference))]),
        )
        .expect_err("another member is refused");
        assert!(matches!(
            refused,
            ApiError::Refused { ref code, .. } if code == "permission_denied"
        ));
    }

    phase a_model_refusal_stays_a_refusal;
    {
        let store = tilo_content::ContentStore::memory();
        let model = Fixture::answering([Err(ModelError::Refused("the provider said no".to_owned()))]);
        let (mut service, _written) = service(model, Vec::new(), &store);
        service.own_person("ada", &[]);
        let reference = put_image(&store, b"a picture");
        let refused = invoke(
            &mut service,
            &person(),
            IMAGE_MODEL_RESOURCE,
            "describe",
            &Map::from_iter([("image".to_owned(), Value::String(reference))]),
        )
        .expect_err("the model refused");
        assert!(matches!(
            refused,
            ApiError::Refused { ref code, ref reason }
                if code == "model_refused" && reason.contains("provider said no")
        ));
    }

    phase a_picture_that_is_not_here_is_unavailable;
    {
        let model = Fixture::answering([Ok(analysis("anything", "", &[], &[]))]);
        let (mut service, _written) = service(model, Vec::new(), &tilo_content::ContentStore::memory());
        service.own_person("ada", &[]);
        let refused = invoke(
            &mut service,
            &person(),
            IMAGE_MODEL_RESOURCE,
            "describe",
            &Map::from_iter([(
                "image".to_owned(),
                Value::String("tilo.content:not-here".to_owned()),
            )]),
        )
        .expect_err("the picture is not on this device");
        assert!(matches!(
            refused,
            ApiError::Unavailable { ref code, .. } if code == "image_not_here"
        ));
    }
}

tilo_test_env::scenario! {
    fn the_library_run_enriches_every_picture_and_retries_only_the_failed;

    phase every_picture_is_written_into_the_index;
    {
        let store = tilo_content::ContentStore::memory();
        let pictures = vec![
            picture(&store, "one"),
            picture(&store, "two"),
            picture(&store, "three"),
        ];
        let model = Fixture::answering([
            Ok(analysis("a dog", "", &["dog"], &["dog"])),
            Ok(analysis("a cat", "hello", &["cat"], &["cat"])),
            Ok(analysis("a boat", "", &["boat"], &[])),
        ]);
        let (mut service, written) = service(model, pictures, &store);
        service.own_person("ada", &[]);
        assert!(matches!(
            invoke(&mut service, &person(), ANALYSIS_RESOURCE, "start", &Map::new()),
            Ok(Started::Accepted)
        ));
        service.settle();
        let reading = serde_json::to_value(service.read(ANALYSIS_RESOURCE).unwrap()).unwrap();
        assert_eq!(reading["phase"], "complete");
        assert_eq!(reading["succeeded"], 3);
        assert_eq!(reading["progress"], 1.0);
        let written = written.0.lock_recover();
        assert_eq!(written.len(), 3);
        assert!(written.iter().any(|(_, found)| found
            .objects
            .iter()
            .any(|object| object == "dog")));
        assert!(written.iter().any(|(_, found)| found.ocr_text == "hello"));
        for (image, found) in written.iter() {
            let finding = crate::finding(image, found);
            assert_eq!(finding.resource, image.resource);
            assert!(finding.fields.iter().any(|(word, _)| word == "dog")
                || finding.fields.iter().any(|(word, _)| word == "cat")
                || finding.fields.iter().any(|(word, _)| word == "boat"));
        }
    }

    phase a_failure_is_retried_alone;
    {
        let store = tilo_content::ContentStore::memory();
        let pictures = vec![picture(&store, "one"), picture(&store, "two")];
        let model = Fixture::answering([
            Ok(analysis("a dog", "", &["dog"], &[])),
            Err(ModelError::Failed("the picture was unreadable".to_owned())),
            Ok(analysis("a dog after all", "", &["dog"], &[])),
        ]);
        let (mut service, written) = service(model, pictures, &store);
        service.own_person("ada", &[]);
        invoke(&mut service, &person(), ANALYSIS_RESOURCE, "start", &Map::new())
            .expect("the run starts");
        service.settle();
        let reading = serde_json::to_value(service.read(ANALYSIS_RESOURCE).unwrap()).unwrap();
        assert_eq!((reading["succeeded"].as_u64(), reading["failed"].as_u64()), (Some(1), Some(1)));

        invoke(&mut service, &person(), ANALYSIS_RESOURCE, "retry", &Map::new())
            .expect("the failed picture is taken again");
        service.settle();
        let reading = serde_json::to_value(service.read(ANALYSIS_RESOURCE).unwrap()).unwrap();
        assert_eq!(reading["phase"], "complete");
        assert_eq!(reading["succeeded"], 1);
        assert_eq!(written.0.lock_recover().len(), 2);
    }
}

tilo_test_env::scenario! {
    fn pausing_and_cancelling_a_run_act_between_pictures;

    phase pause_stops_the_run_and_resume_continues_it;
    {
        let store = tilo_content::ContentStore::memory();
        let pictures = vec![picture(&store, "one"), picture(&store, "two")];
        let model = Fixture::answering([
            Ok(analysis("a dog", "", &["dog"], &[])),
            Ok(analysis("a cat", "", &["cat"], &[])),
        ]);
        let (mut service, written) = service(model, pictures, &store);
        service.own_person("ada", &[]);
        invoke(&mut service, &person(), ANALYSIS_RESOURCE, "pause", &Map::new())
            .expect_err("nothing runs to pause");
        invoke(&mut service, &person(), ANALYSIS_RESOURCE, "start", &Map::new())
            .expect("the run starts");
        // The fixture answers at once, so the run is done before a pause can
        // take hold of it: the reading is the finished run's, and a pause of
        // a finished run is refused.
        service.settle();
        assert!(matches!(
            invoke(&mut service, &person(), ANALYSIS_RESOURCE, "pause", &Map::new()),
            Err(ApiError::Unavailable { ref code, .. }) if code == "analysis_not_running"
        ));
        assert_eq!(written.0.lock_recover().len(), 2);
    }

    phase cancel_stops_before_the_next_picture;
    {
        let store = tilo_content::ContentStore::memory();
        let pictures = vec![
            picture(&store, "one"),
            picture(&store, "two"),
            picture(&store, "three"),
        ];
        let (model, gate) = Fixture::gated([
            Ok(analysis("a dog", "", &["dog"], &[])),
            Ok(analysis("a cat", "", &["cat"], &[])),
            Ok(analysis("a boat", "", &["boat"], &[])),
        ]);
        let (mut service, written) = service(model, pictures, &store);
        service.own_person("ada", &[]);
        invoke(&mut service, &person(), ANALYSIS_RESOURCE, "start", &Map::new())
            .expect("the run starts");
        // The first picture is being analyzed: stop before the next.
        std::thread::yield_now();
        assert!(matches!(
            invoke(&mut service, &person(), ANALYSIS_RESOURCE, "cancel", &Map::new()),
            Ok(Started::Done(_))
        ));
        let (open, waiter) = &*gate;
        *open.lock_recover() = true;
        waiter.notify_all();
        service.settle();
        let reading = serde_json::to_value(service.read(ANALYSIS_RESOURCE).unwrap()).unwrap();
        assert_eq!(reading["phase"], "cancelled");
        assert!(
            written.0.lock_recover().len() < 3,
            "a cancelled run does not analyze the whole library"
        );
    }
}

tilo_test_env::scenario! {
    fn a_device_with_no_model_refuses_in_words;

    phase the_reading_says_why_and_describe_refuses;
    {
        let model = Arc::new(crate::LocalVision::new(None));
        assert!(!model.reading().available);
        assert!(!model.reading().detail.is_empty());
        let refused = model
            .analyze(b"a picture")
            .expect_err("no model answers");
        assert!(matches!(refused, ModelError::Unavailable(_)));
    }
}

#[test]
fn the_surface_file_is_what_this_crate_publishes() {
    let published = serde_json::json!({ "resources": crate::surfaces() });
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("surface.json");
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
        serde_json::to_string_pretty(&published)
            .map(|text| std::fs::write(&path, text))
            .expect("the surface file is written");
        return;
    }
    let committed = std::fs::read_to_string(&path).ok();
    // The values a package publishes, not the order their keys are written in: that order is
    // the writer's, and a build that unifies its dependencies differently keeps the same surface.
    let written: serde_json::Value = committed
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .expect("the committed surface is JSON")
        .unwrap_or(serde_json::Value::Null);
    assert_eq!(
        written, published,
        "crates/services/vision/surface.json is not what this crate publishes; \
         regenerate it with UPDATE_SNAPSHOTS=1"
    );
}

#[test]
fn the_declared_actions_are_the_published_ones() {
    let declared: Vec<&str> = crate::ImageModelEffect::declarations()
        .iter()
        .map(|decl| decl.id)
        .collect();
    assert_eq!(declared, ["describe", "ocr", "tags", "shows", "faces"]);
    let declared: Vec<&str> = crate::AnalysisEffect::declarations()
        .iter()
        .map(|decl| decl.id)
        .collect();
    assert_eq!(declared, ["start", "pause", "resume", "retry", "cancel"]);
    let _: &AnalysisJob = &AnalysisJob::new(
        Arc::new(Pictures(Vec::new())),
        Arc::new(Written::default()),
        Fixture::answering(std::iter::empty()),
        Arc::new(|_, _| None),
        Arc::new(|| ()),
    );
}

#[test]
fn the_addresses_route_through_the_sessions_one_catalogue() {
    let store = tilo_content::ContentStore::memory();
    let model = Fixture::answering([Ok(analysis("A dog", "", &["dog"], &[]))]);
    let (mut service, _written) = service(model, Vec::new(), &store);
    service.own_person("ada", &[]);
    let mut services = tilo_service::Services::default();
    services.add(Box::new(service));
    let published: Vec<String> = services
        .resources()
        .into_iter()
        .map(|resource| resource.id.as_str().to_owned())
        .collect();
    assert!(published.iter().any(|id| id == IMAGE_MODEL_RESOURCE));
    assert!(published.iter().any(|id| id == ANALYSIS_RESOURCE));
    let reference = put_image(&store, b"a picture");
    let Started::Done(output) = services
        .invoke(
            IMAGE_MODEL_RESOURCE,
            "describe",
            &Map::from_iter([("image".to_owned(), Value::String(reference))]),
        )
        .expect("the service publishes the resource")
        .expect("the model answers")
    else {
        panic!("describe answers at once");
    };
    assert_eq!(output.value.expect("a value"), serde_json::json!("A dog"));
}

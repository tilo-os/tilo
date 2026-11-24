//! Rows of a declared type: a plain struct as `RowType`, the effects offered
//! on every row of it (`#[effect(on = Row)]`, bound to the row's id), a
//! status that is about one row, and an item's presentation from state its
//! document does not hold.

use tilo_app::framework::{EffectStatus, Effected, Publishes};
use tilo_app::prelude::*;
use tilo_app_test_host::TestApp;

#[derive(tilo_resource::Row)]
#[row(kind = "test.jobs", label = "Job", effects = JobEffect)]
struct Job;

#[derive(tilo_resource::Row)]
#[row(kind = "test.songs", label = "Song", effects = JobEffect)]
struct Song;

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "test.jobs.effect", catalogue = "tests/locales")]
pub enum JobEffect {
    /// Stop this job.
    #[effect(on = Job, kind = "read")]
    Stop {
        #[param("The job")]
        job: String,
        #[param("Stop it even if it is busy")]
        force: Option<bool>,
    },
    /// Play this.
    ///
    /// One effect offered on two types of row, bound to whichever row it
    /// was offered on.
    #[effect(on = Job, on = Song, kind = "read")]
    Play {
        #[param("The job or song")]
        item: String,
    },
    /// Skip ahead.
    ///
    /// An effect that takes nothing: every row of the type offers it as it
    /// is.
    #[effect(on = Job, kind = "read")]
    Skip,
    /// Retarget this job.
    #[effect(kind = "read")]
    Retarget {
        #[param(resource = "test.jobs", "The job to retarget")]
        job: String,
    },
    /// Wipe this job.
    #[effect(on = Job, kind = "outside", destructive, phrase = "Self::wipe_name")]
    Wipe {
        #[param("The job")]
        job: String,
    },
}

impl JobEffect {
    fn wipe_name(effect: &Self) -> String {
        match effect {
            Self::Wipe { job } => format!("wipe {job}"),
            _ => String::new(),
        }
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct JobItem {
    /// Name
    #[model(label)]
    pub name: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Jobs {
    pub jobs: Collection<JobItem>,
}

#[resource(kind = "test.jobs", catalogue = "tests/locales")]
impl Jobs {
    /// Add a job.
    #[action]
    pub fn add_job(&mut self, cx: &mut ActionCx, name: String) {
        self.jobs.push(cx.id(), JobItem { name });
    }
}

fn find<'a>(
    resources: &'a [tilo_actions::ResourceSpec],
    id: &str,
) -> Option<&'a tilo_actions::ResourceSpec> {
    resources.iter().find_map(|resource| {
        (resource.id.as_str() == id)
            .then_some(resource)
            .or_else(|| find(&resource.children, id))
    })
}

#[test]
fn a_row_type_offers_its_effects_bound_to_the_row() {
    let row = LiveRow::typed::<Job>("job:7", "Backup").into_resource();
    assert_eq!(row.kind, "test.jobs");
    assert_eq!(row.label, "Backup");
    assert!(
        row.resource_type.is_some(),
        "search names what a row is by its type"
    );
    let stop = row
        .actions
        .iter()
        .find(|action| action.id == "stop")
        .unwrap();
    assert_eq!(stop.kind, tilo_actions::OperationKind::Effect);
    assert_eq!(
        stop.params()
            .iter()
            .map(|param| param.name.to_string())
            .collect::<Vec<_>>(),
        ["force"],
        "the row is the job; only the force flag is asked"
    );
    let encoded = stop.build_json(&serde_json::Map::new()).unwrap();
    assert_eq!(
        JobEffect::from_payload(&encoded.payload).unwrap(),
        JobEffect::Stop {
            job: "job:7".into(),
            force: None,
        }
    );
}

/// An effect offered on several row types is bound to whichever row it was
/// offered on, and one that takes nothing is offered on the rows as it is.
#[test]
fn a_row_effect_may_be_offered_on_several_types_and_take_nothing() {
    let find = |row: &tilo_actions::ResourceSpec, id: &str| {
        row.actions
            .iter()
            .find(|action| action.id == id)
            .unwrap_or_else(|| panic!("{id} on {}", row.id.as_str()))
            .clone()
    };

    let job = LiveRow::typed::<Job>("job:7", "Backup").into_resource();
    let play = find(&job, "play");
    let encoded = play.build_json(&serde_json::Map::new()).unwrap();
    assert_eq!(
        JobEffect::from_payload(&encoded.payload).unwrap(),
        JobEffect::Play {
            item: "job:7".into()
        },
        "a job's play is about the job"
    );

    let song = LiveRow::typed::<Song>("song:3", "Layla").into_resource();
    let play = find(&song, "play");
    let encoded = play.build_json(&serde_json::Map::new()).unwrap();
    assert_eq!(
        JobEffect::from_payload(&encoded.payload).unwrap(),
        JobEffect::Play {
            item: "song:3".into()
        },
        "a song's play is about the song"
    );

    let skip = find(&job, "skip");
    assert!(
        skip.params().is_empty(),
        "an effect that takes nothing asks nothing on a row"
    );
    let encoded = skip.build_json(&serde_json::Map::new()).unwrap();
    assert_eq!(
        JobEffect::from_payload(&encoded.payload).unwrap(),
        JobEffect::Skip,
        "every row of the type offers it as it is"
    );

    let ids: Vec<&str> = song
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["play"],
        "a song offers only what the declarations say"
    );
}

#[derive(tilo_resource::Row)]
#[row(kind = "test.transfers", label = "Transfer", effects = TransferEffect)]
struct Transfer;

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "test.transfers.effect", catalogue = "tests/locales")]
pub enum TransferEffect {
    /// Pause this transfer.
    #[effect(on = Transfer, kind = "read")]
    Pause {
        #[param("The transfer")]
        transfer: String,
        #[param("The minute it may continue at")]
        until: String,
    },
    /// Cancel this transfer.
    #[effect(on = Transfer, kind = "read")]
    Cancel {
        #[param("The transfer")]
        transfer: String,
    },
}

#[test]
fn a_row_binds_its_own_state_into_the_effects_it_offers() {
    let bindings = tilo_resource::RowBindings::new().bind(
        TransferEffect::PAUSE,
        [("transfer", "t:1".into()), ("until", "6".into())],
    );
    let row = LiveRow::typed_with::<Transfer>("t:1", "Backup", &bindings).into_resource();
    assert_eq!(
        row.actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>(),
        ["pause"],
        "only the effect the row's state offers is published, with all its arguments bound"
    );
    let pause = &row.actions[0];
    assert!(pause.params().is_empty());
    let encoded = pause.build_json(&serde_json::Map::new()).unwrap();
    assert_eq!(
        TransferEffect::from_payload(&encoded.payload).unwrap(),
        TransferEffect::Pause {
            transfer: "t:1".into(),
            until: "6".into(),
        }
    );
}

#[test]
#[should_panic(expected = "names none")]
fn a_resource_no_declaration_shaped_is_not_a_row() {
    let _ = LiveRow::declared(tilo_actions::ResourceSpec::new("x", "test.none", "None"));
}

#[test]
fn a_row_asks_the_value_it_would_act_with_as_the_default() {
    let bindings = tilo_resource::RowBindings::new().bind_asking(
        TransferEffect::PAUSE,
        [("transfer", "t:3".into()), ("until", "6".into())],
        &["until"],
    );
    let row = LiveRow::typed_with::<Transfer>("t:3", "Backup", &bindings).into_resource();
    let pause = &row.actions[0];
    assert_eq!(
        pause
            .params()
            .iter()
            .map(|param| param.name.to_string())
            .collect::<Vec<_>>(),
        ["until"],
        "the row's own value is asked, not taken"
    );
    assert_eq!(
        pause.params()[0]
            .schema
            .as_deref()
            .and_then(|schema| schema.get("default"))
            .and_then(serde_json::Value::as_str),
        Some("6"),
        "the row's own value is the field's default"
    );
    let given = pause
        .build_json(&serde_json::Map::from_iter([(
            "until".to_owned(),
            "9".into(),
        )]))
        .unwrap();
    assert_eq!(
        TransferEffect::from_payload(&given.payload).unwrap(),
        TransferEffect::Pause {
            transfer: "t:3".into(),
            until: "9".into(),
        },
        "a caller may give another value"
    );
    assert!(
        pause.build_json(&serde_json::Map::new()).is_err(),
        "the parameter stays asked: a caller gives it"
    );
}

#[test]
fn a_row_offering_an_effect_asks_what_its_state_does_not_know() {
    let bindings = tilo_resource::RowBindings::new().offer(TransferEffect::PAUSE);
    let row = LiveRow::typed_with::<Transfer>("t:2", "Index", &bindings).into_resource();
    let pause = &row.actions[0];
    assert_eq!(
        pause
            .params()
            .iter()
            .map(|param| param.name.to_string())
            .collect::<Vec<_>>(),
        ["transfer", "until"],
        "the row offers the effect and its caller names what it acts on"
    );
    let encoded = pause
        .build_json(&serde_json::Map::from_iter([
            ("transfer".to_owned(), "t:2".into()),
            ("until".to_owned(), "9".into()),
        ]))
        .unwrap();
    assert_eq!(
        TransferEffect::from_payload(&encoded.payload).unwrap(),
        TransferEffect::Pause {
            transfer: "t:2".into(),
            until: "9".into(),
        }
    );
}

#[test]
fn a_row_may_name_its_press_the_way_only_it_can() {
    let backup = LiveRow::typed::<Job>("job:1", "Backup")
        .labelled(JobEffect::STOP, "Stop Backup")
        .into_resource();
    let index = LiveRow::typed::<Job>("job:2", "Index").into_resource();
    let label = |row: &tilo_actions::ResourceSpec| {
        row.actions
            .iter()
            .find(|action| action.id == "stop")
            .map(|action| action.label.clone())
            .unwrap()
    };
    assert_eq!(label(&backup), "Stop Backup");
    assert_eq!(
        label(&index),
        "Stop this job",
        "one row's label is not the type's, nor another row's"
    );
}

#[test]
fn a_row_id_is_parsed_as_the_field_the_effect_acts_with() {
    let row = LiveRow::typed::<Minute>("12", "12:30").into_resource();
    let wait = row
        .actions
        .iter()
        .find(|action| action.id == "wait")
        .expect("the minute offers its wait");
    let encoded = wait.build_json(&serde_json::Map::new()).unwrap();
    assert_eq!(
        MinuteEffect::from_payload(&encoded.payload).unwrap(),
        MinuteEffect::Wait { minute: 12 },
        "the row id is the number the effect takes, never text it cannot read"
    );

    let unreadable = LiveRow::typed::<Minute>("minute:twelve", "12:30").into_resource();
    let wait = unreadable
        .actions
        .iter()
        .find(|action| action.id == "wait")
        .expect("the row is still published");
    assert_eq!(
        wait.contract.availability,
        tilo_actions::Availability::Unavailable {
            code: "row_id".into(),
            reason: "`minute:twelve` is not one this effect acts on".into(),
        },
        "a row whose id is not what the effect acts on is offered refused, with why"
    );
}

#[derive(tilo_resource::Row)]
#[row(kind = "test.minutes", label = "Minute", effects = MinuteEffect)]
struct Minute;

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "test.minutes.effect", catalogue = "tests/locales")]
pub enum MinuteEffect {
    /// Wait out this minute.
    #[effect(on = Minute, kind = "read")]
    Wait {
        #[param("The minute to wait out")]
        minute: u64,
    },
}

#[test]
fn a_row_offered_effect_keeps_the_phrase_its_caller_types() {
    let row = LiveRow::typed::<Job>("job:7", "Backup").into_resource();
    let wipe = row
        .actions
        .iter()
        .find(|action| action.id == "wipe")
        .expect("the row offers its wipe");
    assert_eq!(
        wipe.contract.confirmation,
        tilo_actions::ConfirmationPolicy::Phrase {
            phrase: "wipe job:7".into(),
        },
        "a destructive effect bound to a row asks for the phrase it declares"
    );
}

#[derive(tilo_resource::Row)]
#[row(kind = "test.colours", label = "Colour", effects = ColourEffect)]
struct Colour;

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "test.colours.effect", catalogue = "tests/locales")]
pub enum ColourEffect {
    /// Recolour this calendar.
    #[effect(on = Colour, kind = "read")]
    Recolour {
        #[param("The calendar")]
        calendar: String,
        #[param("The colour")]
        colour: String,
    },
}

#[test]
fn a_row_prefills_a_parameter_with_what_its_state_knows() {
    let row = LiveRow::typed::<Colour>("c:1", "Work")
        .prefilled(ColourEffect::RECOLOUR, [("colour", "amber".into())])
        .into_resource();
    let recolour = row
        .actions
        .iter()
        .find(|action| action.id == "recolour")
        .expect("the row offers its recolour");
    assert_eq!(
        recolour
            .params()
            .iter()
            .map(|param| param.name.to_string())
            .collect::<Vec<_>>(),
        ["colour"],
        "the colour is still asked, so its caller may change it"
    );
    assert_eq!(
        recolour.params()[0]
            .schema
            .as_deref()
            .and_then(|schema| schema.get("default")),
        Some(&serde_json::Value::String("amber".into())),
        "the row's own colour fills the field"
    );
    let encoded = recolour
        .build_json(&serde_json::Map::from_iter([(
            "colour".to_owned(),
            "red".into(),
        )]))
        .unwrap();
    assert_eq!(
        ColourEffect::from_payload(&encoded.payload).unwrap(),
        ColourEffect::Recolour {
            calendar: "c:1".into(),
            colour: "red".into(),
        },
        "what the caller finally gives is what it acts with"
    );
}

#[derive(tilo_resource::Row)]
#[row(kind = "test.speeds", label = "Speed", effects = SpeedEffect)]
struct Speed;

/// A typed payload an effect carries as one of its arguments, as the shell's
/// own commands do: a declared, described enum, never an escape hatch.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Slow,
    Fast,
}

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "test.speeds.effect", catalogue = "tests/locales")]
pub enum SpeedEffect {
    /// Set the speed.
    #[effect(on = Speed, kind = "read")]
    SetMode {
        #[param("The speed")]
        speed: String,
        #[param("The mode")]
        mode: Mode,
    },
}

#[test]
fn an_effect_carries_a_typed_payload_as_one_of_its_arguments() {
    let row = LiveRow::typed::<Speed>("s:1", "Fan").into_resource();
    let set_mode = row
        .actions
        .iter()
        .find(|action| action.id == "set-mode")
        .expect("the row offers its mode");
    assert_eq!(
        set_mode
            .params()
            .iter()
            .map(|param| param.name.to_string())
            .collect::<Vec<_>>(),
        ["mode"]
    );
    let encoded = set_mode
        .build_json(&serde_json::Map::from_iter([(
            "mode".to_owned(),
            "fast".into(),
        )]))
        .unwrap();
    assert_eq!(
        SpeedEffect::from_payload(&encoded.payload).unwrap(),
        SpeedEffect::SetMode {
            speed: "s:1".into(),
            mode: Mode::Fast,
        },
        "a typed command travels as the argument its declaration describes"
    );
}

#[test]
fn a_control_binds_a_declared_effect_at_its_resource_with_no_carrier_row() {
    let binding = JobEffect::SKIP
        .binding("so.tilo.test-jobs:jobs")
        .expect("the declaration publishes skip");
    let invocation = binding.invoke(()).expect("skip takes nothing");
    assert_eq!(invocation.address.resource, "so.tilo.test-jobs:jobs");
    assert_eq!(invocation.address.action, "skip");
    assert!(
        invocation.arguments.is_empty(),
        "a nullary effect asks nothing a control must supply"
    );
}

#[test]
fn a_row_binds_an_effect_named_only_when_it_invokes() {
    let bindings = tilo_resource::RowBindings::new()
        .bind_id("pause", [("transfer", "t:3".into()), ("until", "7".into())]);
    let row = LiveRow::typed_with::<Transfer>("t:3", "Restore", &bindings).into_resource();
    let pause = row
        .actions
        .iter()
        .find(|action| action.id == "pause")
        .expect("the row offers what it named");
    let encoded = pause.build_json(&serde_json::Map::new()).unwrap();
    assert_eq!(
        TransferEffect::from_payload(&encoded.payload).unwrap(),
        TransferEffect::Pause {
            transfer: "t:3".into(),
            until: "7".into(),
        }
    );
}

#[test]
fn offering_an_effect_on_a_row_keeps_its_audience() {
    let stop = <JobEffect as tilo_resource::Effect>::specs()
        .into_iter()
        .find(|spec| spec.id == "stop")
        .expect("stop is published on the resource");
    assert_eq!(
        stop.audience(),
        tilo_actions::Audience::Everyone,
        "a declaration that names where it is offered never changes who sees it"
    );
}

#[test]
fn a_pickable_resource_parameter_names_the_kind_it_takes() {
    let spec = JobEffect::spec("retarget").expect("the effect is declared");
    let job = &spec.params()[0];
    assert_eq!(job.name.as_ref(), "job");
    assert_eq!(job.ty, tilo_actions::ParamType::Resource);
    assert_eq!(
        job.resource_kind(),
        Some("test.jobs"),
        "a surface offers a person the resources of that kind"
    );
    let row = <JobEffect as tilo_resource::Effect>::rows()
        .into_iter()
        .find(|row| row.id == "retarget")
        .unwrap();
    assert_eq!(
        (row.param_types)()[0].1(&mut tilo_actions::surface::TypeSet::default()),
        tilo_actions::surface::FieldType::Resource("test.jobs".into())
    );
}

struct RowPublishingApp;

/// This test's own words: the catalogue the framework reads the effect's
/// labels and parameter descriptions from at publication.
static CATALOG: tilo_app::i18n::EmbeddedCatalog =
    tilo_app::i18n::EmbeddedCatalog::new("en-US", &[("en-US", include_str!("locales/en-US.ftl"))])
        .with_language_pack(tilo_app::i18n::LanguagePack {
            id: env!("CARGO_PKG_NAME"),
            locales: &["en-US"],
            source_dir: concat!(env!("CARGO_MANIFEST_DIR"), "/tests/locales"),
        })
        .with_system_locale();

thread_local! {
    static RAN: std::cell::RefCell<Vec<JobEffect>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl ResourceApp for RowPublishingApp {
    type Resource = Jobs;
    const ID: &'static str = "so.tilo.test-jobs";
    const TITLE: &'static str = "Jobs";

    fn catalog() -> Option<&'static tilo_app::i18n::EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn view(cx: &mut ViewCx<Jobs>) -> Element {
        cx.effects(|effect: JobEffect, _| {
            RAN.with(|ran| ran.borrow_mut().push(effect));
            Ok(Effected::done())
        });
        cx.publishing(
            Publishes::new()
                .live_rows(|| {
                    vec![
                        LiveRow::typed::<Job>("job:1", "Backup"),
                        LiveRow::typed::<Job>("job:2", "Index"),
                        // A job whose steps are rows of their own: the status
                        // of a row nested beneath another reaches it too.
                        LiveRow::typed::<Job>("job:3", "Sweep")
                            .row(LiveRow::typed::<Job>("job:3:step", "Sweep step")),
                    ]
                })
                .row_status(|id| match id {
                    "job:1" => vec![
                        EffectStatus::described(JobEffect::STOP, "This job can be stopped now")
                            .on(id),
                    ],
                    "job:2" => vec![
                        EffectStatus::unavailable(
                            JobEffect::STOP,
                            "busy",
                            "This job is busy and cannot be stopped now",
                        )
                        .on(id)
                        .with_phrase("job:2"),
                    ],
                    _ => (id == "job:3:step")
                        .then(|| {
                            EffectStatus::unavailable(
                                JobEffect::STOP,
                                "busy",
                                "This job is busy and cannot be stopped now",
                            )
                            .on(id)
                        })
                        .into_iter()
                        .collect(),
                })
                .item_presentation(|field, id| {
                    (field == "jobs").then(|| tilo_actions::ResourcePresentation {
                        detail: Some(format!("item {id}")),
                        ..Default::default()
                    })
                }),
            Box::new(Column::new()),
        )
    }
}

#[test]
fn a_row_status_and_an_items_presentation_reach_the_published_tree() {
    let mut app = TestApp::<RowPublishingApp>::new().unwrap();
    app.invoke(Jobs::ADD_JOB.with("Reindex")).unwrap();
    let id = app.state().jobs.ids().next().cloned().unwrap();
    let item = tilo_resource::items::item_resource_id::<Jobs>("jobs", &id);
    let resources = app.harness().resources();

    let row = find(&resources, "job:1").expect("a typed row is published");
    let stop = row
        .actions
        .iter()
        .find(|action| action.id == "stop")
        .unwrap();
    assert!(
        stop.contract.availability.is_available(),
        "a row says nothing of this row's effects unless its status does"
    );

    let busy = find(&resources, "job:2").expect("the second row is published");
    let stop = busy
        .actions
        .iter()
        .find(|action| action.id == "stop")
        .unwrap();
    assert_eq!(
        stop.contract.availability,
        tilo_actions::Availability::Unavailable {
            code: "busy".into(),
            reason: "This job is busy and cannot be stopped now".into(),
        },
        "the status is about this row alone"
    );

    assert_eq!(
        stop.contract.confirmation,
        tilo_actions::ConfirmationPolicy::Phrase {
            phrase: "job:2".into()
        },
        "a status says what its caller types to confirm the effect here"
    );

    let idle = find(&resources, "job:1").expect("the first job is published");
    let stop = idle
        .actions
        .iter()
        .find(|action| action.id == "stop")
        .unwrap();
    assert_eq!(
        stop.description.as_deref(),
        Some("This job can be stopped now"),
        "a status says what the effect does on this row"
    );

    let nested = find(&resources, "job:3:step").expect("a nested row is published");
    let stop = nested
        .actions
        .iter()
        .find(|action| action.id == "stop")
        .unwrap();
    assert_eq!(
        stop.contract.availability,
        tilo_actions::Availability::Unavailable {
            code: "busy".into(),
            reason: "This job is busy and cannot be stopped now".into(),
        },
        "a status reaches the row it is about wherever it is published"
    );

    let published = find(&resources, &item).expect("a labelled item is published");
    assert_eq!(
        published
            .presentation
            .as_ref()
            .and_then(|presentation| presentation.detail.as_deref()),
        Some(format!("item {id}").as_str()),
        "the view's presentation is on the item its document does not hold it for"
    );

    CATALOG.ensure();
    let published = JobEffect::spec("retarget").expect("the effect is declared");
    assert_eq!(
        published.params()[0].description,
        "The job to move",
        "a parameter's description is the catalogue's word, read in the person's language, \
         never the source text the declaration carries"
    );
}

#[derive(tilo_resource::Row)]
#[row(kind = "test.plain", label = "Plain")]
struct Plain;

#[test]
fn a_rows_own_row_offers_its_vocabularys_effects_unasked() {
    let row = LiveRow::typed::<Plain>("plain:1", "Plain")
        .vocabulary::<JobEffect>()
        .into_resource();
    let ids: Vec<&str> = row
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    for expected in ["play", "skip", "stop"] {
        assert!(
            ids.contains(&expected),
            "the vocabulary's effects are offered on the row as the resource offers them: {ids:?}"
        );
    }
    let stop = row
        .actions
        .iter()
        .find(|action| action.id == "stop")
        .unwrap();
    assert_eq!(
        stop.params()
            .iter()
            .map(|param| param.name.to_string())
            .collect::<Vec<_>>(),
        ["job", "force"],
        "nothing is bound: what the effect takes is asked"
    );
}

/// What a row's status says of an effect is what a caller of that row meets:
/// the effect the status makes unavailable is refused with its reason, while
/// the same effect on another row is invoked.
#[test]
fn a_row_status_refuses_what_a_caller_invokes_there() {
    let mut app = TestApp::<RowPublishingApp>::new().unwrap();
    let stop = |job: &str| JobEffect::Stop {
        job: job.into(),
        force: None,
    };
    let busy = app
        .effect_on("job:3:step", &stop("job:3:step"))
        .unwrap_err();
    assert!(
        matches!(
            &busy,
            tilo_actions::ApiError::Unavailable { code, reason }
                if code == "busy" && reason == "This job is busy and cannot be stopped now"
        ),
        "{busy}"
    );
    let phrased = app.effect_on("job:2", &stop("job:2")).unwrap_err();
    assert!(
        matches!(phrased, tilo_actions::ApiError::ConfirmationRequired),
        "the status asks the phrase its caller types: {phrased}"
    );
    app.effect_on("job:1", &stop("job:1"))
        .expect("the idle job is stopped");
    RAN.with(|ran| assert_eq!(ran.borrow().as_slice(), [stop("job:1")]));
}

#[test]
fn a_row_says_the_values_a_parameter_takes() {
    let bindings = tilo_resource::RowBindings::new()
        .bind(JobEffect::STOP, [("job", "job:7".into())])
        .choices(
            JobEffect::STOP,
            "force",
            ["now".to_owned(), "later".to_owned()],
        );
    let row = LiveRow::typed_with::<Job>("job:7", "Backup", &bindings).into_resource();
    let stop = row
        .actions
        .iter()
        .find(|action| action.id == "stop")
        .unwrap();
    let force = stop
        .params()
        .iter()
        .find(|param| param.name.as_ref() == "force")
        .expect("the parameter the row left asked");
    assert_eq!(
        force
            .choices
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["now", "later"],
        "the row's own state says what the parameter takes"
    );
}

#[test]
fn a_row_offers_an_action_another_resource_declares() {
    let declared = tilo_resource::Effect::bound(&JobEffect::Play {
        item: "song:1".into(),
    });
    let bindings = tilo_resource::RowBindings::new().spec(declared);
    let row = LiveRow::typed_with::<Job>("job:9", "Index", &bindings).into_resource();
    assert!(
        row.actions.iter().any(|action| action.id == "play"),
        "an action another resource declares is published on the row as it is"
    );
}

#[derive(tilo_resource::Row)]
#[row(kind = "test.late-page", label = "Page", effects = PageEffect)]
struct LatePage;

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "test.late-page.effect", catalogue = "tests/locales")]
pub enum PageEffect {
    /// Open an address on the page.
    #[effect(row, on = LatePage, kind = "read")]
    Go {
        #[param("The address")]
        url: String,
    },
}

thread_local! {
    /// Whether the window's view publishes its page yet.
    static PAGE_SHOWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static WENT: std::cell::RefCell<Vec<PageEffect>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A window whose page is a row its view publishes only once it has built it, and which opens a
/// link on that page.
struct LatePageApp;

impl ResourceApp for LatePageApp {
    type Resource = Jobs;
    const ID: &'static str = "so.tilo.test-late-page";
    const TITLE: &'static str = "Pages";

    fn catalog() -> Option<&'static tilo_app::i18n::EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn open_link(link: &str) -> Option<Vec<Invoke<Jobs>>> {
        Some(vec![
            Invoke::effect(&PageEffect::Go { url: link.into() }).on("late.page"),
        ])
    }

    fn view(cx: &mut ViewCx<Jobs>) -> Element {
        cx.effects(|effect: PageEffect, _| {
            WENT.with(|went| went.borrow_mut().push(effect));
            Ok(Effected::done())
        });
        cx.publishing(
            Publishes::new().live_rows(|| {
                PAGE_SHOWN
                    .get()
                    .then(|| {
                        LiveRow::typed_with::<LatePage>(
                            "late.page",
                            "Page",
                            &tilo_resource::RowBindings::new().offer(PageEffect::GO),
                        )
                    })
                    .into_iter()
                    .collect()
            }),
            Box::new(Column::new()),
        )
    }
}

/// A link handed to a window before its view publishes the page it opens on waits for the page,
/// as a press before an app is ready does, and opens there once it is published: it is never
/// dropped as an address nobody offers.
#[test]
fn a_link_handed_to_a_window_still_building_its_page_opens_once_the_page_is_there() {
    PAGE_SHOWN.set(false);
    let mut app = TestApp::<LatePageApp>::new().unwrap();
    app.call(
        tilo_actions::remote::APPLICATION_OPEN_LINK_ACTION,
        serde_json::json!({ "link": "late:one" }),
    )
    .unwrap();
    app.settle();
    assert!(
        WENT.with(|went| went.borrow().is_empty()),
        "no page to open it on yet"
    );
    assert_eq!(
        app.refused(),
        None,
        "it waits for the page rather than being refused"
    );
    PAGE_SHOWN.set(true);
    app.harness().relayout();
    app.settle();
    WENT.with(|went| {
        assert_eq!(
            went.borrow().as_slice(),
            [PageEffect::Go {
                url: "late:one".into()
            }]
        );
    });
}

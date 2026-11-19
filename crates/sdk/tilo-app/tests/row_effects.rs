//! Rows of a declared type: a plain struct as `RowType`, the effects offered
//! on every row of it (`#[effect(on = Row)]`, bound to the row's id), a
//! status that is about one row, and an item's presentation from state its
//! document does not hold.

use tilo_app::framework::{EffectStatus, Effected, Publishes};
use tilo_app::prelude::*;

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

struct RowPublishingApp;

thread_local! {
    static RAN: std::cell::RefCell<Vec<JobEffect>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl ResourceApp for RowPublishingApp {
    type Resource = Jobs;
    const ID: &'static str = "so.tilo.test-jobs";
    const TITLE: &'static str = "Jobs";

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
}

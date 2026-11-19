//! Rows of a declared type: a plain struct as `RowType`, the effects offered
//! on every row of it (`#[effect(on = Row)]`, bound to the row's id), a
//! status that is about one row, and an item's presentation from state its
//! document does not hold.

use tilo_app::framework::{EffectStatus, Effected, Publishes};
use tilo_app::prelude::*;

#[derive(tilo_resource::Row)]
#[row(kind = "test.jobs", label = "Job", effects = JobEffect)]
struct Job;

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
                    ]
                })
                .row_status(|id| {
                    (id == "job:2")
                        .then(|| {
                            EffectStatus::unavailable(
                                JobEffect::STOP,
                                "busy",
                                "This job is busy and cannot be stopped now",
                            )
                            .on(id)
                        })
                        .into_iter()
                        .collect()
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

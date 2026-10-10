//! Private, local registry operator UI. The registry's existing library owns
//! decisions and proof checks; the SDK owns controls, actions and draft history.

use std::path::{Path, PathBuf};

use registry_server::Storage;
use registry_server::applications::{self, NetworkProofs};
use registry_types::application::{ApplicationStatus, StoredApplication};
use ui::framework::EffectAnswer;
use ui::prelude::*;

mod packages;
pub use packages::PackageReview;

const CATALOG: &str = include_str!("../locales/en-US.ftl");
const PAGE_SIZE: usize = 20;

/// Only the operator's draft is document state, on a disposable private profile.
/// Applications, contact addresses and notes never enter that document.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Draft {
    /// The reviewer named in the eventual confirmed decision.
    pub reviewer: String,
    /// Feedback for the application currently being inspected.
    pub feedback: String,
}

#[resource(kind = "so.tilo.registry-review", label = "Registry review")]
impl Draft {}

/// The same local operator door for controls, accessibility and agents.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.registry-review.effects", device_only)]
pub enum Review {
    /// Refresh the pending publisher applications.
    #[effect(job, returns)]
    Refresh,
    /// Inspect a publisher's application, including its exact review identifier.
    #[effect(returns)]
    Inspect {
        #[param("Publisher public key")]
        publisher: String,
    },
    /// Approve the inspected namespaces after checking their ownership proofs again.
    #[effect(job, privileged)]
    Approve {
        #[param("Publisher public key")]
        publisher: String,
        #[param("Review id from Inspect")]
        review_id: String,
        #[param("Reviewer name")]
        reviewer: String,
    },
    /// Reject the inspected application and return feedback to its publisher.
    #[effect(job, privileged)]
    Reject {
        #[param("Publisher public key")]
        publisher: String,
        #[param("Review id from Inspect")]
        review_id: String,
        #[param("Reviewer name")]
        reviewer: String,
        #[param("Feedback")]
        reason: String,
    },
    /// Show another page of the pending queue.
    #[effect(returns)]
    Page {
        #[param("Zero-based page number")]
        page: usize,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Pane {
    applications: Vec<StoredApplication>,
    packages: Vec<registry_server::PendingSubmission>,
    package_selected: Option<packages::Snapshot>,
    package_page: usize,
    packages_loaded: bool,
    selected: Option<StoredApplication>,
    page: usize,
    busy: bool,
    error: Option<String>,
    loaded: bool,
}

fn queue_result(pane: &Pane) -> ui::serde_json::Value {
    let applications: Vec<_> = pane
        .applications
        .iter()
        .skip(pane.page.saturating_mul(PAGE_SIZE))
        .take(PAGE_SIZE)
        .map(|application| {
            ui::serde_json::json!({
                "publisher": application.application.pubkey_hex,
                "namespaces": application.namespaces(),
                "received_at_unix": application.received_at_unix,
            })
        })
        .collect();
    ui::serde_json::json!({"pending": pane.applications.len(), "page": pane.page, "applications": applications})
}

fn open_existing(directory: &Path) -> Result<Storage, String> {
    if !directory.join("handshake.toml").is_file()
        || !directory.join("publisher-applications").is_dir()
    {
        return Err("Choose an initialized registry data directory; this tool does not initialize registries.".into());
    }
    Storage::open(directory).map_err(|error| error.to_string())
}

fn pending(directory: &Path) -> Result<Vec<StoredApplication>, String> {
    let mut applications = open_existing(directory)?
        .load_publisher_applications()
        .map_err(|error| error.to_string())?;
    applications.retain(|application| application.status == ApplicationStatus::Pending);
    applications.sort_by(|a, b| {
        a.received_at_unix
            .cmp(&b.received_at_unix)
            .then(a.application.pubkey_hex.cmp(&b.application.pubkey_hex))
    });
    Ok(applications)
}

fn candidate(pane: &Pane, publisher: &str, review_id: &str) -> Result<StoredApplication, String> {
    let selected = pane
        .selected
        .as_ref()
        .ok_or("Inspect an application first.")?;
    if selected.application.pubkey_hex != publisher
        || selected.review_id().map_err(|error| error.to_string())? != review_id
    {
        return Err("The inspected application changed; inspect it again before deciding.".into());
    }
    Ok(selected.clone())
}

fn decide(directory: &Path, application: &StoredApplication, effect: Review) -> Result<(), String> {
    let storage = open_existing(directory)?;
    let now = tilo_clock::try_unix_now().ok_or("The system clock is before the Unix epoch.")?;
    match effect {
        Review::Approve { reviewer, .. } => {
            applications::approve_reviewed(
                &storage,
                application,
                &reviewer,
                now,
                &NetworkProofs::default(),
            )
            .map_err(|error| error.to_string())?;
        }
        Review::Reject {
            reviewer, reason, ..
        } => {
            applications::reject_reviewed(&storage, application, &reviewer, &reason, now)
                .map_err(|error| error.to_string())?;
        }
        _ => return Err("This operation is not a review decision.".into()),
    }
    Ok(())
}

fn install(
    cx: &ViewCx<Draft>,
    directory: PathBuf,
    pane: &LocalState<Pane>,
    navigation: &LocalState<NavPane>,
) {
    let pane = pane.clone();
    let navigation = navigation.clone();
    cx.effects::<Review>(move |effect, cx| {
        if pane.get().busy {
            return Err("A registry operation is running; wait for it to finish.".into());
        }
        match effect {
            Review::Page { page } => {
                let mut state = pane.get();
                let last = state.applications.len().saturating_sub(1) / PAGE_SIZE;
                if page > last {
                    return Err("There is no queue page with that number.".into());
                }
                state.page = page;
                let result = queue_result(&state);
                pane.set(state);
                Ok(Effected::value(result))
            }
            Review::Inspect { publisher } => {
                let mut state = pane.get();
                let application = state
                    .applications
                    .iter()
                    .find(|application| application.application.pubkey_hex == publisher)
                    .ok_or("Refresh the queue, then inspect a pending publisher.")?
                    .clone();
                let result = ui::serde_json::json!({
                    "review_id": application.review_id().map_err(|error| error.to_string())?,
                    "application": application,
                });
                let changed = state.selected.as_ref() != Some(&application);
                state.selected = Some(application);
                state.package_selected = None;
                state.error = None;
                if changed {
                    cx.submit(Draft::paths().feedback.replace(""));
                }
                pane.set(state);
                navigation.set(NavPane::Content);
                Ok(Effected::value(result))
            }
            effect => start(cx, &directory, &pane, &navigation, effect),
        }
    });
}

fn start(
    cx: &mut EffectCx<Draft>,
    directory: &Path,
    pane: &LocalState<Pane>,
    navigation: &LocalState<NavPane>,
    effect: Review,
) -> EffectAnswer {
    let selected = match &effect {
        Review::Refresh => None,
        Review::Approve {
            publisher,
            review_id,
            reviewer,
        }
        | Review::Reject {
            publisher,
            review_id,
            reviewer,
            ..
        } => {
            if reviewer.trim().is_empty() {
                return Err("Name the reviewer before deciding.".into());
            }
            if let Review::Reject { reason, .. } = &effect
                && reason.trim().is_empty()
            {
                return Err("Write feedback before rejecting an application.".into());
            }
            Some(candidate(&pane.get(), publisher, review_id)?)
        }
        _ => return Err("This operation is not a registry job.".into()),
    };
    let mut state = pane.get();
    state.busy = true;
    state.error = None;
    pane.set(state);
    let directory = directory.to_owned();
    let pane = pane.clone();
    let navigation = navigation.clone();
    Ok(cx.thread(
        move |_| {
            if let Some(selected) = selected {
                decide(&directory, &selected, effect)?;
            }
            pending(&directory)
        },
        move |(), result, _| {
            let mut state = pane.get();
            state.busy = false;
            match result {
                Ok(applications) => {
                    state.applications = applications;
                    state.selected = None;
                    state.page = 0;
                    state.loaded = true;
                    let result = queue_result(&state);
                    pane.set(state);
                    navigation.set(NavPane::Overview);
                    Ok(Effected::value(result))
                }
                Err(error) => {
                    state.error = Some(error.clone());
                    pane.set(state);
                    Err(error)
                }
            }
        },
    ))
}

fn verbatim(text: impl Into<String>) -> Text {
    Text::new("review-verbatim").with_arg("text", text.into())
}

fn read_field(label: &str, value: &str) -> Element {
    Box::new(
        Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .push(Box::new(Text::new(label).with_role(TextRole::Label)))
            .push(Box::new(verbatim(value))),
    )
}

fn detail(cx: &ViewCx<Draft>, application: &StoredApplication) -> Element {
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_id("review-detail")
        .push(Box::new(
            verbatim(application.namespaces().join(", ")).with_role(TextRole::Heading),
        ))
        .push(Box::new(
            verbatim(application.application.pubkey_hex.clone())
                .with_role(TextRole::Code)
                .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph)
                .with_id("publisher-key"),
        ))
        .push(read_field(
            "review-contact",
            &application.application.contact,
        ))
        .push(read_field("review-note", &application.application.note));
    for proof in &application.verified {
        column = column.push(Box::new(
            Text::new("review-proof")
                .with_arg("namespace", proof.namespace.clone())
                .with_arg("location", proof.location.clone())
                .with_arg("checked", proof.checked_at_unix.to_string()),
        ));
    }
    let reviewer = cx.input(Draft::paths().reviewer).with_id("reviewer-input");
    let feedback = cx.input(Draft::paths().feedback).with_id("feedback-input");
    let application = application.clone();
    let draft = cx.value(Clone::clone);
    column = column
        .push(Box::new(labeled_field("review-reviewer", reviewer)))
        .push(Box::new(labeled_field("review-feedback", feedback)))
        .push(cx.switch(&draft, move |cx, draft| {
            let Ok(review_id) = application.review_id() else {
                return Box::new(Text::new("review-snapshot-error"));
            };
            let actions = cx.actions();
            Box::new(ui::column![
                actions.bind_effect_invocation(
                    Button::new("approve-application", "review-approve"),
                    &Review::Approve {
                        publisher: application.application.pubkey_hex.clone(),
                        review_id: review_id.clone(),
                        reviewer: draft.reviewer.clone(),
                    }
                ),
                actions.bind_effect_invocation(
                    Button::new("reject-application", "review-reject"),
                    &Review::Reject {
                        publisher: application.application.pubkey_hex.clone(),
                        review_id,
                        reviewer: draft.reviewer.clone(),
                        reason: draft.feedback.clone(),
                    }
                ),
            ])
        }));
    Box::new(column)
}

fn queue(cx: &mut ViewCx<Draft>, pane: &Pane) -> Element {
    let actions = cx.actions();
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_id("review-queue")
        .push(Box::new(
            Text::new("review-title").with_role(TextRole::Heading),
        ))
        .push(Box::new(Text::new("review-help")))
        .push(Box::new(actions.bind_effect_invocation(
            Button::new("refresh-applications", "review-refresh"),
            &Review::Refresh,
        )));
    if pane.busy {
        column = column.push(Box::new(Spinner::new()));
    }
    if let Some(error) = &pane.error {
        column = column.push(Box::new(Banner::error(error.clone())));
    }
    if !pane.loaded {
        column = column.push(Box::new(Text::new("review-start")));
    } else if pane.applications.is_empty() {
        column = column.push(Box::new(Text::new("review-empty").with_id("empty-queue")));
    }
    for application in pane
        .applications
        .iter()
        .skip(pane.page.saturating_mul(PAGE_SIZE))
        .take(PAGE_SIZE)
    {
        column = column.push(Box::new(verbatim(application.namespaces().join(", "))));
        column = column.push(Box::new(actions.bind_effect_invocation(
            Button::new(
                &format!("inspect-{}", application.application.pubkey_hex),
                "review-inspect",
            ),
            &Review::Inspect {
                publisher: application.application.pubkey_hex.clone(),
            },
        )));
    }
    if pane.page > 0 {
        column = column.push(Box::new(actions.bind_effect_invocation(
            Button::new("previous-page", "review-previous"),
            &Review::Page {
                page: pane.page.saturating_sub(1),
            },
        )));
    }
    let next = pane.page.saturating_add(1);
    if next.saturating_mul(PAGE_SIZE) < pane.applications.len() {
        column = column.push(Box::new(actions.bind_effect_invocation(
            Button::new("next-page", "review-next"),
            &Review::Page { page: next },
        )));
    }
    column = column.push(packages::queue(cx, pane));
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(column)),
    )
}

/// The native operator window. No registry I/O runs while constructing a view.
pub struct App;

impl ResourceApp for App {
    type Resource = Draft;
    const ID: &'static str = "so.tilo.registry-review";
    const TITLE: &'static str = "Registry review";
    const BEHAVIOUR: ComponentBehaviour = ComponentBehaviour {
        capture_protected: true,
        ..ComponentBehaviour::DEFAULT
    };
    const PRIVATE_WINDOWS: bool = true;
    const OPTIONS: &'static [&'static str] = &["--registry-dir"];

    fn view(cx: &mut ViewCx<Draft>) -> Element {
        if let Err(error) = ui::i18n::I18nManager::install_system("en-US", &[("en-US", CATALOG)]) {
            return Box::new(Banner::error(error.to_string()));
        }
        if cx.is_past() {
            return Box::new(
                Column::new()
                    .with_id("review-draft-history")
                    .push(Box::new(
                        Text::new("review-draft-title").with_role(TextRole::Heading),
                    ))
                    .push(Box::new(
                        Text::new("review-reviewer").with_role(TextRole::Label),
                    ))
                    .push(Box::new(cx.text(|draft| draft.reviewer.clone())))
                    .push(Box::new(
                        Text::new("review-feedback").with_role(TextRole::Label),
                    ))
                    .push(Box::new(cx.text(|draft| draft.feedback.clone()))),
            );
        }
        if !cx.is_private() || cx.launch_option("--registry-dir").is_none() {
            return Box::new(Text::new("review-private-required").with_id("review-locked"));
        }
        let directory = PathBuf::from(cx.launch_option("--registry-dir").unwrap_or_default());
        let pane = cx.local(Pane::default());
        let navigation = cx.local(NavPane::Overview);
        install(cx, directory.clone(), &pane, &navigation);
        packages::install(cx, directory, &pane, &navigation);
        let admin = cx.govern_state();
        cx.switch(&admin, move |cx, allowed| {
            if !*allowed {
                return Box::new(Text::new("review-owner-required").with_id("review-locked"));
            }
            let overview = cx.switch(&pane, queue);
            let content = cx.switch(&pane, |cx, pane| {
                let detail = if let Some(snapshot) = &pane.package_selected {
                    packages::detail(cx, snapshot)
                } else if let Some(application) = &pane.selected {
                    detail(cx, application)
                } else {
                    return Box::new(Text::new("review-select"));
                };
                let mut body = Column::new().with_intrinsic_height();
                if let Some(error) = &pane.error {
                    body = body.push(Box::new(Banner::error(error.clone())));
                }
                if pane.busy {
                    body = body.push(Box::new(Spinner::new()));
                }
                body = body.push(detail);
                Box::new(
                    Scrollable::new_vertical()
                        .fill_parent_height()
                        .with_child(Box::new(body)),
                )
            });
            Box::new(
                NavShell::new()
                    .with_pane_state(&navigation)
                    .overview_header(
                        ui::i18n::I18nManager::format("review-window-title", &[]),
                        Vec::new(),
                    )
                    .overview(overview)
                    .content_header(
                        ui::i18n::I18nManager::format("review-selected-title", &[]),
                        Vec::new(),
                    )
                    .content(content),
            )
        })
    }
}

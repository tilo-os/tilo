//! Genuine registered History refresh checks; constructors and full refreshes stay outside probes.
use super::*;
use crate::application::{AppComponent, LaunchTarget, RestoreInput};
use crate::application_test_support::{
    APP, ProbeHarness, runner_permissions, runner_probe_definition,
};
use crate::bound_dispatch::CallbackBinding;
use crate::history_mode::owner::HistoryOwner;
use crate::history_mode::{HistoryDefinition, HistoryScopeDefinition};
use automerge::{ROOT, ReadDoc, transaction::Transactable};

static DEFINITION: HistoryDefinition = HistoryDefinition {
    app_id: APP,
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Refresh probe",
        presentation: HistoryPresentation::Domain {
            create: crate::history_mode::host_tests::preview,
        },
    }],
};

struct BoundFixture {
    component: AppComponent,
    binding: CallbackBinding,
    parts: Option<std::rc::Rc<dyn crate::history_mode::HistoryParts>>,
    _host: ProbeHarness,
}
impl BoundFixture {
    fn new() -> Self {
        let host = ProbeHarness::new();
        let definition = runner_probe_definition();
        let context = host
            .prepare_with_permissions(definition, 940, &LaunchTarget::New, runner_permissions())
            .unwrap();
        let component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let binding = component.callback_binding();
        binding
            .records
            .with_history(&binding.checkout.id, |document, _| {
                document.put(ROOT, "body", "committed owned body").unwrap();
                document.commit();
            })
            .unwrap();
        Self {
            component,
            binding,
            parts: None,
            _host: host,
        }
    }
    fn history(&self, outer: &AutoCommit) -> HistoryHost {
        let source = HistorySourceRegistration::owned(
            "document",
            APP,
            HistoryOwner::component(self.binding.clone()),
        )
        .unwrap();
        let source = match &self.parts {
            Some(parts) => source.with_parts(std::rc::Rc::clone(parts)),
            None => source,
        };
        HistoryHost::new(
            HistoryRegistration::new(&DEFINITION, vec![source]).unwrap(),
            outer,
        )
        .unwrap()
    }
    fn close(&mut self) {
        let report = self.component.close();
        assert!(
            report.resource_error.is_none(),
            "{:?}",
            report.resource_error
        );
        assert!(report.view_error.is_none(), "{:?}", report.view_error);
        assert!(
            report.teardown_error.is_none(),
            "{:?}",
            report.teardown_error
        );
    }
}
fn outer() -> AutoCommit {
    let mut document = AutoCommit::new();
    document.put(ROOT, "body", "unrelated outer body").unwrap();
    document.commit();
    document
}

struct Parts(std::rc::Rc<std::cell::RefCell<Vec<AutoCommit>>>);
impl Parts {
    fn part(index: usize) -> crate::history_mode::HistoryPart {
        crate::history_mode::HistoryPart {
            chain: "lines".into(),
            index: u64::try_from(index).unwrap(),
        }
    }
    fn body_at(
        &self,
        part: &crate::history_mode::HistoryPart,
        heads: &[automerge::ChangeHash],
    ) -> String {
        let index = usize::try_from(part.index).unwrap();
        self.0.borrow()[index]
            .clone()
            .fork_at(heads)
            .unwrap()
            .get(ROOT, "body")
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok())
            .unwrap_or_default()
    }
}
impl crate::history_mode::HistoryParts for Parts {
    fn heads(&self) -> Vec<automerge::ChangeHash> {
        self.0
            .borrow()
            .iter()
            .flat_map(automerge::transaction::Transactable::base_heads)
            .collect()
    }
    fn documents(&self) -> Vec<(crate::history_mode::HistoryPart, AutoCommit)> {
        self.0
            .borrow()
            .iter()
            .enumerate()
            .map(|(index, part)| (Self::part(index), part.clone()))
            .collect()
    }
    fn preview(
        &self,
        version: crate::history_mode::PartVersion<'_>,
    ) -> Result<Box<dyn crate::history_mode::HistoryPreview>, String> {
        let mut shown = AutoCommit::new();
        shown
            .put(ROOT, "body", self.body_at(version.part, version.heads))
            .unwrap();
        crate::history_mode::host_tests::preview(crate::history_mode::PreviewInput {
            snapshot: crate::history_mode::HistorySnapshot {
                bytes: std::sync::Arc::from(shown.save()),
                heads: version.heads.to_vec(),
                side: PreviewSide::After,
                selection: Vec::new(),
            },
            initial_view: version.view,
            assets: crate::history_mode::FrozenPreviewAssets::default(),
        })
    }
    fn restore(
        &self,
        part: &crate::history_mode::HistoryPart,
        heads: &[automerge::ChangeHash],
        _: &crate::permissions::MemberId,
    ) -> Result<(), String> {
        let body = self.body_at(part, heads);
        let index = usize::try_from(part.index).unwrap();
        let mut parts = self.0.borrow_mut();
        parts[index].put(ROOT, "body", body).unwrap();
        parts[index].commit();
        Ok(())
    }
}

/// A change made at `seconds` past the epoch.
fn typed_at(document: &mut AutoCommit, seconds: i64) {
    document
        .put(ROOT, "body", format!("typed at {seconds}"))
        .unwrap();
    document.commit_with(automerge::transaction::CommitOptions::default().with_time(seconds));
}

#[test]
fn history_previews_and_restores_the_parts_beside_the_document() {
    let mut fixture = BoundFixture::new();
    let outer = outer();
    let mut part = AutoCommit::new();
    for second in 1..=3 {
        typed_at(&mut part, second);
    }
    // The window's History lists the part's changes.
    let parts = std::rc::Rc::new(std::cell::RefCell::new(vec![part]));
    fixture.parts = Some(std::rc::Rc::new(Parts(std::rc::Rc::clone(&parts))));
    let mut history = fixture.history(&outer);
    assert!(history.request_open());
    history.refresh(&outer);
    let listed = history.timeline.entries().to_vec();
    assert!(!listed.is_empty(), "the part's changes are listed");

    // Choosing a part's earliest change shows the part as that change left it.
    let index = listed
        .iter()
        .position(|entry| {
            entry.timestamp == std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1)
        })
        .expect("the part's first change is listed with the window's own");
    let chosen = listed[index].clone();
    history.timeline.set_selection(Some((index, index)));
    history.refresh(&outer);
    assert_eq!(history.timeline_selection(), [chosen.id]);
    let part = Parts::part(0);
    assert_eq!(
        history
            .controller
            .as_ref()
            .and_then(crate::history_mode::HistoryController::selected_part),
        Some(&part)
    );
    let shown_body = {
        let heads = history
            .controller
            .as_ref()
            .unwrap()
            .snapshot()
            .unwrap()
            .heads()
            .to_vec();
        Parts(std::rc::Rc::clone(&parts)).body_at(&part, &heads)
    };
    assert_eq!(shown_body, "typed at 1");
    let shown = history
        .after
        .preview
        .as_ref()
        .expect("the part's version is previewed")
        .accessibility()
        .description;
    assert!(shown.contains(&shown_body), "{shown}");
    assert!(!history.needs_refresh(&outer, 900.0));

    // Restoring it writes the part back as it was, as a new change of the part.
    let changes = parts.borrow()[0].clone().get_changes(&[]).len();
    let review = history
        .controller
        .as_ref()
        .unwrap()
        .review_restore()
        .unwrap();
    history
        .controller
        .as_ref()
        .unwrap()
        .restore_part(&review, &crate::permissions::MemberId::person("ada"))
        .unwrap();
    let mut restored = parts.borrow()[0].clone();
    assert_eq!(restored.get_changes(&[]).len(), changes + 1);
    assert_eq!(
        restored
            .get(ROOT, "body")
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok()),
        Some(shown_body)
    );
    // A review of a version the part moved past since is refused.
    assert_eq!(
        history
            .controller
            .as_ref()
            .unwrap()
            .restore_part(&review, &crate::permissions::MemberId::person("ada")),
        Err(crate::history_mode::HistoryError::StaleLiveHeads)
    );
    // The shown version stays behind the part.
    history.refresh(&outer);
    assert!(
        history
            .controller
            .as_ref()
            .is_some_and(crate::history_mode::HistoryController::has_live_updates)
    );
    fixture.close();
}

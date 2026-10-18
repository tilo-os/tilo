//! Framework item presentation uses the item's bound opener and the live
//! owner dispatch, without moving another participant's private view.

use tilo_app::prelude::*;

#[derive(Model, Default, Clone, PartialEq)]
pub struct Page {
    #[model(label)]
    pub title: String,
}

#[derive(Model, Default, Clone, PartialEq)]
pub struct Pages {
    pub pages: Collection<Page>,
}

#[resource(kind = "test.navigation", label = "Pages")]
impl Pages {
    /// Add a page.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, title: String) {
        self.pages.push(cx.id(), Page { title });
    }
}

thread_local! {
    // Bound from the actual owner's fresh publication before navigating.
    static OPEN_BINDING: std::cell::RefCell<Option<tilo_actions::ActionBinding<()>>> =
        const { std::cell::RefCell::new(None) };
}

struct PagesApp;

impl ResourceApp for PagesApp {
    type Resource = Pages;
    const ID: &'static str = "so.tilo.test-navigation";
    const TITLE: &'static str = "Pages";

    fn view(cx: &mut ViewCx<Pages>) -> Element {
        let selected = cx.local(None::<Id>);
        let opening = selected.clone();
        cx.on_open_item(move |id| {
            opening.set(Some(id.clone()));
            Ok(())
        });
        cx.switch(&selected, |cx, selected| {
            let Some(id) = selected else {
                return Box::new(Text::new("No page open").with_id("no-page"));
            };
            let id = id.clone();
            let title = cx.read(|pages| {
                pages
                    .and_then(|pages| pages.pages.get(&id))
                    .map(|page| page.title.clone())
                    .unwrap_or_default()
            });
            let mut content = Column::new().push(Box::new(
                Text::new(&title).with_id(format!("shown-page-{id}")),
            ));
            if let Some(binding) = OPEN_BINDING.with_borrow(Clone::clone) {
                content = content.push(Box::new(
                    Button::new("show-page", "Show page").on_action(binding),
                ));
            }
            Box::new(content)
        })
    }
}

struct UnpresentedPagesApp;

impl ResourceApp for UnpresentedPagesApp {
    type Resource = Pages;
    const ID: &'static str = "so.tilo.test-unpresented-pages";
    const TITLE: &'static str = "Pages";

    fn view(_: &mut ViewCx<Pages>) -> Element {
        Box::new(Text::new("Pages"))
    }
}

fn item(
    app: &mut TestApp<PagesApp>,
    title: &str,
) -> Result<tilo_actions::Resource, Box<dyn std::error::Error>> {
    app.invoke(Pages::ADD.with(title))?;
    app.harness()
        .resources()
        .into_iter()
        .find(|resource| resource.kind == Pages::KIND)
        .ok_or("the owner did not publish its Pages resource")?
        .children
        .into_iter()
        .find(|resource| resource.label == title)
        .ok_or_else(|| "the owner did not publish the added page".into())
}

#[test]
fn framework_item_view_opens_exact_owner_item_in_live_ui() {
    OPEN_BINDING.with_borrow_mut(|binding| *binding = None);
    let mut app = TestApp::<PagesApp>::new().unwrap();
    let first = item(&mut app, "First page").unwrap();
    let second = item(&mut app, "Second page").unwrap();
    assert_eq!(first.views.len(), 1);
    let view = &first.views[0];
    assert_eq!(view.id, "item");
    assert!(view.primary);
    assert_eq!(view.open_action, "open");
    assert!(view.validate(&first).is_empty());
    // The view is the same for every item: the item's label is the
    // resource's, so the published `open` is one shared catalogue entry.
    assert_eq!(second.views, first.views);
    assert_ne!(view.label, first.label);
    let (_, first_id) = tilo_resource::items::item_of_resource::<Pages>(first.id.as_str()).unwrap();
    let (_, second_id) =
        tilo_resource::items::item_of_resource::<Pages>(second.id.as_str()).unwrap();
    assert!(app.harness().targets().contains_key("no-page"));
    app.call_on(first.id.as_str(), &view.open_action, serde_json::json!({}))
        .unwrap();
    app.settle();
    let targets = app.harness().targets();
    let shown = targets.get(&format!("shown-page-{first_id}")).unwrap();
    assert!(shown.width() > 0.0 && shown.height() > 0.0);
    assert!(!targets.contains_key("no-page"));
    assert!(!targets.contains_key(&format!("shown-page-{second_id}")));
    OPEN_BINDING.with_borrow_mut(|binding| {
        *binding = Some(tilo_actions::ActionBinding::unit_for(&second, "open").unwrap());
    });
    app.call_on(second.id.as_str(), "open", serde_json::json!({}))
        .unwrap();
    app.settle();
    let targets = app.harness().targets();
    assert!(targets.contains_key(&format!("shown-page-{second_id}")));
    assert!(!targets.contains_key(&format!("shown-page-{first_id}")));
    // Exact Widget metadata links this freshly published item address to
    // its control. Presence painting is narration, not another invocation.
    let interaction = tilo_actions::SemanticInteraction {
        paint_origin: None,
        version: tilo_actions::INTERACTION_VERSION,
        invocation: tilo_actions::InvocationId::new(),
        sequence: 1,
        actor: tilo_actions::InteractionActor {
            id: "page-agent".into(),
            kind: tilo_actions::ActorKind::Agent,
            for_person: Some("page-owner".into()),
        },
        phase: tilo_actions::InteractionPhase::Applying,
        action: tilo_actions::InteractionAction {
            id: "open".into(),
            label: "Open".into(),
            app: None,
        },
        resource: tilo_actions::ActionResource {
            id: second.id.0.clone(),
            kind: second.kind.clone(),
            label: second.label.clone(),
        },
        anchor: tilo_actions::InteractionAnchor::Resource,
        group: None,
    };
    let locus = app.harness().targets()["show-page"];
    assert_eq!(app.harness().semantic_locus(&interaction), Some(locus));
    app.harness()
        .update_presence(|presence| presence.apply_interaction(interaction.clone()).unwrap());
    assert!(app.harness().poll_frame().unwrap());
    assert_eq!(app.harness().semantic_locus(&interaction), Some(locus));
    let mut unpresented = interaction;
    unpresented.resource.id = first.id.0.clone();
    assert_eq!(app.harness().semantic_locus(&unpresented), None);
    assert!(
        app.harness()
            .targets()
            .contains_key(&format!("shown-page-{second_id}"))
    );
    OPEN_BINDING.with_borrow_mut(|binding| *binding = None);
}

#[test]
fn framework_does_not_advertise_item_presentation_without_an_opener() {
    let mut app = TestApp::<UnpresentedPagesApp>::new().unwrap();
    app.invoke(Pages::ADD.with("Unpresented page")).unwrap();
    let page = app
        .harness()
        .resources()
        .into_iter()
        .find(|resource| resource.kind == Pages::KIND)
        .unwrap()
        .children
        .into_iter()
        .find(|resource| resource.label == "Unpresented page")
        .unwrap();
    assert!(page.views.is_empty());
    assert!(!page.actions.iter().any(|action| action.id == "open"));
}

#[test]
fn framework_item_view_refuses_foreign_private_navigation_before_opening() {
    OPEN_BINDING.with_borrow_mut(|binding| *binding = None);
    let mut app = TestApp::<PagesApp>::new().unwrap();
    let page = item(&mut app, "Private page").unwrap();
    let invoker = tilo_actions::ActionInvoker::actor("page-agent")
        .with_participant(tilo_actions::ParticipantId::new("page-agent").unwrap());
    let error = app
        .call_by(invoker, page.id.as_str(), "open", serde_json::json!({}))
        .unwrap_err();
    assert!(
        matches!(error, tilo_actions::DoorError::Unavailable { ref code, .. } if code == "private_view")
    );
    app.settle();
    assert!(app.harness().targets().contains_key("no-page"));
}

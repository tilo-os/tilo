use super::*;
use crate::application::{ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget};
use crate::application_view::InstanceViewStore;
use crate::bound_dispatch::CallbackBinding;
use crate::permissions::{PermissionContext, UserId};
use crate::record::{RecordKind, RecordOwners};
use crate::widget::{IconDescriptor, Text};
use std::cell::RefCell;
struct BoundRoot {
    parts: Parts,
    binding: CallbackBinding,
}
impl Widget for BoundRoot {
    retained_parts!("BoundRoot");
    fn callback_binding(&self) -> Option<CallbackBinding> {
        Some(self.binding.clone())
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.parts.build(Style::default(), tree, theme, map);
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.parts.draw(scene, transform, text, theme, presence);
    }
}
#[test]
fn owner_execution_scope_spans_probes_and_closed_or_cross_record_policies_never_run() {
    let temp = tempfile::tempdir().unwrap();
    let mut owners = RecordOwners::new(temp.path().into());
    let records = owners.open("dev.tilo.manual-test", "A").unwrap();
    let views = Rc::new(InstanceViewStore::new(temp.path().into(), "D", "A").unwrap());
    let definition = ApplicationDefinition {
        app_id: "dev.tilo.manual-test".into(),
        title: "Manual".into(),
        icon: IconDescriptor::Glyph("M"),
        record_kind: RecordKind::new("manual"),
        required: vec![],
        optional: vec![],
        opens: &[],
        room_record: false,
        create: |_, _, _| unreachable!(),
    };
    let prepare = |instance| {
        let mut context = HostContext::prepare(
            &definition,
            HostContextInputs {
                records: records.clone(),
                views: views.clone(),
                device_id: "D".into(),
                instance_id: instance,
                permissions: Rc::new(RefCell::new(PermissionContext::permissive())),
                author: UserId("tester".into()),
                action_context: tilo_actions::ActionContext::default(),
            },
            &LaunchTarget::New,
        )
        .unwrap();
        context.initialize_new_record(|_| Ok(())).unwrap();
        context
    };
    let context = prepare(1);
    let other = prepare(2);
    let binding = context.callback_binding();
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    let handles = Rc::new(RefCell::new(Vec::new()));
    let captured = handles.clone();
    let manual = Manual::new(move |ctx| {
        observed.set(observed.get() + 1);
        captured.borrow_mut().push(crate::wake::WakeHandle::new());
        let measured = ctx.measure(
            0,
            Size {
                width: MeasureConstraint::Exact(180.0),
                height: MeasureConstraint::Intrinsic,
            },
        )?;
        captured.borrow_mut().push(crate::wake::WakeHandle::new());
        ctx.place(
            0,
            Rect::new(0.0, 0.0, 180.0, f64::from(measured.size.height)),
        )?;
        Ok(Size {
            width: 200.0,
            height: 100.0,
        })
    })
    .child(Box::new(Text::new("Bound subtree measurement")));
    let mut widget = BoundRoot {
        parts: Parts::new(vec![Box::new(manual)]),
        binding: binding.clone(),
    };
    let mut tree = TaffyTree::new();
    let mut nodes = HashMap::new();
    let root = widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
    let mut bindings = BoundNodeMap::new();
    unsafe {
        bindings.rebuild(&widget, &tree, root, &nodes);
    }
    let mut text = crate::font_catalog::fixture_renderer();
    let available = Size {
        width: AvailableSpace::Definite(300.0),
        height: AvailableSpace::Definite(200.0),
    };
    let mut solve = || unsafe {
        super::super::compute_widget_layout(
            &mut tree, root, available, &nodes, &bindings, &mut text,
        )
        .unwrap()
    };
    solve();
    let ran = calls.get();
    assert!(ran > 0);
    other
        .callback_binding()
        .with_document(&mut automerge::AutoCommit::new(), |_, _| solve())
        .unwrap();
    assert_eq!(calls.get(), ran, "cross-record execution ran a policy");
    binding.closing.set(true);
    solve();
    assert_eq!(calls.get(), ran, "closing owner ran a policy");
    binding.closing.set(false);
    assert!(
        handles
            .borrow()
            .iter()
            .all(crate::wake::WakeHandle::is_active)
    );
    drop(context);
    assert!(
        handles.borrow().iter().all(|handle| !handle.is_active()),
        "policy execution lost the owner's wake scope before or after probing"
    );
    solve();
    assert_eq!(calls.get(), ran, "closed lifetime ran a policy");
}

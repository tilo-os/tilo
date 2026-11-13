//! [`SegChoice`] — one choice of a few, over published actions.

use ui::widget::toggle_group::Segment;
use ui::widget::{ToggleGroup, Widget};

use crate::setup::destination::control::{Bound, Control};

pub fn seg_choice(
    labels: Vec<String>,
    controls: Vec<Control>,
    selected: usize,
    test_id: &str,
) -> Bound {
    let selected = selected.min(labels.len().saturating_sub(1));
    let segments = labels.into_iter().map(Segment::new).collect();
    let listed = controls.first().cloned();
    let choice = ToggleGroup::new(segments, selected)
        .with_id(test_id)
        .fill()
        .on_view(test_id, move |index| {
            if let Some(control) = controls.get(index) {
                control.fire();
            }
        });
    Bound::new(
        Box::new(choice) as Box<dyn Widget>,
        listed.as_ref().map(|control| (test_id.to_owned(), control)),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use automerge::{AutoCommit, ROOT, ReadDoc};
    use taffy::TaffyTree;
    use ui::synthetic_input::named_key;
    use ui::theme::Theme;
    use winit::keyboard::NamedKey;

    use super::*;
    use crate::setup::destination::control::{Press, Presses};
    use crate::setup::destination::effects::DestinationEffect;

    #[test]
    fn arrowing_fires_the_neighbouring_segments_action() {
        let presses = Presses::default();
        let controls = ["btrfs", "ext4", "xfs"]
            .iter()
            .map(|fs| {
                presses.control(Press::new(DestinationEffect::SetFilesystem {
                    filesystem: (*fs).to_owned(),
                }))
            })
            .collect();
        let mut choice = seg_choice(
            ["btrfs", "ext4", "xfs"].map(str::to_owned).to_vec(),
            controls,
            1,
            "filesystem_choice",
        );
        assert_eq!(choice.test_id(), Some("filesystem_choice"));
        let bindings = ui::widget::controls::control_action_bindings(&choice);
        assert_eq!(bindings.len(), 1, "one action, one inventory entry");
        assert_eq!(bindings[0].control_id, "filesystem_choice");
        assert!(matches!(
            bindings[0].kind,
            ui::widget::ControlBindingKind::Action(_)
        ));
        choice.set_focus(true);
        let mut doc = AutoCommit::new();
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let node = choice.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(360.0),
                    height: taffy::AvailableSpace::MaxContent,
                },
            )
            .unwrap();
        choice.apply_layout(&taffy, node, vello::kurbo::Point::ORIGIN);
        let mut keys = ui::testing::Dispatch::new()
            .doc(&mut doc)
            .layout(&taffy, node);
        keys.send(&mut choice, &named_key(NamedKey::ArrowRight));
        let mut tree: Box<dyn Widget> = Box::new(choice);
        ui::widget::controls::start_pending_control_invocations(
            tree.as_mut(),
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
        );
        let press = presses.take().expect("a segment action");
        assert!(
            press.effect
                == DestinationEffect::SetFilesystem {
                    filesystem: "xfs".to_owned()
                }
        );
        assert_eq!(doc.length(ROOT), 0, "a segment never writes the document");
    }
}

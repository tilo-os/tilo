//! An open menu's rows as the host reaches them: each row a person can
//! choose now is published as the `invoke` action of control
//! `menu.<row>` (`menu.<row>.<nested row>` for a row of a nested menu), so
//! every app's menus are agent-addressable without a line of their own.
//!
//! An agent's or a script's choice is the person's: the menu asks the host
//! to press the row where it is drawn ([`press_where_drawn`]), so the row's
//! own closure runs and the owner reads the choice from
//! [`ContextMenu::take_selection`] as it does after a click. A nested row
//! opens its menu first. A row that runs a published action
//! ([`MenuItem::for_action`]) is that action, so no control is published
//! for it. The rows the menu adds itself ("Tell an agent…") are not
//! published.
//!
//! [`press_where_drawn`]: crate::widget::activation::press_where_drawn

use automerge::ObjId;

use super::{ContextMenu, MenuItem};
use crate::widget::activation::{NATIVE_INVOKE, NativeControl, invoked_control};

/// The prefix of every published row's control id.
const CONTROL: &str = "menu";

/// A row a person can choose now.
struct Row {
    /// Its control id: `menu.<row>`, or `menu.<row>.<nested row>`.
    id: String,
    /// What it reads as; a nested row's after its parent's.
    label: String,
    /// The indices that reach it, a nested row's parent first.
    path: Vec<usize>,
    /// The published action it runs, when it is one.
    address: Option<tilo_actions::ActionAddress>,
}

impl ContextMenu {
    /// The owner's rows a person can choose now, with their index.
    fn choosable_rows(&self) -> impl Iterator<Item = (usize, &MenuItem)> + '_ {
        let own = self.own_rows.unwrap_or(self.items.len());
        self.items
            .iter()
            .enumerate()
            .take(own)
            .filter(|(_, item)| item.is_enabled())
    }

    /// Every row a person can choose now, nested rows under their parent.
    fn published_rows(&self) -> Vec<Row> {
        if !self.open {
            return Vec::new();
        }
        let mut rows = Vec::new();
        for (index, item) in self.choosable_rows() {
            let Some(label) = item.label() else {
                continue;
            };
            match item.submenu_items() {
                Some(nested) => {
                    for (inner, row) in nested.iter().enumerate() {
                        if row.is_enabled()
                            && row.submenu_items().is_none()
                            && let Some(inner_label) = row.label()
                        {
                            rows.push(Row {
                                id: format!("{CONTROL}.{index}.{inner}"),
                                label: format!("{label}: {inner_label}"),
                                path: vec![index, inner],
                                address: row.action_address().cloned(),
                            });
                        }
                    }
                }
                None => rows.push(Row {
                    id: format!("{CONTROL}.{index}"),
                    label: label.to_owned(),
                    path: vec![index],
                    address: item.action_address().cloned(),
                }),
            }
        }
        rows
    }

    /// Choose the row at `path` the way a person does: bring it into view,
    /// open the nested menu it is in, and have the host press it where it
    /// is drawn. Returns whether the row is there to press.
    fn press_row(&mut self, path: &[usize]) -> bool {
        match *path {
            [index] => {
                self.focus_item(index);
                let Some(rect) = self.item_rect(index) else {
                    return false;
                };
                crate::widget::activation::press_where_drawn(rect.center());
                true
            }
            [index, inner] => {
                self.focus_item(index);
                self.open_submenu(index, false);
                let Some((_, menu)) = self.submenu.as_mut() else {
                    return false;
                };
                menu.focus_item(inner);
                let Some(rect) = menu.item_rect(inner) else {
                    return false;
                };
                crate::widget::activation::press_where_drawn(rect.center());
                true
            }
            _ => false,
        }
    }

    fn start_row(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let Some(control) = invoked_control(op)? else {
            return Ok(None);
        };
        let Some(path) = self
            .published_rows()
            .into_iter()
            .find(|row| row.address.is_none() && row.id == control)
            .map(|row| row.path)
        else {
            return Ok(None);
        };
        if !self.press_row(&path) {
            return Err(format!("menu row {control} is not shown"));
        }
        Ok(Some(crate::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

impl crate::widget::ResourceOperations for ContextMenu {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        if self.open { &[NATIVE_INVOKE] } else { &[] }
    }

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        _doc: &mut crate::widget::Edits<'_>,
        _path: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        self.start_row(op)
    }
}

impl crate::resources::PublishesResources for ContextMenu {
    /// Each row is an effect: what a menu row does is the owner's to say,
    /// and the host admits an effect under the invoker's grants.
    fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        self.published_rows()
            .into_iter()
            .filter(|row| row.address.is_none())
            .map(|row| {
                NativeControl::resource_of(&row.id, &row.label, tilo_actions::OperationKind::Effect)
            })
            .collect()
    }
}

impl crate::widget::controls::ActionControls for ContextMenu {
    /// Each row is bound to the action it runs, or else to its published
    /// control, under the target its accessibility node names.
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        let mut bindings = crate::widget::controls::default_bindings(self);
        for row in self.published_rows() {
            let [index] = row.path[..] else {
                continue;
            };
            let address = row.address.unwrap_or_else(|| {
                tilo_actions::ActionAddress::new(format!("tilo.control:{}", row.id), "invoke")
            });
            bindings.push(crate::widget::ControlBindingMetadata {
                control_id: format!("context-menu:item:{index}"),
                kind: crate::widget::ControlBindingKind::Action(address),
            });
        }
        bindings
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::super::{ContextMenu, MenuItem};

    /// A row of a nested menu is published under its parent's name, and an
    /// agent choosing it opens the nested menu and presses the row there.
    #[test]
    fn an_agent_chooses_a_nested_row_through_its_published_control() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let by_name = Rc::new(Cell::new(0));
        let mut menu = ContextMenu::with_items(vec![
            MenuItem::action("Rename", || {}),
            MenuItem::separator(),
            MenuItem::submenu(
                "Sort",
                vec![
                    MenuItem::action("By date", || {}),
                    MenuItem::action("By name", {
                        let by_name = Rc::clone(&by_name);
                        move || by_name.set(by_name.get() + 1)
                    }),
                ],
            ),
            MenuItem::action("Hidden", || {}).with_enabled(false),
        ]);
        menu.open_at([40.0, 40.0]);
        let mut harness = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            menu,
            (600, 400),
            crate::theme::Theme::dark(),
        );
        harness.relayout();
        let published = harness
            .resources()
            .into_iter()
            .filter(|resource| resource.id.0.starts_with("tilo.control:menu."))
            .map(|resource| (resource.id.0, resource.label))
            .collect::<Vec<_>>();
        assert_eq!(
            published,
            [
                ("tilo.control:menu.0".to_owned(), "Rename".to_owned()),
                (
                    "tilo.control:menu.2.0".to_owned(),
                    "Sort: By date".to_owned()
                ),
                (
                    "tilo.control:menu.2.1".to_owned(),
                    "Sort: By name".to_owned()
                ),
            ],
            "every row a person can choose now, and only those"
        );
        harness
            .invoke_published("tilo.control:menu.2.1", "invoke", &serde_json::Map::new())
            .expect("the nested row is reachable");
        assert_eq!(by_name.get(), 1);
        assert!(!harness.root::<ContextMenu>().is_open());
    }

    /// A row that runs a published action is that action: no control of
    /// the menu's own, and the action named as the row's binding.
    #[test]
    fn a_row_that_runs_a_published_action_is_bound_to_it() {
        let share = tilo_actions::ActionAddress::new("doc:1", "share");
        let mut menu = ContextMenu::with_items(vec![
            MenuItem::selection("Share").for_action(share.clone()),
            MenuItem::selection("Rename"),
        ]);
        menu.open_at([0.0, 0.0]);
        let ids =
            crate::resources::PublishesResources::resources(&menu, &automerge::AutoCommit::new())
                .into_iter()
                .map(|resource| resource.id.0)
                .collect::<Vec<_>>();
        assert_eq!(ids, ["tilo.control:menu.1"]);
        let bindings = crate::widget::controls::control_action_bindings(&menu)
            .into_iter()
            .filter_map(|binding| match binding.kind {
                crate::widget::ControlBindingKind::Action(address) => {
                    Some((binding.control_id, address))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            bindings,
            [
                ("context-menu:item:0".to_owned(), share),
                (
                    "context-menu:item:1".to_owned(),
                    tilo_actions::ActionAddress::new("tilo.control:menu.1", "invoke")
                ),
            ]
        );
    }
}

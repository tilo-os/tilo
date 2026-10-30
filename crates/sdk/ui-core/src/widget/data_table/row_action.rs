//! A table's own row actions: a check box turned, a row clicked. Each is
//! published as the table's view action and answered by the table itself,
//! so a caller speaks to the table the same way whether the press happened
//! on this device or came from another participant.

/// The operation type of turning a row's check box.
pub(super) const CHECK_ROW: &str = "tilo.ui.table.check";
/// The operation type of clicking a row.
pub(super) const CLICK_ROW: &str = "tilo.ui.table.row_click";

/// A table's own row action — a check box turned, a row clicked — published
/// as the table's view action, and the app's handler for it. The argument
/// is the row, by its index in the rows the host set, with what the person
/// did to it.
pub(super) struct RowAction<A> {
    pub(super) id: String,
    pub(super) resource: crate::tilo_actions::ResourceSpec,
    pub(super) action: crate::widget::ActionEmitter<A>,
    pub(super) handler: Box<dyn FnMut(A)>,
}

/// What a `tilo.ui.table.check` operation carries.
#[derive(serde::Deserialize)]
struct CheckPayload {
    control: String,
    row: usize,
    checked: bool,
}

/// What a `tilo.ui.table.row_click` operation carries.
#[derive(serde::Deserialize)]
struct ClickPayload {
    control: String,
    row: usize,
    #[serde(default)]
    ctrl: bool,
    #[serde(default)]
    shift: bool,
}

impl RowAction<(usize, bool)> {
    /// The box a `check` operation addressed to this table turns, when it is
    /// for it.
    pub(super) fn start(
        &self,
        op: &crate::widget::ResourceOperation<'_>,
    ) -> Result<Option<(usize, bool)>, String> {
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        if request.kind != crate::tilo_actions::OperationKind::View || request.type_id != CHECK_ROW
        {
            return Ok(None);
        }
        let payload: CheckPayload =
            serde_json::from_slice(request.payload).map_err(|error| error.to_string())?;
        Ok((payload.control == self.id).then_some((payload.row, payload.checked)))
    }
}

impl RowAction<(usize, bool, bool)> {
    /// The row a `click` operation addressed to this table clicks, when it is
    /// for it.
    pub(super) fn start(
        &self,
        op: &crate::widget::ResourceOperation<'_>,
    ) -> Result<Option<(usize, bool, bool)>, String> {
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        if request.kind != crate::tilo_actions::OperationKind::View || request.type_id != CLICK_ROW
        {
            return Ok(None);
        }
        let payload: ClickPayload =
            serde_json::from_slice(request.payload).map_err(|error| error.to_string())?;
        Ok((payload.control == self.id).then_some((payload.row, payload.ctrl, payload.shift)))
    }
}

/// The row actions a table answers itself, indexed by the mask of those it
/// publishes (a choice, a check, a click) and whether it keeps its own edits.
pub(super) const ROW_OPERATION_HANDLERS: [&[&str]; 16] = {
    const C: &str = super::super::view_choice::CHOICE_SELECT;
    const K: &str = CHECK_ROW;
    const L: &str = CLICK_ROW;
    const E: &str = crate::undo::EDIT_OPERATION;
    [
        &[],
        &[C],
        &[K],
        &[C, K],
        &[L],
        &[C, L],
        &[K, L],
        &[C, K, L],
        &[E],
        &[C, E],
        &[K, E],
        &[C, K, E],
        &[L, E],
        &[C, L, E],
        &[K, L, E],
        &[C, K, L, E],
    ]
};

use super::DataTableCore;

impl crate::widget::ResourceOperations for DataTableCore {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        let mut mask = 0;
        if self.row_choice.is_some() || self.opens.is_some() {
            mask |= 1;
        }
        if self.check.is_some() {
            mask |= 2;
        }
        if self.row_click.is_some() {
            mask |= 4;
        }
        if !crate::widget::resource_operation::local_history_handlers(self).is_empty() {
            mask |= 8;
        }
        ROW_OPERATION_HANDLERS[mask]
    }

    /// Answer a `select` or an `open` addressed to this table: select the
    /// row and report it, or open it, whoever chose it.
    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let checked = self
            .check
            .as_ref()
            .map(|check| check.start(op))
            .transpose()?
            .flatten();
        let clicked = self
            .row_click
            .as_ref()
            .map(|click| click.start(op))
            .transpose()?
            .flatten();
        if let Some((row, checked)) = checked {
            if row >= self.row_total() {
                return Err(format!("no row `{row}`"));
            }
            if let Some(check) = &mut self.check {
                (check.handler)((row, checked));
            }
            return Ok(Some(crate::widget::view_choice::chosen()));
        }
        if let Some((row, ctrl, shift)) = clicked {
            if row >= self.row_total() {
                return Err(format!("no row `{row}`"));
            }
            if let Some(click) = &mut self.row_click {
                (click.handler)((row, ctrl, shift));
            }
            return Ok(Some(crate::widget::view_choice::chosen()));
        }
        let chosen = self
            .row_choice
            .as_ref()
            .map(|view| view.choice.start(op))
            .transpose()?
            .flatten();
        let opened = self
            .opens
            .as_ref()
            .map(|view| view.choice.start(op))
            .transpose()?
            .flatten();
        let Some((entry, opens)) = chosen
            .map(|entry| (entry, false))
            .or(opened.map(|entry| (entry, true)))
        else {
            return crate::widget::resource_operation::start_by_default(self, op, doc, path);
        };
        let row = entry
            .parse::<usize>()
            .ok()
            .filter(|row| *row < self.row_total())
            .ok_or_else(|| format!("no row `{entry}`"))?;
        // A person's click selected it already, perhaps beside others.
        if !self.selected_rows().contains(&row) {
            self.select_row(row);
        }
        let choice = if opens {
            &mut self.opens
        } else {
            &mut self.row_choice
        };
        if let Some(view) = choice {
            (view.handler)(row);
        }
        Ok(Some(crate::widget::view_choice::chosen()))
    }
}

impl crate::resources::PublishesResources for DataTableCore {
    fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        let mut resources: Vec<tilo_actions::ResourceSpec> = self
            .row_choice
            .iter()
            .chain(self.opens.iter())
            .map(|view| view.choice.resource_unlisted())
            .collect();
        resources.extend(self.check.iter().map(|check| check.resource.clone()));
        resources.extend(self.row_click.iter().map(|click| click.resource.clone()));
        resources
    }
}

//! The shell's own settings, run from the session: the bar's material, its finish and the
//! rest of the `Shell` record's choices, as pressing their controls in the shell's settings
//! does.

use std::collections::BTreeMap;

use serde_json::Value as Json;
use tilolang_tilo::runtime::Value;

use super::{SHELL_RECORD, ShellPart};

impl ShellPart {
    /// Run `setting` of the shell's own record (`root shell: Shell`) with `arguments`, as
    /// pressing its control in the shell's settings does: a choice of the record takes the
    /// name of its case (`set_bar_finish` with `["translucent"]`), the rest the JSON a
    /// reading writes.
    ///
    /// # Errors
    /// The part's package does not run, it declares no such setting, an argument does not
    /// hold the type the setting declares, or the setting refused.
    pub fn run_setting(&mut self, setting: &str, arguments: Vec<Json>) -> Result<(), String> {
        let host = self
            .instance
            .host()
            .ok_or_else(|| "the shell's package does not run".to_owned())?;
        let app = self.instance.app().to_owned();
        let package = host
            .package(&app)
            .ok_or_else(|| format!("`{app}` has no package"))?;
        let meta = &package.meta;
        let root = meta
            .named_roots
            .iter()
            .find(|root| root.name == SHELL_RECORD)
            .ok_or_else(|| format!("`{app}` declares no `{SHELL_RECORD}` record"))?;
        let record = host
            .root_record(&app, &root.ty)
            .map_err(|error| format!("the shell's record is not made: {error:?}"))?;
        let declared = meta
            .actions
            .iter()
            .find(|declared| declared.name == setting)
            .ok_or_else(|| format!("`{app}` declares no `{setting}`"))?;
        // The record the setting changes comes first, as the settings' own presses run it.
        let mut values = vec![Value::resource(&record)];
        for (param, argument) in declared.params.iter().skip(1).zip(arguments) {
            values.push(Self::case_value(meta, &param.ty, &argument)?);
        }
        let first = host.next_ticket();
        let outcome = host
            .runtime()
            .invoke_action(host.viewer(), (declared.id.as_str(), values))
            .map_err(|error| error.to_string())?;
        match outcome {
            tilolang_tilo::runtime::InvokeOutcome::Done(_) => {}
            tilolang_tilo::runtime::InvokeOutcome::Suggested(_) => {
                return Err(format!("`{setting}` waits for a person's approval"));
            }
            tilolang_tilo::runtime::InvokeOutcome::Denied => {
                return Err(format!("`{setting}` is not allowed"));
            }
        }
        // The runtime committed the record's change under a ticket: the part applies it to the
        // record its views read, as it does a change a press in the shell made, so the next
        // turn draws the shell as the setting left it. The tickets are taken here rather than
        // handed to the part's door, since the door's queue is what a press of the shell's own
        // views answers, and the part answers nothing on the person's behalf outside it.
        self.apply_committed(&host, first);
        self.again = true;
        Ok(())
    }

    /// Apply everything the runtime committed since the part last took it — a test's own
    /// invocations, which no setting ran — to the record this part's views read, as a press in
    /// the shell's views has the part apply what it committed.
    #[cfg(test)]
    pub fn apply_committed_for_test(&mut self) {
        if let Some(host) = self.instance.host() {
            self.apply_committed(&host, 0);
        }
    }

    /// Apply what the runtime committed from `first` on to the record this part's views read,
    /// and bring the instance up to date with it.
    fn apply_committed(&mut self, host: &tilolang_tilo::host::Host, first: u64) {
        let at = std::time::Instant::now();
        let mut applied = false;
        for committed in host.take_committed_from(first) {
            let result = match host.store().take_ticket(committed.ticket) {
                Some(action) => {
                    crate::commit::recorded(host, &mut self.record.borrow_mut(), &action)
                }
                None => Err(format!("no committed action {}", committed.ticket)),
            };
            match result {
                Ok(()) => applied = true,
                Err(why) => tracing::error!(why, "a shell setting could not be applied"),
            }
        }
        if !applied {
            return;
        }
        // The record moved: the part's next turn settles what that started and draws the
        // shell as the setting left it, as it does a change another part of this shell
        // committed, since both write the record the parts share.
        self.record.borrow_mut().commit();
        self.work.committing = self.work.committing.saturating_add(at.elapsed());
    }

    /// `argument` as an argument of a parameter of type `ty`: a choice takes the name of its
    /// case, and every other type the JSON a reading writes.
    fn case_value(
        meta: &tilolang_tilo::tilolang::PackageMeta,
        ty: &str,
        argument: &Json,
    ) -> Result<Value, String> {
        // A parameter's declared type names a choice as `enum Finish`; its type is the one the
        // package declares.
        let named = match tilolang_tilo::runtime::meta::parse_ty(ty, &meta.app) {
            Ok(tilolang_tilo::runtime::meta::Ty::Enum(key)) => Some(key.name),
            _ => None,
        };
        let choices = named.as_deref().and_then(|name| {
            meta.enums
                .iter()
                .find(|en| en.name == name || en.id == name)
        });
        if let Some(choices) = choices {
            let name = argument
                .as_str()
                .ok_or_else(|| format!("`{}` takes a case's name", choices.name))?;
            let case = choices
                .variants
                .iter()
                .find(|case| case.name == name || case.id == name)
                .ok_or_else(|| format!("`{}` has no case `{name}`", choices.name))?;
            return Ok(Value::Enum(tilolang_tilo::runtime::value::EnumValue {
                ty: choices.id.clone(),
                variant: case.id.clone(),
                fields: BTreeMap::new(),
            }));
        }
        tilolang_tilo::runtime::value::from_json(argument)
    }
}

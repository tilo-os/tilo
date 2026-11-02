//! The shell's own settings, run from the session: the bar's material, its finish and the
//! rest of the `Shell` record's choices, as pressing their controls in the shell's settings
//! does.

use std::collections::BTreeMap;

use serde_json::Value as Json;
use tilolang_tilo::runtime::{Principal, Value};

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
            values.push(Self::case_value(meta, &param.ty, argument)?);
        }
        host.runtime()
            .invoke_action(&Principal::new("session"), (declared.id.as_str(), values))
            .map_err(|error| error.to_string())?;
        // The record the setting wrote is this part's: its next turn sees the heads move and
        // draws the shell as it is now, so the part goes on to that turn.
        self.again = true;
        Ok(())
    }

    /// `argument` as an argument of a parameter of type `ty`: a choice takes the name of its
    /// case, and every other type the JSON a reading writes.
    fn case_value(
        meta: &tilolang_tilo::tilolang::PackageMeta,
        ty: &str,
        argument: Json,
    ) -> Result<Value, String> {
        if let Some(choices) = meta.enums.iter().find(|en| en.id == ty || en.name == ty) {
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
        tilolang_tilo::runtime::value::from_json(&argument)
    }
}

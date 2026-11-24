//! The shell's own actions, run from the session: the shell style it shows, the bar's material,
//! its finish and the rest of its records' choices, as pressing their controls in its views does.

use std::collections::BTreeMap;

use serde_json::Value as Json;
use tilolang_tilo::runtime::Value;
use tilolang_tilo::runtime::meta::NamedRootMeta;

use super::ShellPart;

impl ShellPart {
    /// Run the shell's action `action` with `arguments`, as pressing its control in the shell's
    /// views does: the action's record comes first, the record a root of the shell's package is
    /// (`root shell: Shell`, `root viewing: Viewing`), and a choice of that record takes the name
    /// of its case (`set_bar_finish` with `["translucent"]`), the rest the JSON a reading writes.
    ///
    /// # Errors
    /// The part's package does not run, it declares no such action, the action's record is not
    /// made, an argument does not hold the type the action declares, or the action refused.
    pub fn run_action(&mut self, action: &str, arguments: Vec<Json>) -> Result<(), String> {
        let host = self
            .instance
            .host()
            .ok_or_else(|| "the shell's package does not run".to_owned())?;
        let app = self.instance.app().to_owned();
        let package = host
            .package(&app)
            .ok_or_else(|| format!("`{app}` has no package"))?;
        let meta = &package.meta;
        let declared = meta
            .actions
            .iter()
            .find(|declared| declared.name == action)
            .ok_or_else(|| format!("`{app}` declares no `{action}`"))?;
        // The record the action changes is its first parameter's, and comes first as its own
        // presses pass it. A parameter names its type as it is written, and a root by the id its
        // type is declared under, so the type is read between them.
        let root = declared
            .params
            .first()
            .and_then(
                |param| match tilolang_tilo::runtime::meta::parse_ty(&param.ty, &meta.app) {
                    Ok(tilolang_tilo::runtime::meta::Ty::Record(key)) => meta
                        .named_roots
                        .iter()
                        .find(|root| Self::root_type(meta, root) == key.name.as_str()),
                    _ => None,
                },
            )
            .ok_or_else(|| format!("`{app}` declares no record `{action}` acts on"))?;
        let record = host
            .root_resource(&app, &root.ty)
            .map_err(|error| format!("the shell's `{}` is not made: {error:?}", root.name))?;
        let mut values = vec![Value::resource(&record)];
        for (param, argument) in declared.params.iter().skip(1).zip(arguments) {
            values.push(Self::case_value(meta, &param.ty, &argument)?);
        }
        let outcome = host
            .runtime()
            .invoke_action(host.viewer(), (declared.id.as_str(), values))
            .map_err(|error| error.to_string())?;
        match outcome {
            tilolang_tilo::runtime::InvokeOutcome::Done(_) => {}
            tilolang_tilo::runtime::InvokeOutcome::Waits { .. }
            | tilolang_tilo::runtime::InvokeOutcome::Asked(_) => {
                return Err(format!("`{action}` waits for a person's approval"));
            }
            tilolang_tilo::runtime::InvokeOutcome::Denied => {
                return Err(format!("`{action}` is not allowed"));
            }
        }
        // The runtime wrote the change to the shell's store: the next turn draws the shell as
        // the action left it.
        self.again = true;
        Ok(())
    }

    /// The name of the type the root `root` of `meta` is made of: the type it is declared under,
    /// or the id its root names when the package declares no such type.
    fn root_type<'a>(
        meta: &'a tilolang_tilo::tilolang::PackageMeta,
        root: &'a NamedRootMeta,
    ) -> &'a str {
        meta.types
            .iter()
            .find(|ty| ty.id == root.ty)
            .map_or(root.ty.as_str(), |ty| ty.name.as_str())
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

//! The value a view state starts from when a run stages one, before any person picks there:
//! what `layout-check --state place=Task.today` sets, read in the app's own terms.

use crate::meta::Meta;
use crate::meta::TypeKey;
use crate::query_host::filters_of;
use crate::registry::Registry;
use crate::value::Value;

/// The state of `app` named `state`, as its view declares it; `view.state` names the view where
/// more than one declares one name.
///
/// # Errors
///
/// A message when no view of the app declares it, or more than one does and none is named.
pub fn declared_state(
    meta: &Meta,
    app: &str,
    state: &str,
) -> Result<crate::meta::StateMeta, String> {
    let (view, name) = match state.split_once('.') {
        Some((view, name)) => (Some(view), name),
        None => (None, state),
    };
    let mut declared: Vec<(String, crate::meta::StateMeta)> = Vec::new();
    for v in meta
        .views
        .iter()
        .filter(|v| view.is_none_or(|name| v.name == name))
    {
        for s in v.state.iter().filter(|s| s.name == name) {
            // A view the metadata names more than once (one per root it shows) declares its
            // state once.
            if declared.iter().all(|(_, held)| held.id != s.id) {
                declared.push((v.name.clone(), s.clone()));
            }
        }
    }
    match declared.as_slice() {
        [(_, one)] => Ok(one.clone()),
        [] => {
            let mut names: Vec<String> = meta
                .views
                .iter()
                .flat_map(|v| v.state.iter().map(|s| format!("`{}.{}`", v.name, s.name)))
                .collect();
            names.sort();
            let what = if names.is_empty() {
                "no state".to_owned()
            } else {
                names.join(", ")
            };
            Err(format!(
                "no view of `{app}` declares the state `{state}`; its views declare: {what}"
            ))
        }
        many => {
            let views: Vec<&str> = many.iter().map(|(view, _)| view.as_str()).collect();
            Err(format!(
                "`{name}` is a state of {}: name the view, as `{}.{name}`",
                views.join(", "),
                views.first().copied().unwrap_or_default()
            ))
        }
    }
}

/// The value `text` names as the app's own code writes it, read against the declared type `ty`:
/// a filter of the type it queries (`Task.today`), a case of an enum (`Side.right`), or the
/// literal a day, a time, a number, a truth or a text takes.
///
/// # Errors
///
/// A message when the type takes no such value, or the app declares neither the type the filter
/// queries nor the enum of the case.
pub fn state_value(
    reg: &Registry,
    app: &str,
    meta: &Meta,
    ty: &str,
    text: &str,
) -> Result<Value, String> {
    let text = text.trim();
    // The metadata spells a named type by its kind: `enum Side`.
    let ty = ty.strip_prefix("enum ").unwrap_or(ty);
    let (prefix, name) = match text.rsplit_once('.') {
        Some((prefix, name)) => (Some(prefix), name),
        None => (None, text),
    };
    if let Some(queried) = ty
        .strip_prefix("Filter<")
        .and_then(|rest| rest.strip_suffix('>'))
    {
        let queried = queried.rsplit('.').next().unwrap_or(queried);
        if prefix.is_some_and(|prefix| prefix != queried) {
            return Err(format!(
                "`{ty}` holds the filters of `{queried}`, not of `{}`",
                prefix.unwrap_or_default()
            ));
        }
        let declared = meta
            .types
            .iter()
            .find(|t| t.name == queried)
            .ok_or_else(|| format!("`{app}` declares no type `{queried}`"))?;
        // A staged value is the query a check asks a view to start from, not words a window
        // shows: it keeps the words the app wrote.
        let filter = filters_of(declared, &meta.enums, &|_, source| source.to_owned())
            .into_iter()
            .find(
                |f| matches!(f, Value::Tuple(parts) if parts.first() == Some(&Value::text(name))),
            );
        return filter.ok_or_else(|| {
            let keys: Vec<&str> = declared
                .search
                .filters
                .iter()
                .map(|f| f.key.as_str())
                .collect();
            format!(
                "`{queried}` declares no filter `{name}`; its filters: {}",
                keys.join(", ")
            )
        });
    }
    if let Some(declared) = meta.enums.iter().find(|e| e.name == ty) {
        if prefix.is_some_and(|prefix| prefix != declared.name) {
            return Err(format!(
                "`{ty}` holds a `{}`, not a `{}`",
                declared.name,
                prefix.unwrap_or_default()
            ));
        }
        let key = TypeKey {
            app: app.to_owned(),
            name: declared.name.clone(),
        };
        return reg.enum_value(&key, name).map(Value::Enum);
    }
    match ty {
        "Bool" => match text {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            other => Err(format!(
                "a `Bool` state holds `true` or `false`, not `{other}`"
            )),
        },
        "Int" => text
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| format!("an `Int` state holds a whole number, not `{text}`")),
        "Text" | "Date" | "Time" | "TimeStamp" => Ok(Value::text(text.trim_matches('"'))),
        other => Err(format!(
            "`--state` sets a filter (`Task.today`), an enum's case (`Side.right`), or a literal a day, a time, a number, a truth or a text takes; this state is a `{other}`"
        )),
    }
}

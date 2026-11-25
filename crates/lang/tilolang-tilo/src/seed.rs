//! A workspace of `n` items, for measuring a window as a person with that many sees it: the items
//! of the package's root collection, made a thousand at a time by the app's own action that adds
//! many items at once, as a person adds them, and their fields then set as the person's own edit.
//! They are a fixture: what a shot, a benchmark or a test starts from, never what an outside
//! service said, so no store admits them as an origin's write; and a field edit sets fields and
//! makes no record, so the records are made by the action that makes them.

use tilolang_runtime::ResourceRef;
use tilolang_runtime::origin::OriginWrite;
use tilolang_runtime::value::Value;

use crate::host::Host;
use crate::meta::MetaExt as _;
use crate::schema::MergeKind;

/// How many items one change writes.
const BATCH: usize = 1000;

/// Fill the root collection of the app `app`'s first root with `n` items at the instant `at`,
/// made by the app's action that adds many items from their titles ([`maker`]): the first text
/// field of each `Item <i>`, the others `Text of item <i>`, every third item's
/// yes/no fields set (one an action settles by running that action on it), each set of texts
/// holding `inbox`, and every second item's optional days the day of `at`: a workspace a person
/// adds work to day by day, where a place that filters by the day holds some items and not
/// others. Returns how many items were written.
///
/// # Errors
/// The app is not loaded, its first root holds no collection, it has no action that adds many
/// items at once, or the store refused a write.
pub fn seed(host: &Host, app: &str, n: usize, at: &str) -> Result<usize, String> {
    seed_more(host, app, 0, n, at)
}

/// As [`seed`], numbering the items from `from`: to grow a workspace already seeded.
///
/// # Errors
/// As [`seed`].
pub fn seed_more(host: &Host, app: &str, from: usize, n: usize, at: &str) -> Result<usize, String> {
    let (_, _, item_ty, _) = collection(host, app)?;
    let schemas = host
        .package(app)
        .map(|package| package.meta.schemas())
        .unwrap_or_default();
    let item = schemas
        .iter()
        .find(|schema| schema.id == item_ty)
        .ok_or_else(|| format!("{app}'s item type is not declared"))?;
    let texts: Vec<&str> = item
        .fields
        .iter()
        .filter(|field| field.kind == MergeKind::Text)
        .map(|field| field.id.as_str())
        .collect();
    let all_flags: Vec<&str> = item
        .fields
        .iter()
        .filter(|field| {
            field.kind == MergeKind::Register
                && matches!(field.ty, Some(tilolang_runtime::meta::Ty::Bool))
        })
        .map(|field| field.id.as_str())
        .collect();
    // A flag an action settles is set by that action, as every write of it from outside the
    // app's own code is: what the action keeps in step with it (a task's status with its done
    // box) follows, so a seeded item is one the app could have made. The rest are written whole.
    let meta = host
        .package(app)
        .map(|package| package.meta)
        .ok_or_else(|| format!("{app} is not loaded"))?;
    let maker = maker(&meta, item)?;
    let actions = meta.actions;
    let settler_of = |field: &str| {
        actions
            .iter()
            .find(|action| !action.scenario && action.settles.as_deref() == Some(field))
            .map(|action| action.id.clone())
    };
    let settled: Vec<String> = all_flags.iter().filter_map(|id| settler_of(id)).collect();
    let flags: Vec<&str> = all_flags
        .iter()
        .copied()
        .filter(|id| settler_of(id).is_none())
        .collect();
    let sets: Vec<&str> = item
        .fields
        .iter()
        .filter(|field| field.kind == MergeKind::Set)
        .map(|field| field.id.as_str())
        .collect();
    let days: Vec<&str> = item
        .fields
        .iter()
        .filter(|field| {
            field.kind == MergeKind::Register
                && matches!(&field.ty, Some(tilolang_runtime::meta::Ty::Option(inner))
                    if matches!(**inner, tilolang_runtime::meta::Ty::Date))
        })
        .map(|field| field.id.as_str())
        .collect();
    let day = at.get(..10).unwrap_or(at).to_owned();
    let mut written = 0;
    while written < n {
        let end = n.min(written.saturating_add(BATCH));
        let numbers = from.saturating_add(written)..from.saturating_add(end);
        let titles = numbers
            .clone()
            .map(|i| Value::Text(format!("Item {i}")))
            .collect();
        let made = match host
            .runtime()
            .invoke_action(host.viewer(), (maker.as_str(), vec![Value::List(titles)]))
            .map_err(|error| error.to_string())?
        {
            tilolang_runtime::InvokeOutcome::Done(Value::List(made)) => made
                .into_iter()
                .map(|item| match item {
                    Value::Ref(item) => Ok((*item.target).clone()),
                    other => Err(format!("{app}'s {maker} answered {other:?} for an item")),
                })
                .collect::<Result<Vec<ResourceRef>, String>>()?,
            other => return Err(format!("{app}'s {maker}: {other:?}")),
        };
        if made.len() != end - written {
            return Err(format!(
                "{app}'s {maker} made {} items of {}",
                made.len(),
                end - written
            ));
        }
        let writes = numbers
            .clone()
            .zip(&made)
            .map(|(i, item)| {
                let mut fields: Vec<(String, Value)> = texts
                    .iter()
                    .skip(1)
                    .map(|id| ((*id).to_owned(), Value::Text(format!("Text of item {i}"))))
                    .collect();
                fields.extend(
                    flags
                        .iter()
                        .map(|id| ((*id).to_owned(), Value::Bool(i % 3 == 0))),
                );
                fields.extend(sets.iter().map(|id| {
                    (
                        (*id).to_owned(),
                        Value::List(vec![Value::Text("inbox".to_owned())]),
                    )
                }));
                if i % 2 == 0 {
                    fields.extend(days.iter().map(|id| {
                        (
                            (*id).to_owned(),
                            Value::Opt(Some(Box::new(Value::Text(day.clone())))),
                        )
                    }));
                }
                OriginWrite::Set {
                    target: item.clone(),
                    fields,
                }
            })
            .collect();
        host.runtime()
            .write_edit_for(Some(host.viewer().clone()), Some(at.to_owned()), |change| {
                change.write(writes)
            })
            .map_err(|error| error.to_string())?;
        if !settled.is_empty() {
            for (i, item) in numbers.zip(made) {
                if i % 3 != 0 {
                    continue;
                }
                for settler in &settled {
                    let args = vec![Value::resource(&item), Value::Bool(true)];
                    match host
                        .runtime()
                        .invoke_action(host.viewer(), (settler.as_str(), args))
                        .map_err(|error| error.to_string())?
                    {
                        tilolang_runtime::InvokeOutcome::Done(_) => {}
                        other => return Err(format!("{app}'s {settler} on item {i}: {other:?}")),
                    }
                }
            }
        }
        written = end;
    }
    Ok(written)
}

/// The action of the app `meta` that adds many items of the type `item` at once from their
/// titles (`action new_tasks(titles: List<Text>, …) -> List<Task>`): one an app offers, not a
/// scenario's, whose one parameter with no default is a list of texts and which answers the
/// items it made. It is how a seed makes its items, as a person adds them.
///
/// # Errors
/// The app declares no such action.
fn maker(
    meta: &tilolang_runtime::meta::Meta,
    item: &crate::schema::TypeSchema,
) -> Result<String, String> {
    use tilolang_runtime::meta::{Ty, parse_ty};
    // A type the package's own metadata writes reads, or the package is broken: say so.
    let read = |text: &str| parse_ty(text, &meta.app);
    let mut found = None;
    for action in meta.actions.iter().filter(|action| !action.scenario) {
        let Some(result) = action.result.as_deref() else {
            continue;
        };
        let answers_items = matches!(&read(result)?, Ty::List(inner)
            if matches!(&**inner, Ty::Record(key) if key.app == item.app && key.name == item.name));
        let required: Vec<_> = action
            .params
            .iter()
            .filter(|param| param.default.is_none())
            .collect();
        let takes_titles = match required.as_slice() {
            [titles] => matches!(&read(&titles.ty)?, Ty::List(inner) if **inner == Ty::Text),
            _ => false,
        };
        if answers_items && takes_titles {
            found = Some(action);
            break;
        }
    }
    found.map(|action| action.id.clone()).ok_or_else(|| {
        format!(
            "{} has no action that adds many {} at once from their titles",
            meta.app, item.name
        )
    })
}

/// The collection the app `app`'s items are seeded into: its first root, the root's first
/// collection field, the type of the items it holds, and the items' first text field.
///
/// # Errors
/// The app is not loaded, or its first root holds no collection of items with a text field.
pub fn collection(host: &Host, app: &str) -> Result<(ResourceRef, String, String, String), String> {
    let meta = host
        .package(app)
        .map(|package| package.meta)
        .ok_or_else(|| format!("{app} is not loaded"))?;
    let root_ty = meta
        .roots
        .first()
        .cloned()
        .ok_or_else(|| format!("{app} has no root to seed"))?;
    let schemas = meta.schemas();
    let root_schema = schemas
        .iter()
        .find(|schema| schema.id == root_ty)
        .ok_or_else(|| format!("{app}'s root type is not declared"))?;
    let (field, item_ty) = root_schema
        .fields
        .iter()
        .find_map(|field| {
            (field.kind == MergeKind::Collection)
                .then(|| field.record.clone().map(|item| (field.id.clone(), item)))
                .flatten()
        })
        .ok_or_else(|| format!("{app}'s root holds no collection to seed"))?;
    let title = schemas
        .iter()
        .find(|schema| schema.id == item_ty)
        .and_then(|item| {
            item.fields
                .iter()
                .find(|field| field.kind == MergeKind::Text)
        })
        .map(|field| field.id.clone())
        .ok_or_else(|| format!("{app}'s items hold no text"))?;
    let root = host
        .root_resource(app, &root_ty)
        .map_err(|error| error.to_string())?;
    Ok((root, field, item_ty, title))
}

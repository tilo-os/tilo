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
    let mut fields = Fields::of(item);
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
    // The settled flags by the names the app writes them under, as a parameter takes them: the
    // store keeps a field by its id.
    let named = |id: &str| {
        meta.types
            .iter()
            .filter(|ty| ty.id == item_ty)
            .flat_map(|ty| &ty.fields)
            .find(|field| field.id == id)
            .map(|field| field.name.as_str())
    };
    let settled_flags: Vec<&str> = fields
        .flags
        .iter()
        .copied()
        .filter(|id| settler_of(id).is_some())
        .map(|id| named(id).ok_or_else(|| format!("{app}'s field {id} has no name")))
        .collect::<Result<_, _>>()?;
    // An item seeded with its settled flags set is made with them by the maker where it takes
    // them, as one change for the batch: its code keeps in step what the settler would. Otherwise
    // each such item is settled by the settler after it is made, one change each.
    let made_set = maker_args(&maker, Value::List(Vec::new()), &settled_flags).is_some();
    let settled: Vec<String> = if made_set {
        Vec::new()
    } else {
        fields
            .flags
            .iter()
            .filter_map(|id| settler_of(id))
            .collect()
    };
    fields.flags.retain(|id| settler_of(id).is_none());
    let day = at.get(..10).unwrap_or(at).to_owned();
    let mut written = 0;
    while written < n {
        let end = n.min(written.saturating_add(BATCH));
        let numbers = from.saturating_add(written)..from.saturating_add(end);
        // The items whose settled flags are set (every third) are made apart from the rest when
        // the maker sets them itself; the two lists keep the items' order within each.
        let (set_numbers, plain_numbers): (Vec<usize>, Vec<usize>) = if made_set {
            numbers.clone().partition(|i| i % 3 == 0)
        } else {
            (Vec::new(), numbers.clone().collect())
        };
        let made = make(
            host,
            app,
            &maker,
            [(plain_numbers, &[][..]), (set_numbers, &settled_flags[..])],
        )?;
        let writes = fields.writes(numbers.clone().zip(&made), &day);
        host.runtime()
            .write_edit_for(Some(host.viewer().clone()), Some(at.to_owned()), |change| {
                change.write(writes)
            })
            .map_err(|error| error.to_string())?;
        if !settled.is_empty() {
            settle(host, app, &settled, numbers.zip(made))?;
        }
        written = end;
    }
    Ok(written)
}

/// The fields of an item type a seed fills past its title, by their ids: its texts, its yes/no
/// flags, its sets of texts and its optional days.
struct Fields<'a> {
    texts: Vec<&'a str>,
    flags: Vec<&'a str>,
    sets: Vec<&'a str>,
    days: Vec<&'a str>,
}

impl<'a> Fields<'a> {
    fn of(item: &'a crate::schema::TypeSchema) -> Self {
        use tilolang_runtime::meta::Ty;
        let of = |keep: &dyn Fn(&crate::schema::FieldSchema) -> bool| -> Vec<&'a str> {
            item.fields
                .iter()
                .filter(|field| keep(field))
                .map(|field| field.id.as_str())
                .collect()
        };
        Self {
            texts: of(&|field| field.kind == MergeKind::Text),
            flags: of(&|field| {
                field.kind == MergeKind::Register && matches!(field.ty, Some(Ty::Bool))
            }),
            sets: of(&|field| field.kind == MergeKind::Set),
            days: of(&|field| {
                field.kind == MergeKind::Register
                    && matches!(&field.ty, Some(Ty::Option(inner)) if matches!(**inner, Ty::Date))
            }),
        }
    }

    /// The writes that fill the numbered `items` past their title, the even ones dated `day`.
    fn writes<'b>(
        &self,
        items: impl Iterator<Item = (usize, &'b ResourceRef)>,
        day: &str,
    ) -> Vec<OriginWrite> {
        items
            .map(|(i, item)| {
                let mut fields: Vec<(String, Value)> = self
                    .texts
                    .iter()
                    .skip(1)
                    .map(|id| ((*id).to_owned(), Value::Text(format!("Text of item {i}"))))
                    .collect();
                fields.extend(
                    self.flags
                        .iter()
                        .map(|id| ((*id).to_owned(), Value::Bool(i % 3 == 0))),
                );
                fields.extend(self.sets.iter().map(|id| {
                    (
                        (*id).to_owned(),
                        Value::List(vec![Value::Text("inbox".to_owned())]),
                    )
                }));
                if i % 2 == 0 {
                    fields.extend(self.days.iter().map(|id| {
                        (
                            (*id).to_owned(),
                            Value::Opt(Some(Box::new(Value::Text(day.to_owned())))),
                        )
                    }));
                }
                OriginWrite::Set {
                    target: item.clone(),
                    fields,
                }
            })
            .collect()
    }
}

/// Make the items numbered in each batch by the app's `maker`, each batch with the flags it names
/// set, and answer them in the order of their numbers.
fn make<const N: usize>(
    host: &Host,
    app: &str,
    maker: &tilolang_runtime::meta::ActionMeta,
    batches: [(Vec<usize>, &[&str]); N],
) -> Result<Vec<ResourceRef>, String> {
    let mut made_by_number = std::collections::BTreeMap::new();
    for (batch, set) in batches {
        if batch.is_empty() {
            continue;
        }
        let titles = Value::List(
            batch
                .iter()
                .map(|i| Value::Text(format!("Item {i}")))
                .collect(),
        );
        let args = maker_args(maker, titles, set)
            .ok_or_else(|| format!("{app}'s {} takes no {set:?}", maker.id))?;
        let made = match host
            .runtime()
            .invoke_action(host.viewer(), (maker.id.as_str(), args))
            .map_err(|error| error.to_string())?
        {
            tilolang_runtime::InvokeOutcome::Done(Value::List(made)) => made,
            other => return Err(format!("{app}'s {}: {other:?}", maker.id)),
        };
        if made.len() != batch.len() {
            return Err(format!(
                "{app}'s {} made {} items of {}",
                maker.id,
                made.len(),
                batch.len()
            ));
        }
        for (i, item) in batch.into_iter().zip(made) {
            let Value::Ref(item) = item else {
                return Err(format!(
                    "{app}'s {} answered {item:?} for an item",
                    maker.id
                ));
            };
            made_by_number.insert(i, (*item.target).clone());
        }
    }
    Ok(made_by_number.into_values().collect())
}

/// Settle every third of the numbered `items` by each of the app's `settled` actions, one change
/// each.
fn settle(
    host: &Host,
    app: &str,
    settled: &[String],
    items: impl Iterator<Item = (usize, ResourceRef)>,
) -> Result<(), String> {
    for (i, item) in items.filter(|(i, _)| i % 3 == 0) {
        for settler in settled {
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
    Ok(())
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
) -> Result<tilolang_runtime::meta::ActionMeta, String> {
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
    found.cloned().ok_or_else(|| {
        format!(
            "{} has no action that adds many {} at once from their titles",
            meta.app, item.name
        )
    })
}

/// The arguments of `maker` that make the items titled `titles`, with each flag of `set` given
/// as `true`: the action's own parameter of that flag's name, which its code keeps what follows
/// from it in step with. A parameter before the last one given takes its `.none`. `None` when
/// the maker takes one of `set` under no parameter of its own, or a parameter before it holds
/// a default other than `.none`, which a seed cannot leave out by position.
fn maker_args(
    maker: &tilolang_runtime::meta::ActionMeta,
    titles: Value,
    set: &[&str],
) -> Option<Vec<Value>> {
    let given = |name: &str| set.contains(&name);
    if !set.iter().all(|flag| {
        maker
            .params
            .iter()
            .any(|param| param.name == *flag && param.ty == "Bool")
    }) {
        return None;
    }
    let last = maker
        .params
        .iter()
        .rposition(|param| param.default.is_none() || given(&param.name))?;
    let mut titles = Some(titles);
    maker
        .params
        .get(..=last)?
        .iter()
        .map(|param| {
            if param.default.is_none() {
                titles.take()
            } else if given(&param.name) {
                Some(Value::Bool(true))
            } else if param
                .default
                .as_ref()
                .is_some_and(tilolang_runtime::meta::DefaultMeta::is_none)
            {
                Some(Value::Opt(None))
            } else {
                None
            }
        })
        .collect()
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

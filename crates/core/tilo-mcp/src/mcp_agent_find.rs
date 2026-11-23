//! What an agent's `find` answers, and the path it names each result by: the
//! shell's one search, then every hit's path built the way `tilo_ls` writes it.

use super::*;
use tilo_actions::remote::Within;
use tilo_actions::tree::BOUND_PREFIX;

/// What one name asked of one level of one app's directory: the app's provider, the parent and
/// the name.
type Asked = (String, String, String);

/// What one find has read of the apps' directories, kept while it names its hits: each app's
/// levels apart, and what each name asked of a level answered. A search answers many rows of
/// one conversation, and the steps from each of them up to its app are the same steps, so each
/// is asked once.
#[derive(Default)]
pub(super) struct Reads {
    /// The levels read, by the provider that publishes them.
    levels: HashMap<String, Levels>,
    /// What a level answered a name, by what was asked.
    named: HashMap<Asked, Option<Level>>,
}

impl Reads {
    /// The levels read of what `provider` publishes.
    fn of(&mut self, provider: &str) -> &mut Levels {
        self.levels.entry(provider.to_owned()).or_default()
    }
}

/// One hit as a typed answer carries it: the path it reads at, what it is, what it is called,
/// the action it offers and, where it says so, when it was written.
fn hit_row(path: &str, kind: &str, hit: &Found) -> Value {
    let mut row = serde_json::Map::new();
    row.insert("path".into(), json!(path));
    row.insert("kind".into(), json!(kind));
    row.insert("label".into(), json!(hit.label.trim()));
    row.insert("action".into(), json!(hit.open));
    if let Some(when) = hit.when {
        row.insert("when".into(), json!(when));
    }
    Value::Object(row)
}

/// What a search that found nothing answers: what was looked through and what never is, so
/// that silence about something search does not reach is not taken for an answer.
fn nothing_matches(query: &str) -> String {
    format!(
        "nothing matches {} in what you may read here (what the apps keep and the workspace's \
         files); the web, mail still on its server and conversations you take no part in are \
         not searched. tilo_ls shows the apps and what is in them",
        query.trim()
    )
}

/// A path written as what a person types to name a resource (an issue's
/// `#12`), split into that name and what follows it: `None` for a path that
/// begins with anything else. The name goes up to the next unescaped `#`, which
/// starts an action as it does after any other address.
pub(super) fn split_reference(path: &str) -> Option<(&str, &str)> {
    let text = path.trim();
    let rest = text.strip_prefix('#')?;
    let end = rest
        .char_indices()
        .find(|(at, character)| {
            *character == '#'
                && (*at == 0 || rest.as_bytes().get(at.saturating_sub(1)) != Some(&b'\\'))
        })
        .map_or(text.len(), |(at, _)| at.saturating_add(1));
    let (reference, after) = text.split_at(end);
    (!reference.trim_matches('#').trim().is_empty()).then_some((reference.trim(), after))
}

/// `reply`, saying first that it carries content from outside the person's own data. A find
/// that names a file of the workspace's folder answers its name and its path, which whoever
/// put it there wrote: data to reason about, as what the file says is, never an instruction.
fn naming_files(reply: Reply) -> Reply {
    reply.labelled()
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// What the workspace's search finds for `needle`, asked as whoever calls it: said plainly
    /// when this device keeps no search, never answered with a ranking of its own.
    async fn searched(&self, needle: &str, within: &Within) -> Result<Vec<Found>, Failure> {
        let Some(search) = self.host.resource_search() else {
            return Err(
                "this host keeps no search of its workspace: list an app's directory instead \
                 (tilo_ls names the apps)"
                    .into(),
            );
        };
        let found = search
            .find_within(&self.caller, needle, within)
            .await
            .ok_or("the host's search stopped answering")??;
        Ok(found)
    }

    /// `path` as the address the tools list the resource at, when it is
    /// written as the reference a person types (`#12`), or ends in the number
    /// a listing shows an item by (`tasks/Issues/1`, the `#1` a person reads):
    /// what the workspace's search finds for it, named by the path `tilo_ls`
    /// writes. Any other path is itself.
    pub(super) async fn expand_reference(&self, path: &str) -> Result<String, Failure> {
        let (reference, after, numbered) = match split_reference(path) {
            Some((reference, after)) => (reference.to_owned(), after.to_owned(), false),
            None => match path.trim().rsplit_once('/') {
                Some((_, last)) if last.chars().all(|character| character.is_ascii_digit()) => {
                    (format!("#{last}"), String::new(), true)
                }
                _ => return Ok(path.to_owned()),
            },
        };
        let found = self.searched(&reference, &Within::default()).await?;
        let (_, running) = self.tree.providers().await?;
        let mut reads = Reads::default();
        for hit in &found {
            if hit.kind == tilo_actions::remote::FOUND_FILE {
                continue;
            }
            if let Some((path, _)) = self
                .locate(hit.app.as_deref(), hit, &running, &mut reads)
                .await
            {
                return Ok(format!("{path}{after}"));
            }
        }
        // A number that names nothing is not an address of its own: the path
        // stands, and the read it is of says what it finds.
        if numbered {
            return Ok(path.to_owned());
        }
        Err(format!(
            "no resource of the workspace reads as {reference}; tilo_ls lists the apps and what \
             they hold, and tilo_find looks for a word in them"
        )
        .into())
    }

    /// What the workspace holds, by the host's own search: on a desktop the
    /// one search it runs, the palette's ranking and `place_found`'s, so a
    /// text an agent asks for answers what the person sees, in the person's
    /// order; on a host with no desktop its ranking of the records it holds.
    /// Each result is named by the path `tilo_ls` and `tilo_do` take, an
    /// action as `path#action(params)`, so the next call acts on what this
    /// one found. A query narrows itself to a conversation and a stretch of
    /// time ([`Self::narrowed`]).
    ///
    /// A host with no search of its own is told so: answering by its apps'
    /// own directories here would be a second ranking beside the host's.
    pub(super) async fn agent_find(
        &self,
        args: FindArgs,
        structured: bool,
    ) -> Result<Reply, Failure> {
        let (words, within) = self.narrowed(&args.query).await?;
        if words.is_empty() && within.is_everything() {
            return Err("query needs a word to look for".into());
        }
        let limit = args.limit.unwrap_or(FIND_HITS).clamp(1, 100);
        let found = self.searched(&words, &within).await?;
        if found.is_empty() {
            let mut reply = Reply::ok(nothing_matches(&args.query));
            reply.structured = structured.then(|| json!({"query": args.query.trim(), "hits": []}));
            return Ok(reply);
        }
        let wanted = match args
            .app
            .as_deref()
            .map(str::trim)
            .filter(|app| !app.is_empty())
        {
            Some(app) => Some(self.app_ids(app).await?),
            None => None,
        };
        // Each resource once, by its path: the search ranks a resource and
        // the actions on it apart, and an agent acts on the resource. Each
        // directory is read once for all of the results it names. A hit is
        // named as the listing names it, type and all: the same program form.
        let (_, running) = self.tree.providers().await?;
        let forms = self.program_forms().await.unwrap_or_default();
        // Every directory this find reads once, for all the hits that name it: a search answers
        // many rows from one app's directory, and reading it again for each of them is what an
        // agent's find spends its time on.
        let mut reads = Reads::default();
        let mut lines: Vec<String> = Vec::new();
        let mut hits: Vec<Value> = Vec::new();
        for hit in &found {
            if wanted
                .as_ref()
                .is_some_and(|wanted| !hit.app.as_ref().is_some_and(|app| wanted.contains(app)))
            {
                continue;
            }
            // A file of the workspace's folder is no resource of an app: it is
            // named by its path there, which a computer that logged in has
            // below the workspace's folder and a page shows in Files.
            if hit.kind == tilo_actions::remote::FOUND_FILE {
                let path = hit.resource.trim_matches('/');
                let line = format!("{path}  file in the workspace's folder");
                if !lines.contains(&line) {
                    hits.push(json!({"path": path, "kind": "file", "label": path}));
                    lines.push(line);
                }
                continue;
            }
            let Some((line, path, kind)) = self.found_line(hit, &running, &forms, &mut reads).await
            else {
                continue;
            };
            // What says when it was written is answered with it, as a query's `after:` and
            // `before:` read a moment.
            let line = match hit.when.and_then(Within::said_at) {
                Some(at) => format!("{line}  {at}"),
                None => line,
            };
            if !lines.contains(&line) {
                // One hit per resource, by the path it reads at: the search
                // ranks a resource and each action on it apart, and a caller
                // acts on the resource.
                if !hits.iter().any(|hit: &Value| hit["path"] == path) {
                    hits.push(hit_row(&path, forms.kind(&kind).unwrap_or(&kind), hit));
                }
                lines.push(line);
            }
        }
        let total = lines.len();
        let mut text = String::new();
        for line in lines.into_iter().take(limit) {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&line);
        }
        if total > limit {
            let _ = write!(
                text,
                "\n… {} more; narrow the query or raise limit",
                total.saturating_sub(limit)
            );
        }
        if text.is_empty() {
            text = nothing_matches(&args.query);
        }
        // A file's name and its path are the words of whoever put it into the folder.
        let file = tilo_actions::remote::FOUND_FILE;
        let names_files = hits.iter().take(limit).any(|hit| hit["kind"] == file);
        let mut reply = Reply::ok(text);
        if structured {
            reply.structured = Some(json!({
                "query": args.query.trim(),
                "hits": hits.into_iter().take(limit).collect::<Vec<Value>>(),
            }));
        }
        Ok(if names_files {
            naming_files(reply)
        } else {
            reply
        })
    }

    /// The words of `query`, and what the rest of it narrows the search to: `in:here` (the
    /// conversation this turn is in) or `in:` a conversation's path as `tilo_ls` lists it,
    /// `after:` and `before:` a day (`2026-11-10`) or a moment (`2026-11-10T14:00:00Z`).
    async fn narrowed(&self, query: &str) -> Result<(String, Within), String> {
        let moment = |name: &str, text: &str| {
            Within::moment(text).ok_or_else(|| {
                format!(
                    "{name}: takes a day (2026-11-10) or a moment (2026-11-10T14:00:00Z), not \
                     {text:?}"
                )
            })
        };
        let mut within = Within::default();
        let mut words: Vec<&str> = Vec::new();
        for word in query.split_whitespace() {
            if let Some(place) = word.strip_prefix("in:") {
                within.place = Some(self.placed(place).await?);
            } else if let Some(text) = word.strip_prefix("after:") {
                within.after = Some(moment("after", text)?);
            } else if let Some(text) = word.strip_prefix("before:") {
                within.before = Some(moment("before", text)?);
            } else {
                words.push(word);
            }
        }
        Ok((words.join(" "), within))
    }

    /// What `in:` names: `here`, the conversation this turn is in, or a conversation by the
    /// path `tilo_ls` lists it at.
    async fn placed(&self, place: &str) -> Result<String, String> {
        if place == "here" {
            return self
                .caller
                .turn()
                .map(|turn| turn.conversation.clone())
                .ok_or_else(|| {
                    "in:here is the conversation a turn is in, and this session is in none: \
                     name one by its path (in:chat/…)"
                        .to_owned()
                });
        }
        let parsed = Address::parse(place)?;
        match self.resolve(&parsed).await?.0 {
            Ok(Resolved {
                node: Some(node), ..
            }) => Ok(node.id),
            Ok(_) => Err(format!("in:{place} names an app, not a conversation in it")),
            Err(note) => Err(note),
        }
    }

    /// The app ids `name` means, among the apps this workspace runs and
    /// installs: its whole id or the name `tilo_ls` lists it by, any case. An
    /// error names the apps there are when it names none, so a search
    /// narrowed to an app that is not there does not answer that nothing
    /// matches.
    pub(super) async fn app_ids(&self, name: &str) -> Result<Vec<String>, String> {
        let (_, running) = self.tree.providers().await?;
        let seen: Vec<String> = running
            .iter()
            .filter_map(|(_, app)| app["app_id"].as_str().map(str::to_owned))
            .collect();
        let installed = self.tree.installed().await;
        let unread = installed.as_ref().err().map(ToString::to_string);
        searched_apps(
            name,
            &seen,
            &installed.unwrap_or_default(),
            unread.as_deref(),
        )
    }

    /// One result as the line an agent reads: the path `tilo_ls` names the
    /// resource by, then what it is, or the action it offers after `#`, and
    /// the path and kind it stands at. `forms` names the type as the listing
    /// does (`tasks.Task`), so a row here reads back the same as a row there.
    pub(super) async fn found_line(
        &self,
        found: &Found,
        running: &[(String, Value)],
        forms: &ProgramForms,
        reads: &mut Reads,
    ) -> Option<(String, String, String)> {
        let located = self
            .locate(found.app.as_deref(), found, running, reads)
            .await;
        // No directory of an app this workspace lists holds it (a setting's
        // page, a file): a path `tilo_ls` cannot read names nothing, so the
        // result is left out rather than answered as a path that fails.
        //
        // The path is `locate`'s, which is the one `tilo_ls` lists the
        // resource at: a search row and a listing name it the same way.
        let (named, written) = located?;
        let kind = forms.kind(&written).unwrap_or(&written);
        let label = found.label.trim();
        let line = match found.open.as_deref() {
            // The resource's own `open` names the resource itself, with what
            // it is called when the path does not say it (a note named by its
            // id, a file whose name holds a slash); any other action is
            // written after `#`.
            Some("open") | None => {
                let said = named.rsplit('/').next() == Some(label) || label.is_empty();
                if said {
                    format!("{named}  {kind}")
                } else {
                    format!("{named}  {kind}  {label}")
                }
            }
            Some(action) => format!("{named}#{action}  {label}"),
        };
        Some((line, named, written))
    }

    /// The path and kind of `resource` in the directories of the apps that
    /// publish one: the app `hint` names first, then every other running
    /// app and the host's services, oldest instance first. A workspace's
    /// records are published under the provider that keeps them, whose name
    /// is not the app the person knows, so a resource is found where it is,
    /// never where its id suggests. Each directory is read once per call.
    pub(super) async fn locate(
        &self,
        hint: Option<&str>,
        found: &Found,
        running: &[(String, Value)],
        reads: &mut Reads,
    ) -> Option<(String, String)> {
        // Only the apps `tilo_ls` lists, by the name it lists them by: a
        // path it cannot read names nothing. The app the search said the row
        // is of is tried first.
        let mut providers: Vec<(String, String)> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let mut candidates: Vec<&(String, Value)> = running.iter().rev().collect();
        if let Some(app) = hint {
            let wanted = app_names(std::slice::from_ref(&app.to_owned())).join("");
            candidates.sort_by_key(|(_, entry)| {
                entry["app_id"].as_str().is_none_or(|app_id| {
                    app_names(std::slice::from_ref(&app_id.to_owned())).join("") != wanted
                })
            });
        }
        for (provider, app) in candidates {
            let Some(app_id) = app["app_id"].as_str() else {
                continue;
            };
            if seen.contains(provider) {
                continue;
            }
            seen.push(provider.clone());
            providers.push((
                app_names(std::slice::from_ref(&app_id.to_owned())).join(""),
                provider.clone(),
            ));
        }
        // A hit that says its record is found there, and named by it where
        // the app has several records: two PDFs publish their documents
        // under one id, and the first record's would answer for both.
        if let Some(own) = &found.provider
            && let Some(at) = providers.iter().position(|(_, provider)| provider == own)
        {
            let (name, provider) = providers.remove(at);
            let several = providers.iter().any(|(other, _)| *other == name);
            let app = if several {
                format!("{name}@{provider}")
            } else {
                name
            };
            if let Some((path, kind)) = self.ls_path(&provider, found, reads).await {
                return Some((format!("{app}/{path}"), kind));
            }
            // A record the provider lists no rows of falls through: what its app published
            // names it below.
        }
        for (name, provider) in providers {
            let Some((path, kind)) = self.ls_path(&provider, found, reads).await else {
                continue;
            };
            return Some((format!("{name}/{path}"), kind));
        }
        // A resource a tilolang app publishes (`tl:<app>:<path>`) is named by its app's name and
        // the path its app gave it: the host that keeps the record publishes its search entries
        // where no listing of this workspace holds its rows (a server runs the app for the people
        // who call it), and the entry the search found is what names it.
        if let (Some(app), Some(rest)) = (&found.app, found.resource.strip_prefix("tl:"))
            && let Some(path) = rest
                .strip_prefix(app.as_str())
                .and_then(|rest| rest.strip_prefix(':'))
        {
            let name = app_names(std::slice::from_ref(app)).join("");
            return Some((format!("{name}/{path}"), found.kind.clone()));
        }
        None
    }

    /// The app's other documents, where it keeps one per file (a PDF each): a line and a row
    /// each, by the path that names it (`pdf-viewer@record:…`) and what it holds. Listing the
    /// app shows the record it acts in; these are the rest, so none of them goes unseen.
    pub(super) async fn other_resources(
        &self,
        parsed: &Address,
        resolved: &Resolved,
        (lines, rows): (&mut Vec<String>, &mut Vec<Value>),
    ) {
        if !below_app(parsed).is_empty() || app_of(parsed).contains('@') {
            return;
        }
        let Ok((_, running)) = self.tree.providers().await else {
            return;
        };
        let name = app_names(std::slice::from_ref(&resolved.app_id)).join("");
        for (provider, entry) in &running {
            if entry["app_id"].as_str() != Some(resolved.app_id.as_str())
                || *provider == resolved.provider
                || !tilo_actions::remote::is_resource_provider(provider)
            {
                continue;
            }
            let mut levels = Levels::new();
            // A record that holds nothing yet is no item of the app's.
            let Some(label) = self
                .tree
                .level(provider, "", &mut levels)
                .await
                .ok()
                .and_then(|top| top.children.first().map(|node| node.label.clone()))
                .filter(|label| !label.is_empty())
            else {
                continue;
            };
            let path = format!("{name}@{provider}");
            lines.push(format!("{path}/  {label}"));
            rows.push(json!({"path": path, "name": label, "label": label,
                "kind": resolved.app_id, "has_children": true, "reads": true, "actions": []}));
        }
    }

    /// Whether `node` is the resource `found` names: the same id, or the
    /// same label and kind (a host mints a fresh id at every publication, so
    /// the search's id may name no node the directory answers now).
    fn same_resource(node: &Node, found: &Found) -> bool {
        // A bound id names its resource by the logical key at its end, which
        // is the key the directory publishes it under. A bound field's row
        // is never another row that shares its name (a note's body is called
        // what the note is), so nothing else decides it.
        if bound_key(&found.resource).is_some() {
            return bound_key(&found.resource).is_some_and(|key| key == node.key);
        }
        node.id == found.resource
            || (node.label.trim().eq_ignore_ascii_case(found.label.trim())
                && (node.resource_type == found.kind || !found.kind.contains('.')))
    }

    /// The path `tilo_ls` takes from the app's root down to the resource
    /// `found` names, and what it is. It is built upward: each directory row
    /// says the id of the resource that holds it, so the chain from the hit
    /// to the app is followed a level at a time, and each name is the one
    /// `ls` writes at that level. The cost is the resource's depth, never
    /// how many siblings a container holds.
    #[allow(
        clippy::too_many_lines,
        reason = "one walk from a result up to the app's root, its steps in order"
    )]
    pub(super) async fn ls_path(
        &self,
        provider: &str,
        found: &Found,
        reads: &mut Reads,
    ) -> Option<(String, String)> {
        let node = self.find_node(provider, found, reads).await?;
        let kind = node.resource_type.clone();
        // The name of each step from the hit up, best last.
        let mut names: Vec<String> = Vec::new();
        let mut here = node;
        for _ in 0..DEEP_READS {
            // A row the app publishes at its own top level is named there,
            // however deep its resource also nests it: the shortest address is
            // the one the app's own listing writes.
            let Ok(top) = self.tree.level(provider, "", reads.of(provider)).await else {
                return None;
            };
            if top.children.iter().any(|node| node.id == here.id) {
                names.push(here.found_name(&top.children));
                break;
            }
            let parent = here.parent.clone();
            if parent.is_empty() {
                // A row the top level does not list (a body published beside
                // its note) hangs under the container that feeds it, found by
                // the key its own key extends.
                // A field published beside its record (a note's body) hangs
                // under the record that publishes it: the note is asked for
                // the field's own name, one lookup.
                let field = here.key.rsplit('/').next().unwrap_or_default().to_owned();
                let mut under: Option<(Node, Level)> = None;
                for container in top
                    .children
                    .iter()
                    .filter(|node| node.container)
                    .take(ROOT_PEEK)
                {
                    let got = self
                        .named_once(provider, &container.id, &field, reads)
                        .await;
                    let Ok(Some(at)) = got else {
                        continue;
                    };
                    if let Some(owner) = at.children.iter().find(|node| node.container) {
                        under = Some((owner.clone(), at));
                        break;
                    }
                }
                let Some((owner, at)) = under else {
                    names.push(here.found_name(&top.children));
                    break;
                };
                names.push(here.found_name(&at.children));
                here = owner;
                continue;
            }
            // Asked by the name the row goes by: the rows holding it, not
            // the whole level, whose size the container's contents decide.
            let Ok(level) = self
                .level_named(provider, &parent, here.label.trim(), reads)
                .await
            else {
                return None;
            };
            // The row the parent names, by its id or by the key that id
            // carries: a container is published under one id at the app's
            // root and names another beneath it.
            let key = bound_key(&parent);
            let holder = level
                .children
                .iter()
                .find(|node| node.id == parent)
                .or_else(|| {
                    level.children.iter().find(|node| {
                        node.container
                            && (node.key == parent
                                || key.as_ref().is_some_and(|key| node.key == *key))
                    })
                })
                .cloned()
                // A container the level read does not list is at the app's
                // root instead, which is one more lookup.
                .or_else(|| None);
            let Some(holder) = holder else {
                // Not a row of this level: the top level lists it, and
                // names `here` in the level the parent read.
                let Ok(top) = self.tree.level(provider, "", reads.of(provider)).await else {
                    return None;
                };
                let top_holder = top.children.iter().find(|node| node.id == parent).cloned();
                // A body published beside its note: the note is in the
                // notebook's level, not the body's, and is found by its id
                // there, one container read.
                let top_holder = match top_holder {
                    Some(owner) => Some(owner),
                    // `parent` is an id, not a name a program types a type with: it is found
                    // by the id alone, so no program form is read into it.
                    None => self
                        .tree
                        .deep(provider, &parent, None, reads.of(provider), None)
                        .await
                        .ok()
                        .flatten(),
                };
                // Named among the rows of its own holder's level: the level
                // read for the parent was asked by this row's own name, so
                // it holds this row alone.
                if let Some(owner) = top_holder {
                    let held = self
                        .level_named(provider, &owner.id, here.label.trim(), reads)
                        .await
                        .unwrap_or_else(|_| level.clone());
                    let mut siblings = held.children.clone();
                    siblings.push(owner.clone());
                    names.push(here.found_name(&siblings));
                    here = owner;
                    continue;
                }
                names.push(here.found_name(&level.children));
                break;
            };
            // Named among the siblings of its own holder's level, which is
            // the level `ls` writes its name from, with the holder itself:
            // a field labelled with its record's title is told apart from
            // the record, and falls back to its own key's name (`body`).
            let held = self
                .level_named(provider, &holder.id, here.label.trim(), reads)
                .await
                .unwrap_or_else(|_| level.clone());
            let mut siblings = held.children.clone();
            siblings.push(holder.clone());
            names.push(here.found_name(&siblings));
            here = holder;
        }
        names.reverse();
        Some((names.join("/"), kind))
    }

    /// One level, asked by the name a row goes by where the app answers
    /// that way, and listed where it cannot: a search that names a result
    /// costs the rows naming it, not a page of every sibling.
    pub(super) async fn level_named(
        &self,
        provider: &str,
        parent: &str,
        name: &str,
        reads: &mut Reads,
    ) -> Result<Level, String> {
        if let Ok(Some(named)) = self.named_once(provider, parent, name, reads).await {
            return Ok(named);
        }
        self.tree
            .level(provider, parent, reads.of(provider))
            .await
            .map_err(String::from)
    }

    /// [`Self::named`], asked once in one find: the same name under the same parent of the same
    /// app is the same answer for every hit whose path passes it.
    async fn named_once(
        &self,
        provider: &str,
        parent: &str,
        name: &str,
        reads: &mut Reads,
    ) -> Result<Option<Level>, String> {
        let asked: Asked = (
            provider.to_owned(),
            parent.to_owned(),
            name.trim().to_owned(),
        );
        if let Some(answered) = reads.named.get(&asked) {
            return Ok(answered.clone());
        }
        let answered = self.tree.named(provider, parent, name).await?;
        reads.named.insert(asked, answered.clone());
        Ok(answered)
    }

    /// The directory row `found` names: the row whose id it carries, or the
    /// row whose key is the logical key a bound id ends in (a note's body,
    /// which the note's own level lists).
    pub(super) async fn find_node(
        &self,
        provider: &str,
        found: &Found,
        reads: &mut Reads,
    ) -> Option<Node> {
        let Ok(top) = self.tree.level(provider, "", reads.of(provider)).await else {
            return None;
        };
        let key = bound_key(&found.resource);
        // A bound id's tail names its own logical key, which no app answers
        // by: only an unbound id is asked by its tail.
        let asked = key
            .is_none()
            .then(|| found.resource.rsplit(['/', ':']).next().map(str::to_owned))
            .flatten();
        // Asked at the app's top by the name the search gave it: a host that
        // publishes more rows than one listing reads (a workspace's records)
        // still answers what a name names, and the row may be past the page. A top level that
        // was read whole holds every row a name can name there, and is looked through below
        // by the same rule: only a level the read stopped in is asked. A bound id is asked as
        // before, since its field's row is looked for under what holds it first.
        let asks_top = key.is_some() || top.unread();
        for name in asked.iter().chain([&found.label]).filter(|_| asks_top) {
            let Ok(Some(at)) = self.named_once(provider, "", name, reads).await else {
                continue;
            };
            if let Some(node) = at
                .children
                .iter()
                .find(|node| Self::same_resource(node, found))
            {
                return Some(node.clone());
            }
        }
        // The rows the resource can hang under: the ones the publisher says
        // hold children, and the one its own id extends. A provider that
        // publishes a tree whole lists its root as a plain row, and the tree
        // answers under it all the same, so the id decides as well. The row an
        // id extends is read down alone: its level holds the resource, and the
        // top level of a workspace lists more rows than a peek reads, so the
        // branch that names the resource is not spent looking elsewhere.
        let extends = |node: &&Node| {
            found
                .resource
                .strip_prefix(node.id.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        };
        let extending: Vec<&Node> = top.children.iter().filter(extends).collect();
        let containers: Vec<&Node> = if extending.is_empty() {
            top.children
                .iter()
                .filter(|node| node.container)
                .take(ROOT_PEEK)
                .collect()
        } else {
            extending.into_iter().take(ROOT_PEEK).collect()
        };
        // A bound field's row (a note's body) is published under the record
        // holding it, named by the key the bound id spells: the record's own
        // key with the field's after it. The record is asked by the name the
        // search gave the field (a field is labelled with its record's), and
        // its own level — the record's contents, never the container's —
        // names the field's row. Asked before the top level is scanned: a
        // API that lists a field beside its record lists both with no
        // parent, and only the key says which holds the other.
        if let Some(key) = key.as_ref()
            && let Some((_, field)) = key.rsplit_once('/')
        {
            let field = field.to_owned();
            for container in &containers {
                for name in [&found.label, &field] {
                    let Ok(Some(at)) = self.named_once(provider, &container.id, name, reads).await
                    else {
                        continue;
                    };
                    let Some(holder) = at.children.iter().find(|node| node.container) else {
                        continue;
                    };
                    let Ok(held) = self
                        .tree
                        .level(provider, &holder.id, reads.of(provider))
                        .await
                    else {
                        continue;
                    };
                    if let Some(node) = held.children.iter().find(|node| node.key == *key) {
                        let mut node = node.clone();
                        // The record that publishes the field is the one
                        // whose level lists it; an API that lists the field
                        // beside the record names no parent either.
                        if node.parent.is_empty() {
                            node.parent.clone_from(&holder.id);
                        }
                        return Some(node);
                    }
                }
            }
        }
        for node in &top.children {
            if Self::same_resource(node, found) {
                return Some(node.clone());
            }
        }
        // The resource can hang below the row its id extends: a mailbox is
        // under its account, not under the app's root.
        self.under_rows(provider, found, asked.as_deref(), &containers, reads)
            .await
    }

    /// The row `found` names below the rows `starts`: a provider that
    /// publishes a tree by path publishes its root as a plain row, so the
    /// tree is walked down, each row asked for the names the search gave the
    /// resource, and a level listed only where its names answer nothing. It
    /// stops when the row is found or the tree is deeper than an app draws
    /// it, reading no more rows than a peek at each level.
    async fn under_rows(
        &self,
        provider: &str,
        found: &Found,
        asked: Option<&str>,
        starts: &[&Node],
        reads: &mut Reads,
    ) -> Option<Node> {
        let key = bound_key(&found.resource);
        let mut frontier: Vec<Node> = starts.iter().map(|node| (*node).clone()).collect();
        let mut visited: Vec<String> = Vec::new();
        let mut expanded = 0usize;
        for _ in 0..DEEP_READS {
            let mut deeper: Vec<Node> = Vec::new();
            for container in std::mem::take(&mut frontier) {
                if expanded >= ROOT_PEEK || visited.contains(&container.id) {
                    continue;
                }
                visited.push(container.id.clone());
                expanded = expanded.saturating_add(1);
                // Asked by the name the search gave it: the resource's own
                // label names it where its id does not (an id a publication
                // minted is never a name the app knows).
                for name in asked
                    .into_iter()
                    .chain(std::iter::once(found.label.as_str()))
                {
                    let Ok(Some(inside)) =
                        self.named_once(provider, &container.id, name, reads).await
                    else {
                        continue;
                    };
                    if let Some(node) = inside
                        .children
                        .iter()
                        .find(|node| Self::same_resource(node, found))
                    {
                        return Some(node.clone());
                    }
                }
                // A bound field's row is a child of the container itself,
                // named by the key the bound id spells. Only a bound id: a
                // plain resource is a row the level above named.
                let Ok(level) = self
                    .tree
                    .level(provider, &container.id, reads.of(provider))
                    .await
                else {
                    continue;
                };
                // A row the container's level lists is a row the resource
                // can be: a mailbox stands under its account, a note under its
                // notebook, and neither has to be asked for by name.
                if let Some(node) = level
                    .children
                    .iter()
                    .find(|node| Self::same_resource(node, found))
                {
                    return Some(node.clone());
                }
                if found.resource.starts_with(BOUND_PREFIX)
                    && let Some(key) = &key
                    && let Some(node) = level.children.iter().find(|node| node.key == *key)
                {
                    return Some(node.clone());
                }
                // Only a row the publisher says holds children has a level to
                // read: one that says it holds none ends the branch there.
                deeper.extend(level.children.into_iter().filter(|node| node.container));
            }
            if deeper.is_empty() {
                break;
            }
            frontier = deeper;
        }
        None
    }
}

/// The ids of the apps `name` names among those `running` and `installed`:
/// its whole id or the name `tilo_ls` lists it by, any case. An error naming
/// the apps there are when it names none, so a search narrowed to an app
/// that is not there does not answer that nothing matches. `unread` is why
/// the installed apps could not be read, when they could not: an app that
/// is not running may then be installed, and the error says why the agent
/// cannot know rather than that there is no such app.
fn searched_apps(
    name: &str,
    running: &[String],
    installed: &[InstalledApp],
    unread: Option<&str>,
) -> Result<Vec<String>, String> {
    let known = running
        .iter()
        .chain(installed.iter().map(|app| &app.app_id));
    let mut meant: Vec<String> = Vec::new();
    let mut all: Vec<String> = Vec::new();
    for app_id in known {
        all.push(app_id.clone());
        let short = app_names(std::slice::from_ref(app_id)).join("");
        if (app_id.eq_ignore_ascii_case(name) || short.eq_ignore_ascii_case(name))
            && !meant.contains(app_id)
        {
            meant.push(app_id.clone());
        }
    }
    if meant.is_empty() {
        meant.extend(listed_as(installed, name).map(|app| app.app_id.clone()));
    }
    if meant.is_empty() {
        // No app is called exactly that: the name as a model writes one, among the same apps.
        let named: Vec<(&str, &str)> = installed
            .iter()
            .map(|app| (app.app_id.as_str(), app.label.as_str()))
            .chain(running.iter().map(|app_id| (app_id.as_str(), "")))
            .collect();
        meant.extend(tilo_actions::tree::app_meant(name, &named)?);
    }
    if !meant.is_empty() {
        return Ok(meant);
    }
    if let Some(reason) = unread {
        return Err(format!(
            "no running app is named {name}, and the installed apps could not be read: {}",
            super::answer::words(reason)
        ));
    }
    let listed = app_names(&all);
    Err(if listed.is_empty() {
        format!("no app {name}: no app is running or installed")
    } else {
        format!("no app {name}; the apps: {}", list_names(&listed))
    })
}

//! What an agent's `find` answers, and the path it names each result by: the
//! desk's one search, then every hit's path built the way `tilo_ls` writes it.

use super::*;

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// What the workspace holds, by the host's own search: on a desktop the
    /// one search it runs, the palette's ranking and `place_found`'s, so a
    /// text an agent asks for answers what the person sees, in the person's
    /// order; on a host with no desktop its ranking of the records it holds.
    /// Each result is named by the path `tilo_ls` and `tilo_do` take, an
    /// action as `path#action(params)`, so the next call acts on what this
    /// one found.
    ///
    /// A host with no search of its own is told so: answering by its apps'
    /// own directories here would be a second ranking beside the host's.
    pub(super) async fn agent_find(
        &self,
        args: FindArgs,
        structured: bool,
    ) -> Result<Reply, String> {
        if args.query.trim().is_empty() {
            return Err("query needs a word to look for".into());
        }
        let limit = args.limit.unwrap_or(FIND_HITS).clamp(1, 100);
        let Some(search) = self.host.resource_search() else {
            return Err(
                "this host keeps no search of its workspace: list an app's directory instead \
                 (tilo_ls names the apps)"
                    .into(),
            );
        };
        let found = search
            .find(&self.caller, &args.query)
            .await
            .ok_or("the host's search stopped answering")?
            .map_err(|error| error.to_string())?;
        if found.is_empty() {
            return Ok(Reply::ok(format!(
                "nothing matches {}; tilo_ls shows the apps and what is in them",
                args.query.trim()
            )));
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
        let (_, running) = self.providers().await?;
        let forms = self.program_forms().await.unwrap_or_default();
        let mut lines: Vec<String> = Vec::new();
        let mut hits: Vec<Value> = Vec::new();
        for hit in &found {
            if wanted
                .as_ref()
                .is_some_and(|wanted| !hit.app.as_ref().is_some_and(|app| wanted.contains(app)))
            {
                continue;
            }
            let Some(line) = self.found_line(hit, &running, &forms).await else {
                continue;
            };
            if !lines.contains(&line) {
                // One hit per resource, by the path it reads at: the search
                // ranks a resource and each action on it apart, and a caller
                // acts on the resource.
                if let Some((path, kind)) = self.locate(hit.app.as_deref(), hit, &running).await
                    && !hits.iter().any(|hit: &Value| hit["path"] == path)
                {
                    hits.push(json!({
                        "path": path,
                        "kind": forms.kind(&kind).unwrap_or(&kind),
                        "label": hit.label.trim(),
                        "action": hit.open,
                    }));
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
            text = format!(
                "nothing matches {}; tilo_ls shows the apps and what is in them",
                args.query.trim()
            );
        }
        let mut reply = Reply::ok(text);
        if structured {
            reply.structured = Some(json!({
                "query": args.query.trim(),
                "hits": hits.into_iter().take(limit).collect::<Vec<Value>>(),
            }));
        }
        Ok(reply)
    }

    /// The app ids `name` means, among the apps this workspace runs and
    /// installs: its whole id or the name `tilo_ls` lists it by, any case. An
    /// error names the apps there are when it names none, so a search
    /// narrowed to an app that is not there does not answer that nothing
    /// matches.
    pub(super) async fn app_ids(&self, name: &str) -> Result<Vec<String>, String> {
        let (_, running) = self.providers().await?;
        let seen: Vec<String> = running
            .iter()
            .filter_map(|(_, app)| app["app_id"].as_str().map(str::to_owned))
            .collect();
        let installed = self.installed().await;
        let unread = installed.as_ref().err().cloned();
        searched_apps(
            name,
            &seen,
            &installed.unwrap_or_default(),
            unread.as_deref(),
        )
    }

    /// One result as the line an agent reads: the path `tilo_ls` names the
    /// resource by, then what it is, or the action it offers after `#`.
    /// `forms` names the type as the listing does (`tasks.Task`), so a row
    /// here reads back the same as a row there.
    pub(super) async fn found_line(
        &self,
        found: &Found,
        running: &[(String, Value)],
        forms: &ProgramForms,
    ) -> Option<String> {
        let located = self.locate(found.app.as_deref(), found, running).await;
        // No directory of an app this workspace lists holds it (a setting's
        // page, a file): a path `tilo_ls` cannot read names nothing, so the
        // result is left out rather than answered as a path that fails.
        //
        // The path is `locate`'s, which is the one `tilo_ls` lists the
        // resource at: a search row and a listing name it the same way.
        let (named, kind) = located?;
        let kind = forms.kind(&kind).unwrap_or(&kind);
        Some(match found.open.as_deref() {
            // The resource's own `open` names the resource itself; any other
            // action is written after `#`.
            Some("open") | None => format!("{named}  {kind}"),
            Some(action) => format!("{named}#{action}  {}", found.label.trim()),
        })
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
        for (name, provider) in providers {
            let Some((path, kind)) = self.ls_path(&provider, found).await else {
                continue;
            };
            return Some((format!("{name}/{path}"), kind));
        }
        None
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
                && (node.kind == found.kind || !found.kind.contains('.')))
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
    pub(super) async fn ls_path(&self, provider: &str, found: &Found) -> Option<(String, String)> {
        let mut levels = Levels::new();
        let node = self.find_node(provider, found, &mut levels).await?;
        let kind = node.kind.clone();
        // The name of each step from the hit up, best last.
        let mut names: Vec<String> = Vec::new();
        let mut here = node;
        for _ in 0..DEEP_READS {
            let parent = here.parent.clone();
            if parent.is_empty() {
                // At the app's root: the top level names it there. A row the
                // top level does not list (a body published beside its note)
                // hangs under the container that feeds it, found by the key
                // its own key extends.
                let Ok(top) = self.level(provider, "", &mut levels).await else {
                    return None;
                };
                if top.children.iter().any(|node| node.id == here.id) {
                    names.push(here.name(&top.children));
                    break;
                }
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
                    let got = self.named(provider, &container.id, &field).await;
                    let Ok(Some(at)) = got else {
                        continue;
                    };
                    if let Some(owner) = at.children.iter().find(|node| node.container) {
                        under = Some((owner.clone(), at));
                        break;
                    }
                }
                let Some((owner, at)) = under else {
                    names.push(here.name(&top.children));
                    break;
                };
                names.push(here.name(&at.children));
                here = owner;
                continue;
            }
            // Asked by the name the row goes by: the rows holding it, not
            // the whole level, whose size the container's contents decide.
            let Ok(level) = self
                .level_named(provider, &parent, here.label.trim(), &mut levels)
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
                let Ok(top) = self.level(provider, "", &mut levels).await else {
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
                        .deep(provider, &parent, None, &mut levels, None)
                        .await
                        .ok()
                        .flatten(),
                };
                // Named among the rows of its own holder's level: the level
                // read for the parent was asked by this row's own name, so
                // it holds this row alone.
                if let Some(owner) = top_holder {
                    let held = self
                        .level_named(provider, &owner.id, here.label.trim(), &mut levels)
                        .await
                        .unwrap_or_else(|_| level.clone());
                    let mut siblings = held.children.clone();
                    siblings.push(owner.clone());
                    names.push(here.name(&siblings));
                    here = owner;
                    continue;
                }
                names.push(here.name(&level.children));
                break;
            };
            // Named among the siblings of its own holder's level, which is
            // the level `ls` writes its name from, with the holder itself:
            // a field labelled with its record's title is told apart from
            // the record, and falls back to its own key's name (`body`).
            let held = self
                .level_named(provider, &holder.id, here.label.trim(), &mut levels)
                .await
                .unwrap_or_else(|_| level.clone());
            let mut siblings = held.children.clone();
            siblings.push(holder.clone());
            names.push(here.name(&siblings));
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
        levels: &mut Levels,
    ) -> Result<Level, String> {
        if let Ok(Some(named)) = self.named(provider, parent, name).await {
            return Ok(named);
        }
        self.level(provider, parent, levels).await
    }

    /// The directory row `found` names: the row whose id it carries, or the
    /// row whose key is the logical key a bound id ends in (a note's body,
    /// which the note's own level lists).
    pub(super) async fn find_node(
        &self,
        provider: &str,
        found: &Found,
        levels: &mut Levels,
    ) -> Option<Node> {
        let mut own = Levels::new();
        let levels = if levels.is_empty() { &mut own } else { levels };
        let Ok(top) = self.level(provider, "", levels).await else {
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
        // still answers what a name names, and the row may be past the page.
        for name in asked.iter().chain([&found.label]) {
            let Ok(Some(at)) = self.named(provider, "", name).await else {
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
        // door that lists a field beside its record lists both with no
        // parent, and only the key says which holds the other.
        if let Some(key) = key.as_ref()
            && let Some((_, field)) = key.rsplit_once('/')
        {
            let field = field.to_owned();
            for container in &containers {
                for name in [&found.label, &field] {
                    let Ok(Some(at)) = self.named(provider, &container.id, name).await else {
                        continue;
                    };
                    let Some(holder) = at.children.iter().find(|node| node.container) else {
                        continue;
                    };
                    let Ok(held) = self.level(provider, &holder.id, levels).await else {
                        continue;
                    };
                    if let Some(node) = held.children.iter().find(|node| node.key == *key) {
                        let mut node = node.clone();
                        // The record that publishes the field is the one
                        // whose level lists it; a door that lists the field
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
        self.under_rows(provider, found, asked.as_deref(), &containers, levels)
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
        levels: &mut Levels,
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
                    let Ok(Some(inside)) = self.named(provider, &container.id, name).await else {
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
                let Ok(level) = self.level(provider, &container.id, levels).await else {
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
    if !meant.is_empty() {
        return Ok(meant);
    }
    if let Some(reason) = unread {
        return Err(format!(
            "no running app is named {name}, and the installed apps could not be read: {reason}"
        ));
    }
    let listed = app_names(&all);
    Err(if listed.is_empty() {
        format!("no app {name}: no app is running or installed")
    } else {
        format!("no app {name}; the apps: {}", list_names(&listed))
    })
}

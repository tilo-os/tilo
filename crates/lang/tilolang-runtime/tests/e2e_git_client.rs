//! The Git app (`apps/tl/git-client/app.tl`) end to end: a repository read through the device's
//! engine and kept in the app's records, its changes staged, committed and discarded hunk by
//! hunk, its branches, stashes, working directories and remotes worked, and its views drawn.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::host_work::EngineHost;
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, SimpleAuthority, Value, ViewInstance};

    const SOURCE: &str = include_str!("../../../../apps/tl/git-client/app.tl");
    const REPO: &str = "/srv/repos/garden";

    /// One changed file as the device's engine holds it.
    #[derive(Clone)]
    struct ChangeState {
        path: &'static str,
        area: &'static str,
        kind: &'static str,
        additions: u32,
        deletions: u32,
        ours: u32,
        theirs: u32,
        hunks: u32,
    }

    /// The repository the engine stands for: what a transfer or a commit changed, so the app's
    /// next read sees it.
    #[derive(Default)]
    struct Repo {
        changes: Vec<ChangeState>,
        branches: Vec<&'static str>,
        head: &'static str,
        stashes: Vec<&'static str>,
        worktrees: Vec<&'static str>,
        remotes: Vec<(&'static str, &'static str)>,
        commits: Vec<(&'static str, &'static str)>,
        files: Vec<&'static str>,
        answer: &'static str,
        diff_reads: u32,
    }

    /// The device's engine: what it was asked, and the answers it gives.
    #[derive(Clone, Default)]
    struct Engine {
        asked: Rc<RefCell<Vec<(String, String)>>>,
        repo: Rc<RefCell<Repo>>,
    }

    impl Engine {
        fn new() -> Self {
            let engine = Self::default();
            *engine.repo.borrow_mut() = Repo {
                changes: vec![
                    ChangeState {
                        path: "README.md",
                        area: "unstaged",
                        kind: "modified",
                        additions: 3,
                        deletions: 1,
                        ours: 0,
                        theirs: 0,
                        hunks: 2,
                    },
                    ChangeState {
                        path: "notes/todo.md",
                        area: "untracked",
                        kind: "untracked",
                        additions: 2,
                        deletions: 0,
                        ours: 0,
                        theirs: 0,
                        hunks: 1,
                    },
                ],
                branches: vec!["main"],
                head: "main",
                stashes: vec!["WIP on main: try the blur path"],
                worktrees: vec!["/srv/repos/garden"],
                remotes: vec![("origin", "git@example.org:garden.git")],
                commits: vec![("Start the garden planner", "5f2c1a9")],
                files: vec!["src/beds.rs", "README.md"],
                answer: "",
                diff_reads: 0,
            };
            engine
        }

        fn ops(&self) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .map(|(op, _)| op.clone())
                .collect()
        }

        fn asked_with(&self, op: &str) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .filter(|(asked, _)| asked == op)
                .map(|(_, input)| input.clone())
                .collect()
        }

        fn last_input(&self) -> String {
            self.asked
                .borrow()
                .last()
                .map(|(_, input)| input.clone())
                .unwrap_or_default()
        }

        fn loaded(&self) -> String {
            let repo = self.repo.borrow();
            let changes: Vec<String> = repo
                .changes
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "path": c.path,
                        "old_path": null,
                        "area": c.area,
                        "kind": c.kind,
                        "additions": c.additions,
                        "deletions": c.deletions,
                        "binary": false,
                        "ours": c.ours,
                        "theirs": c.theirs,
                        "language": "plain",
                        "unified": format!("   1    1  {}\n", c.path),
                        "left": format!("   1  {}\n", c.path),
                        "right": format!("   1  {}\n", c.path),
                        "hunks": (0..c.hunks)
                            .map(|index| serde_json::json!({
                                "index": index,
                                "header": format!("hunk {index}"),
                                "old_start": 1,
                                "new_start": 1,
                            }).to_string())
                            .collect::<Vec<_>>(),
                    })
                    .to_string()
                })
                .collect();
            let commits: Vec<String> = repo
                .commits
                .iter()
                .enumerate()
                .map(|(i, (summary, id))| {
                    serde_json::json!({
                        "id": format!("{id}0000000000000000000000000000000000"),
                        "summary": summary,
                        "body": "",
                        "author_name": "Jane Doe",
                        "author_email": "jane@example.com",
                        "time": 1_780_000_000,
                        "age": 3600,
                        "date": "2026-05-29 07:06 UTC",
                        "parents": [],
                        "refs": [serde_json::json!({"name": "main", "kind": "local_branch"}).to_string()],
                        "column": i,
                    })
                    .to_string()
                })
                .collect();
            let branches: Vec<String> = repo
                .branches
                .iter()
                .map(|name| {
                    serde_json::json!({
                        "name": name,
                        "is_head": *name == repo.head,
                        "is_remote": false,
                        "upstream": null,
                        "ahead": 0,
                        "behind": 0,
                        "tip": "5f2c1a9",
                    })
                    .to_string()
                })
                .collect();
            serde_json::json!({
                "answer": repo.answer,
                "name": "garden",
                "head": repo.head,
                "workdir": REPO,
                "author_name": "Jane Doe",
                "author_email": "jane@example.com",
                "changes": changes,
                "commits": commits,
                "branches": branches,
                "tags": [],
                "stashes": repo.stashes.iter().enumerate().map(|(index, message)| serde_json::json!({
                    "index": index, "message": message, "branch": "main",
                }).to_string()).collect::<Vec<_>>(),
                "remotes": repo.remotes.iter().map(|(name, url)| serde_json::json!({
                    "name": name, "url": url,
                }).to_string()).collect::<Vec<_>>(),
                "worktrees": repo.worktrees.iter().map(|path| serde_json::json!({
                    "name": "main", "path": path, "head": "main", "current": true,
                }).to_string()).collect::<Vec<_>>(),
                "worktree_error": null,
            })
            .to_string()
        }
    }

    fn field(input: &str, name: &str) -> serde_json::Value {
        serde_json::from_str::<serde_json::Value>(input)
            .unwrap_or(serde_json::Value::Null)
            .get(name)
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    }

    impl EngineHost for Engine {
        fn call(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            if op == "load" {
                return Ok(self.loaded());
            }
            let mut repo = self.repo.borrow_mut();
            match op {
                "stage_file" | "unstage_file" => {
                    let file = field(input, "file");
                    let area = if op == "stage_file" {
                        "staged"
                    } else {
                        "unstaged"
                    };
                    for change in repo.changes.iter_mut() {
                        if serde_json::Value::String(change.path.to_owned()) == file {
                            change.area = area;
                            if area == "staged" && change.kind == "untracked" {
                                change.kind = "added";
                            }
                        }
                    }
                    repo.answer = "";
                    drop(repo);
                    Ok(self.loaded())
                }
                "stage_all" | "unstage_all" => {
                    let area = if op == "stage_all" {
                        "staged"
                    } else {
                        "unstaged"
                    };
                    for change in repo.changes.iter_mut() {
                        change.area = area;
                    }
                    repo.answer = "";
                    drop(repo);
                    Ok(self.loaded())
                }
                "apply_hunk" | "discard_hunk" => {
                    repo.answer = "";
                    drop(repo);
                    Ok(self.loaded())
                }
                "discard_file" => {
                    let file = field(input, "file");
                    repo.changes
                        .retain(|c| serde_json::Value::String(c.path.to_owned()) != file);
                    repo.answer = "";
                    drop(repo);
                    Ok(self.loaded())
                }
                "commit" => {
                    repo.changes.retain(|c| c.area != "staged");
                    repo.commits.push(("Flush the blur pass", "abc1234"));
                    repo.answer = "abc1234";
                    drop(repo);
                    Ok(self.loaded())
                }
                "create_branch" => {
                    repo.branches.push("topic");
                    if field(input, "checkout") == true {
                        repo.head = "topic";
                    }
                    repo.answer = "";
                    drop(repo);
                    Ok(self.loaded())
                }
                "checkout_branch" => {
                    repo.head = "main";
                    repo.answer = if field(input, "stash") == true {
                        repo.stashes.push("before switching");
                        "9f8e7d6"
                    } else {
                        ""
                    };
                    drop(repo);
                    Ok(self.loaded())
                }
                "delete_branch" => {
                    repo.branches.retain(|name| *name != "topic");
                    repo.answer = "";
                    drop(repo);
                    Ok(self.loaded())
                }
                "stash_save" => {
                    repo.stashes.push("WIP on main");
                    repo.answer = "1a2b3c4";
                    drop(repo);
                    Ok(self.loaded())
                }
                "stash_apply" => {
                    repo.answer = "";
                    drop(repo);
                    Ok(self.loaded())
                }
                "create_worktree" => {
                    repo.worktrees.push("/srv/repos/garden-experiment");
                    repo.answer = "";
                    drop(repo);
                    Ok(self.loaded())
                }
                "fetch" | "pull" | "push" => {
                    repo.answer = "";
                    drop(repo);
                    Ok(self.loaded())
                }
                "resolve_conflict" => {
                    let file = field(input, "file");
                    repo.changes
                        .retain(|c| serde_json::Value::String(c.path.to_owned()) != file);
                    repo.answer = "";
                    drop(repo);
                    Ok(self.loaded())
                }
                "conflict_cost" => Ok(r#"{"ours":2,"theirs":3}"#.to_owned()),
                "commit_detail" => {
                    let files: Vec<String> = repo
                        .files
                        .iter()
                        .map(|path| {
                            serde_json::json!({
                            "path": path,
                            "old_path": null,
                            "kind": "modified",
                            "additions": 2,
                            "deletions": 1,
                            "binary": false,
                            "language": "rust",
                            "unified": "   1    1  fn beds() {}\n   2      -one\n         2 +two\n",
                            "left": "   1  fn beds() {}\n   2  -one\n",
                            "right": "   1  fn beds() {}\n   2  +two\n",
                        }).to_string()
                        })
                        .collect();
                    Ok(serde_json::to_string(&files).unwrap())
                }
                "commit_files" => {
                    let files: Vec<String> = repo
                        .files
                        .iter()
                        .map(|path| {
                            serde_json::json!({
                                "path": path,
                                "old_path": null,
                                "area": "staged",
                                "kind": "modified",
                                "additions": 2,
                                "deletions": 1,
                            })
                            .to_string()
                        })
                        .collect();
                    Ok(serde_json::to_string(&files).unwrap())
                }
                "commit_diff" => {
                    repo.diff_reads += 1;
                    Ok(serde_json::json!({
                        "path": field(input, "file"),
                        "old_path": null,
                        "kind": "modified",
                        "additions": 2,
                        "deletions": 1,
                        "binary": false,
                        "language": "rust",
                        "unified": "   1    1  fn beds() {}\n   2      -one\n         2 +two\n",
                        "left": "   1  fn beds() {}\n   2  -one\n",
                        "right": "   1  fn beds() {}\n   2  +two\n",
                    })
                    .to_string())
                }
                other => Err(format!("`{other}` is no op this device runs")),
            }
        }

        fn start(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            Err("this device answers transfers now".to_owned())
        }

        fn status(&self, _job: &str) -> Result<String, String> {
            Err("no job".to_owned())
        }

        fn cancel(&self, _job: &str) -> bool {
            false
        }
    }

    struct Site {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        engine: Engine,
    }

    impl Site {
        fn new() -> Self {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            let engine = Engine::new();
            rt.set_engine(engine.clone());
            let app = App::compile(SOURCE, &[]);
            app.load(&rt);
            let root = rt.create_root("Repository").unwrap();
            // The window opens the app: the person's session starts then.
            let arrive = app.action("arrive");
            rt.invoke_action(&me(), (arrive.as_str(), vec![Value::resource(&root)]))
                .unwrap();
            Self {
                rt,
                app,
                root,
                engine,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> tilolang_runtime::InvokeOutcome {
            let id = self.app.action(action);
            let answer = self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap();
            // The host settles what the call queued, as a window does after a press.
            let _ = self.rt.settle();
            answer
        }

        fn try_call(
            &self,
            action: &str,
            args: Vec<Value>,
        ) -> Result<tilolang_runtime::InvokeOutcome, String> {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .map_err(|error| error.to_string())
        }

        fn session(&self) -> ResourceRef {
            self.records(&self.root, "sessions")[0].clone()
        }

        fn text(&self, at: &ResourceRef, field: &str) -> String {
            match self.rt.read(at, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn number(&self, at: &ResourceRef, field: &str) -> i64 {
            match self.rt.read(at, field).unwrap() {
                Value::Int(n) => n,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn records(&self, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
            match self.rt.read(at, field).unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(reference) => Some(Rc::unwrap_or_clone(reference.target)),
                        _ => None,
                    })
                    .collect(),
                Value::Coll(coll) => {
                    let _ = coll;
                    panic!("{field} reads a collection handle, not its items")
                }
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn open(&self, path: &str) {
            self.call(
                "open_repository",
                vec![Value::resource(&self.root), Value::text(path)],
            );
        }

        fn window(&self) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view("changes"),
                    vec![Value::resource(&self.root)],
                )
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }

        fn snapshot(&self) -> serde_json::Value {
            serde_json::from_str(&self.text(&self.root, "snapshot")).unwrap_or_default()
        }

        fn snapshot_len(&self, field: &str) -> usize {
            self.snapshot()[field].as_array().map_or(0, Vec::len)
        }

        fn list_len(&self, at: &ResourceRef, field: &str) -> usize {
            match self.rt.read(at, field).unwrap() {
                Value::List(items) => items.len(),
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn enum_field(&self, at: &ResourceRef, field: &str) -> tilolang_runtime::EnumValue {
            match self.rt.read(at, field).unwrap() {
                Value::Enum(value) => value,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn enum_arg(&self, ty: &str, variant: &str) -> Value {
            let ids = &self.app.compiled.ids;
            Value::Enum(tilolang_runtime::EnumValue {
                ty: ids.get(ty).unwrap().to_owned(),
                variant: ids.get(variant).unwrap().to_owned(),
                fields: BTreeMap::default(),
            })
        }
    }

    /// Every label the window draws, wherever it stands in the tree.
    fn collect_labels(node: &tilolang_runtime::UiNode, out: &mut Vec<String>) {
        if let Some(Value::Text(label)) = node.props.get("label") {
            out.push(label.clone());
        }
        if let Some(Value::Text(label)) = node.props.get("title") {
            out.push(label.clone());
        }
        if let Some(Value::Text(label)) = node.props.get("value") {
            out.push(label.clone());
        }
        for child in &node.children {
            collect_labels(child, out);
        }
    }

    tilo_test_env::scenario! {
        fn the_git_client_reads_works_and_shows_a_repository;

        phase opening_a_repository_reads_it_into_the_app;
        {
            let s = Site::new();
            s.open(REPO);
            assert_eq!(
                s.engine.asked_with("load").first().map(|ask| field(ask, "path")),
                Some(serde_json::Value::String(REPO.to_owned())),
                "the engine read the repository at the path"
            );
            let session = s.session();
            assert_eq!(s.text(&s.root, "name"), "garden");
            assert_eq!(s.text(&s.root, "head"), "main");
            let changes = s.records(&s.session(), "changes");
            assert_eq!(changes.len(), 2, "both changes came over");
            assert_eq!(s.text(&changes[0], "path"), "README.md");
            assert_eq!(s.number(&changes[0], "additions"), 3);
            assert_eq!(s.number(&changes[0], "deletions"), 1);
            assert_eq!(s.text(&changes[0], "language"), "plain");
            assert!(s.text(&changes[0], "unified").contains("README.md"));
            let hunks: serde_json::Value =
                serde_json::from_str(&s.text(&changes[0], "hunks")).unwrap();
            assert_eq!(hunks.as_array().map_or(0, Vec::len), 2);
            assert_eq!(s.snapshot_len("branches"), 1);
            assert_eq!(s.snapshot_len("commits"), 1);
            assert_eq!(s.snapshot_len("remotes"), 1);
            assert_eq!(s.snapshot_len("worktrees"), 1);
            assert_eq!(s.text(&s.session(), "path"), REPO);
        }

        phase a_file_is_staged_and_taken_back;
        {
            let s = Site::new();
            s.open(REPO);
            let session = s.session();
            let changes = s.records(&session, "changes");
            s.call(
                "stage_file",
                vec![Value::resource(&s.root), Value::resource(&changes[0])],
            );
            let ask = s.engine.asked_with("stage_file");
            eprintln!("ops: {:?} inputs: {:?}", s.engine.ops(), s.engine.asked);
            assert_eq!(ask.len(), 1);
            assert_eq!(field(&ask[0], "file"), serde_json::Value::String("README.md".to_owned()));
            let staged = s
                .records(&s.session(), "changes")
                .into_iter()
                .find(|c| s.text(c, "path") == "README.md")
                .expect("README.md is still listed");
            let ids = &s.app.compiled.ids;
            assert_eq!(
                s.enum_field(&staged, "area").variant,
                ids.get("ChangeArea.staged").unwrap().to_owned(),
                "the change reads as staged after the refresh"
            );
            s.call(
                "unstage_file",
                vec![Value::resource(&s.root), Value::resource(&staged)],
            );
            assert_eq!(s.engine.asked_with("unstage_file").len(), 1);
            let back = s
                .records(&s.session(), "changes")
                .into_iter()
                .find(|c| s.text(c, "path") == "README.md")
                .expect("README.md is still listed");
            assert_eq!(
                s.enum_field(&back, "area").variant,
                s.app.compiled.ids.get("ChangeArea.unstaged").unwrap().to_owned()
            );
            s.call("stage_all", vec![Value::resource(&s.root)]);
            assert_eq!(s.engine.asked_with("stage_all").len(), 1);
            assert_eq!(
                s.enum_field(&s.records(&s.session(), "changes")[1], "area").variant,
                s.app.compiled.ids.get("ChangeArea.staged").unwrap().to_owned()
            );
            s.call("unstage_all", vec![Value::resource(&s.root)]);
            assert_eq!(s.engine.asked_with("unstage_all").len(), 1);
            assert_eq!(
                s.enum_field(&s.records(&s.session(), "changes")[1], "area").variant,
                s.app.compiled.ids.get("ChangeArea.unstaged").unwrap().to_owned()
            );
        }

        phase a_hunk_is_staged_taken_back_and_discarded;
        {
            let s = Site::new();
            s.open(REPO);
            let change = s.records(&s.session(), "changes")[0].clone();
            s.call(
                "stage_hunk",
                vec![Value::resource(&s.root), Value::resource(&change), Value::Int(1)],
            );
            let ask = s.engine.asked_with("apply_hunk");
            assert_eq!(field(&ask[0], "hunk"), serde_json::Value::from(1));
            assert_eq!(field(&ask[0], "unstage"), serde_json::Value::Bool(false));
            s.call(
                "unstage_hunk",
                vec![Value::resource(&s.root), Value::resource(&change), Value::Int(1)],
            );
            let asks = s.engine.asked_with("apply_hunk");
            assert_eq!(field(&asks[1], "unstage"), serde_json::Value::Bool(true));
            s.call(
                "discard_hunk",
                vec![Value::resource(&s.root), Value::resource(&change), Value::Int(0)],
            );
            let ask = s.engine.asked_with("discard_hunk");
            assert_eq!(ask.len(), 1, "discarding a hunk is its own call");
            assert_eq!(field(&ask[0], "hunk"), serde_json::Value::from(0));
        }

        phase a_file_is_discarded_whole;
        {
            let s = Site::new();
            s.open(REPO);
            let change = s.records(&s.session(), "changes")[0].clone();
            s.call(
                "discard_file",
                vec![Value::resource(&s.root), Value::resource(&change)],
            );
            assert_eq!(s.engine.asked_with("discard_file").len(), 1);
            let left = s.records(&s.session(), "changes");
            assert_eq!(left.len(), 1);
            assert_eq!(s.text(&left[0], "path"), "notes/todo.md");
        }

        phase a_commit_writes_what_is_staged_and_says_what_it_made;
        {
            let s = Site::new();
            s.open(REPO);
            assert!(
                s.try_call(
                    "commit",
                    vec![Value::resource(&s.root), Value::text("")],
                )
                .is_err(),
                "a commit with no message is refused"
            );
            assert!(
                s.try_call(
                    "commit",
                    vec![Value::resource(&s.root), Value::text("Flush the blur pass")],
                )
                .is_err(),
                "a commit with nothing staged is refused"
            );
            assert!(s.engine.asked_with("commit").is_empty(), "nothing reached the repository");
            s.call("stage_all", vec![Value::resource(&s.root)]);
            s.call(
                "commit",
                vec![Value::resource(&s.root), Value::text("Flush the blur pass")],
            );
            assert_eq!(s.engine.asked_with("commit").len(), 1);
            assert!(
                s.records(&s.session(), "changes").is_empty(),
                "the working tree is clean after the commit"
            );
            assert_eq!(s.text(&s.session(), "notice"), "Committed abc1234");
            assert_eq!(s.snapshot_len("commits"), 2);
        }

        phase branches_are_made_checked_out_and_deleted;
        {
            let s = Site::new();
            s.open(REPO);
            assert!(
                s.try_call("create_branch", vec![Value::resource(&s.root), Value::text("")]).is_err(),
                "a branch with no name is refused"
            );
            s.call(
                "create_branch",
                vec![Value::resource(&s.root), Value::text("topic")],
            );
            let ask = s.engine.asked_with("create_branch");
            assert_eq!(field(&ask[0], "name"), serde_json::Value::String("topic".to_owned()));
            assert_eq!(field(&ask[0], "checkout"), serde_json::Value::Bool(true));
            assert_eq!(s.text(&s.root, "head"), "topic", "a new branch is switched to");
            assert!(
                s.text(&s.root, "snapshot").contains("topic"),
                "the new branch is listed"
            );
            s.call("checkout_branch", vec![Value::resource(&s.root), Value::text("main")]);
            assert_eq!(s.text(&s.session(), "pending_switch"), "main");
            s.call("switch_carrying", vec![Value::resource(&s.root)]);
            assert_eq!(s.text(&s.root, "head"), "main");
            s.call("delete_branch", vec![Value::resource(&s.root), Value::text("topic")]);
            assert_eq!(s.engine.asked_with("delete_branch").len(), 1);
        }

        phase a_switch_over_uncommitted_work_asks_and_parks_it;
        {
            let s = Site::new();
            s.open(REPO);
            s.call("checkout_branch", vec![Value::resource(&s.root), Value::text("main")]);
            assert_eq!(
                s.text(&s.session(), "pending_switch"),
                "main",
                "a switch with changes waiting asks first"
            );
            assert!(s.engine.asked_with("checkout_branch").is_empty());
            s.call("switch_stashing", vec![Value::resource(&s.root)]);
            let ask = s.engine.asked_with("checkout_branch");
            assert_eq!(field(&ask[0], "stash"), serde_json::Value::Bool(true));
            assert!(s.text(&s.session(), "notice").contains("stashed as 9f8e7d6"));
            assert_eq!(s.text(&s.session(), "pending_switch"), "");
        }

        phase stashes_and_working_directories_are_worked;
        {
            let s = Site::new();
            s.open(REPO);
            s.call(
                "save_stash",
                vec![Value::resource(&s.root), Value::text("try the blur path")],
            );
            let ask = s.engine.asked_with("stash_save");
            assert_eq!(
                field(&ask[0], "message"),
                serde_json::Value::String("try the blur path".to_owned())
            );
            s.call("apply_stash", vec![Value::resource(&s.root), Value::Int(0)]);
            assert_eq!(s.engine.asked_with("stash_apply").len(), 1);
            s.call(
                "create_worktree",
                vec![
                    Value::resource(&s.root),
                    Value::text("/srv/repos/garden-experiment"),
                    Value::text("codex/experiment"),
                    Value::text("HEAD"),
                ],
            );
            let ask = s.engine.asked_with("create_worktree");
            assert_eq!(field(&ask[0], "branch"), serde_json::Value::String("codex/experiment".to_owned()));
            assert_eq!(s.snapshot_len("worktrees"), 2);
        }

        phase remotes_are_fetched_pulled_and_pushed;
        {
            let s = Site::new();
            s.open(REPO);
            s.call("fetch", vec![Value::resource(&s.root), Value::text("origin")]);
            s.call("pull", vec![Value::resource(&s.root), Value::text("origin")]);
            s.call("push", vec![Value::resource(&s.root), Value::text("origin")]);
            assert_eq!(s.engine.asked_with("fetch").len(), 1);
            assert_eq!(s.engine.asked_with("pull").len(), 1);
            assert_eq!(s.engine.asked_with("push").len(), 1);
            assert_eq!(s.text(&s.session(), "notice"), "Pushed to origin");
        }

        phase a_conflict_says_what_each_side_costs_and_is_settled;
        {
            let s = Site::new();
            {
                let mut repo = s.engine.repo.borrow_mut();
                repo.changes[0].kind = "conflicted";
                repo.changes[0].ours = 2;
                repo.changes[0].theirs = 3;
            }
            s.open(REPO);
            let change = s.records(&s.session(), "changes")[0].clone();
            assert_eq!(s.number(&change, "ours"), 2);
            assert_eq!(s.number(&change, "theirs"), 3);
            s.call(
                "resolve_conflict",
                vec![Value::resource(&s.root), Value::resource(&change), Value::Bool(true)],
            );
            let ask = s.engine.asked_with("resolve_conflict");
            assert_eq!(field(&ask[0], "ours"), serde_json::Value::Bool(true));
            assert_eq!(s.records(&s.session(), "changes").len(), 1);
        }

        phase the_diff_preferences_are_this_persons_and_this_reports_them;
        {
            let s = Site::new();
            s.open(REPO);
            let session = s.session();
            let split = s.enum_arg("DiffLayout", "DiffLayout.split");
            s.call(
                "toggle_layout",
                vec![Value::resource(&s.root), Value::resource(&session), split],
            );
            assert_eq!(
                s.enum_field(&session, "layout").variant,
                s.app.compiled.ids.get("DiffLayout.split").unwrap().to_owned()
            );
            s.call(
                "toggle_syntax",
                vec![Value::resource(&s.root), Value::resource(&session), Value::Bool(false)],
            );
            assert_eq!(s.rt.read(&session, "highlight").unwrap(), Value::Bool(false));
            s.call(
                "toggle_wrap",
                vec![Value::resource(&s.root), Value::resource(&session), Value::Bool(false)],
            );
            assert_eq!(s.rt.read(&session, "wrap").unwrap(), Value::Bool(false));
            s.call(
                "toggle_whitespace",
                vec![Value::resource(&s.root), Value::resource(&session), Value::Bool(true)],
            );
            let ask = s.engine.asked_with("load").last().cloned().unwrap_or_default();
            assert_eq!(
                field(&ask, "whitespace"),
                serde_json::Value::Bool(true),
                "the device renders the whitespace the person asked to see"
            );
        }

        phase history_reads_a_commits_files_and_their_changes;
        {
            let s = Site::new();
            s.open(REPO);
            let read = s.app.action("read_commit");
            let id = s.snapshot()["commits"][0]
                .as_str()
                .map(|text| serde_json::from_str::<serde_json::Value>(text).unwrap()["id"].as_str().unwrap().to_owned())
                .unwrap();
            s.rt.invoke_action(&me(), (read.as_str(), vec![Value::resource(&s.root), Value::text(id)]))
                .unwrap();
            assert_eq!(s.engine.asked_with("commit_detail").len(), 1);
            let session = s.session();
            assert_eq!(
                s.rt.read(&session, "commit_id").unwrap(),
                Value::text(s.text(&session, "commit_id").as_str())
            );
            let files = s.text(&session, "commit_files");
            assert!(files.contains("src/beds.rs"), "{files}");
            assert!(files.contains("fn beds()"), "{files}");
            assert!(files.contains("rust"), "{files}");
        }

        phase the_window_shows_the_open_page_and_then_the_changes;
        {
            let s = Site::new();
            let window = s.window();
            let mut labels = Vec::new();
            collect_labels(window.tree(), &mut labels);
            assert!(
                labels.iter().any(|label| label.contains("Open a repository")),
                "the open page is what a window with no repository shows: {labels:?}"
            );
            s.open(REPO);
            let mut window = s.window();
            window.set_lazy_rows(false).unwrap();
            let mut labels = Vec::new();
            collect_labels(window.tree(), &mut labels);
            assert!(
                labels.iter().any(|label| label == "README.md"),
                "the changed file stands in the window: {labels:?}"
            );
        }
    }
}

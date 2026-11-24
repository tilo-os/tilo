/// The repository a screenshot shows: a small project with changes waiting, history and
/// branches, so the window shows its views with no repository on the machine and no engine.
scenario "a week of changes" {
    let s = git.sessions.insert(Session(path: "~/src/garden-planner"))
    git.path = "~/src/garden-planner"
    git.name = "garden-planner"
    git.head = "spring-layout"
    git.workdir = "~/src/garden-planner"
    git.author_name = "Jane Doe"
    git.author_email = "jane@example.com"
    let bed = s.changes.insert(Change(path: "src/beds.rs"))
    bed.area = .staged
    bed.kind = .modified
    bed.additions = 6
    bed.deletions = 2
    bed.language = "rust"
    bed.unified = "   1    1  pub struct Bed \{\n   2    2      pub name: String,\n   3      -    pub width_m: f32,\n         3 +    pub width_cm: u32,\n         4 +    pub length_cm: u32,\n   4    5  \}\n"
    bed.left = "   1  pub struct Bed \{\n   2      pub name: String,\n   3  -    pub width_m: f32,\n\n\n   4  \}\n"
    bed.right = "   1  pub struct Bed \{\n   2      pub name: String,\n\n   3  +    pub width_cm: u32,\n   4  +    pub length_cm: u32,\n   5  \}\n"
    bed.hunks = json.encode(
        [json.encode(Hunk(index: 0, header: "pub struct Bed", old_start: 1, new_start: 1))],
    )
    let readme = s.changes.insert(Change(path: "README.md"))
    readme.area = .unstaged
    readme.kind = .modified
    readme.additions = 3
    readme.deletions = 1
    readme.unified = "   1    1  # Garden planner\n   2    2  \n   3      -Plan beds.\n         3 +Plan beds and sowing dates for the community garden.\n         4 +\n         5 +Run the planner to see the beds.\n"
    readme.left = "   1  # Garden planner\n   2  \n   3  -Plan beds.\n\n\n"
    readme.right = "   1  # Garden planner\n   2  \n\n   3  +Plan beds and sowing dates for the community garden.\n   4  +\n   5  +Run the planner to see the beds.\n"
    readme.hunks = json.encode(
        [json.encode(Hunk(index: 0, header: "", old_start: 1, new_start: 1))],
    )
    let todo = s.changes.insert(Change(path: "notes/todo.md"))
    todo.area = .untracked
    todo.kind = .untracked
    todo.additions = 2
    todo.unified = "         1 +- Add frost dates for Leipzig\n         2 +- Ask about the water tap\n"
    todo.left = "     \n     \n"
    todo.right = "   1  +- Add frost dates for Leipzig\n   2  +- Ask about the water tap\n"
    todo.hunks = json.encode([json.encode(Hunk(index: 0, header: "", old_start: 0, new_start: 1))])
    git.snapshot = json.encode(
        Loaded(
            name: "garden-planner",
            head: "spring-layout",
            workdir: "~/src/garden-planner",
            author_name: "Jane Doe",
            author_email: "jane@example.com",
            changes: [],
            commits: [
                json.encode(
                    WireCommit(
                        id: "5f2c1a9e4b7d3c8a1f6e0b2d9c4a7e3f5b8d1c6a",
                        summary: "Sow dates per crop",
                        body: "The sowing table is the only place the dates live.",
                        author_name: "Jane Doe",
                        author_email: "jane@example.com",
                        time: 1780000000,
                        age: 172800,
                        date: "2026-05-29 07:06 UTC",
                        parents: ["a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4"],
                        refs: [
                            json.encode(WireRef(name: "spring-layout", kind: .local_branch)),
                            json.encode(WireRef(name: "HEAD", kind: .head)),
                        ],
                        column: 0,
                    ),
                ),
                json.encode(
                    WireCommit(
                        id: "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4",
                        summary: "Add raised beds and their sizes",
                        author_name: "Jane Doe",
                        author_email: "jane@example.com",
                        time: 1779000000,
                        age: 1296000,
                        date: "2026-05-17 10:40 UTC",
                        parents: ["0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c"],
                        refs: [
                            json.encode(WireRef(name: "raised-beds", kind: .local_branch)),
                            json.encode(
                                WireRef(name: "origin/raised-beds", kind: .remote_branch),
                            ),
                        ],
                        column: 1,
                    ),
                ),
                json.encode(
                    WireCommit(
                        id: "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c",
                        summary: "Start the garden planner",
                        author_name: "Sam Okafor",
                        author_email: "sam@example.com",
                        time: 1777000000,
                        age: 3628800,
                        date: "2026-04-24 09:12 UTC",
                        refs: [json.encode(WireRef(name: "v0.1", kind: .tag))],
                        column: 0,
                    ),
                ),
            ],
            branches: [
                json.encode(
                    WireBranch(
                        name: "spring-layout",
                        is_head: true,
                        upstream: .some("origin/spring-layout"),
                        ahead: 2,
                        tip: "5f2c1a9",
                    ),
                ),
                json.encode(
                    WireBranch(
                        name: "raised-beds",
                        ahead: 1,
                        behind: 3,
                        upstream: .some("origin/raised-beds"),
                        tip: "a1b2c3d",
                    ),
                ),
                json.encode(
                    WireBranch(name: "origin/spring-layout", is_remote: true, tip: "5f2c1a9"),
                ),
                json.encode(
                    WireBranch(name: "origin/raised-beds", is_remote: true, tip: "a1b2c3d"),
                ),
            ],
            tags: [
                json.encode(
                    WireTag(name: "v0.1", target: "0f1e2d3", message: .some("The first bed")),
                ),
            ],
            stashes: [
                json.encode(
                    WireStash(
                        index: 0,
                        message: "WIP on spring-layout: try the msaa path",
                        branch: "spring-layout",
                    ),
                ),
            ],
            remotes: [
                json.encode(
                    WireRemote(name: "origin", url: "git@example.org:garden/planner.git"),
                ),
            ],
            worktrees: [
                json.encode(
                    WireWorktree(
                        name: "main",
                        path: "~/src/garden-planner",
                        head: "spring-layout",
                        current: true,
                    ),
                ),
            ],
        ),
    )
    s.commit_id = "5f2c1a9e4b7d3c8a1f6e0b2d9c4a7e3f5b8d1c6a"
    s.commit_files = json.encode(
        [
            json.encode(
                WireFile(
                    path: "src/sowing.rs",
                    kind: .added,
                    additions: 8,
                    language: "rust",
                    unified: "         1 +pub fn first_sowing(crop: &str) -> Option<&'static str> \{\n         2 +    match crop \{\n         3 +        peas => Some(March),\n         4 +        beans => Some(May),\n         5 +        _ => None,\n         6 +    \}\n         7 +\}\n",
                    left: "\n",
                    right: "   1  +pub fn first_sowing(crop: &str) -> Option<&'static str> \{\n   2  +    match crop \{\n   3  +        peas => Some(March),\n   4  +        beans => Some(May),\n   5  +        _ => None,\n   6  +    \}\n   7  +\}\n",
                ),
            ),
        ],
    )
}

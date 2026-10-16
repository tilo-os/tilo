//! A bundled, realistic repository fixture.
//!
//! Used when no on-disk repository is available — chiefly the headless
//! `--shots` renderer — so the screenshot shows a populated, believable
//! state: a handful of staged / unstaged / untracked changes, several
//! files with real diffs (including a rename and a new file), and a commit
//! history with two side branches and a merge so the graph exercises
//! multiple lanes. Everything here is static data; nothing touches disk.

use crate::diff::{language_for, parse_unified};
use crate::graph;
use crate::model::{
    Branch, ChangeArea, ChangeKind, Commit, DiffKey, DiffMap, FileChange, FileDiff, RefKind,
    RefLabel, Remote, RepoModel, Stash, Tag,
};

fn fc(path: &str, area: ChangeArea, kind: ChangeKind, add: u32, del: u32) -> FileChange {
    FileChange {
        path: path.to_string(),
        old_path: None,
        area,
        kind,
        additions: add,
        deletions: del,
    }
}

/// Build the full sample model, with the graph lanes already assigned and a
/// map of per-file diffs for the changed files.
pub fn model() -> (RepoModel, DiffMap) {
    let diffs = sample_diffs();
    let counted = |mut changes: Vec<FileChange>| {
        // A row's line counts are the counts of the diff it opens.
        for change in &mut changes {
            if let Some(diff) = diffs.get(&DiffKey::for_area(change.area, change.path.clone())) {
                change.additions = diff.additions();
                change.deletions = diff.deletions();
            }
        }
        changes
    };
    let staged = counted(sample_staged_changes());
    let unstaged = counted(sample_unstaged_changes());
    let untracked = counted(sample_untracked_changes());

    let mut commits = commit_history();
    let layout = graph::layout(&mut commits);

    let model = RepoModel {
        name: "renderer".into(),
        head: "main".into(),
        workdir: format!("{}/renderer", tilo_paths::WORKSPACE_MOUNT),
        staged,
        unstaged,
        untracked,
        commits,
        branches: sample_branches(),
        tags: sample_tags(),
        stashes: sample_stashes(),
        remotes: sample_remotes(),
        worktrees: Vec::new(),
        worktree_error: None,
        graph_width: layout.lanes,
        author_name: MIRA.0.into(),
        author_email: MIRA.1.into(),
    };

    (model, diffs)
}

fn sample_staged_changes() -> Vec<FileChange> {
    vec![
        fc(
            "src/render/scene.rs",
            ChangeArea::Staged,
            ChangeKind::Modified,
            24,
            6,
        ),
        fc(
            "src/render/pipeline.rs",
            ChangeArea::Staged,
            ChangeKind::Added,
            58,
            0,
        ),
        {
            let mut r = fc(
                "src/gpu/device.rs",
                ChangeArea::Staged,
                ChangeKind::Renamed,
                3,
                3,
            );
            r.old_path = Some("src/gpu/adapter.rs".to_string());
            r
        },
    ]
}

fn sample_unstaged_changes() -> Vec<FileChange> {
    vec![
        fc(
            "src/app.rs",
            ChangeArea::Unstaged,
            ChangeKind::Modified,
            11,
            4,
        ),
        fc(
            "Cargo.toml",
            ChangeArea::Unstaged,
            ChangeKind::Modified,
            2,
            1,
        ),
        fc(
            "src/legacy/blit.rs",
            ChangeArea::Unstaged,
            ChangeKind::Deleted,
            0,
            41,
        ),
    ]
}

fn sample_untracked_changes() -> Vec<FileChange> {
    vec![
        fc(
            "docs/rendering.md",
            ChangeArea::Untracked,
            ChangeKind::Untracked,
            92,
            0,
        ),
        fc(
            "src/render/shaders/blur.wgsl",
            ChangeArea::Untracked,
            ChangeKind::Untracked,
            37,
            0,
        ),
    ]
}

/// Staged files carry a HEAD→index diff; unstaged / untracked files carry a
/// worktree diff — key each on its side so selection resolves the right one.
fn sample_diffs() -> DiffMap {
    let mut diffs: DiffMap = DiffMap::new();
    diffs.insert(DiffKey::staged("src/render/scene.rs"), diff_scene());
    diffs.insert(DiffKey::staged("src/render/pipeline.rs"), diff_pipeline());
    diffs.insert(DiffKey::staged("src/gpu/device.rs"), diff_rename());
    diffs.insert(DiffKey::worktree("src/app.rs"), diff_app());
    diffs.insert(DiffKey::worktree("Cargo.toml"), diff_cargo());
    diffs.insert(DiffKey::worktree("src/legacy/blit.rs"), diff_deleted());
    diffs.insert(DiffKey::worktree("docs/rendering.md"), diff_untracked_md());
    diffs.insert(
        DiffKey::worktree("src/render/shaders/blur.wgsl"),
        diff_untracked_wgsl(),
    );
    diffs
}

fn sample_branches() -> Vec<Branch> {
    vec![
        Branch {
            name: "main".into(),
            is_head: true,
            is_remote: false,
            upstream: Some("origin/main".into()),
            ahead: 2,
            behind: 0,
            tip: "a1b2c3d".into(),
        },
        Branch {
            name: "wgpu-pipeline".into(),
            is_head: false,
            is_remote: false,
            upstream: Some("origin/wgpu-pipeline".into()),
            ahead: 4,
            behind: 1,
            tip: "9f8e7d6".into(),
        },
        Branch {
            name: "fix/scene-leak".into(),
            is_head: false,
            is_remote: false,
            upstream: None,
            ahead: 1,
            behind: 0,
            tip: "5c4b3a2".into(),
        },
        Branch {
            name: "origin/main".into(),
            is_head: false,
            is_remote: true,
            upstream: None,
            ahead: 0,
            behind: 0,
            tip: "7d6e5f4".into(),
        },
        Branch {
            name: "origin/wgpu-pipeline".into(),
            is_head: false,
            is_remote: true,
            upstream: None,
            ahead: 0,
            behind: 0,
            tip: "3a2b1c0".into(),
        },
    ]
}

fn sample_tags() -> Vec<Tag> {
    vec![
        Tag {
            name: "v0.4.0".into(),
            target: "7d6e5f4".into(),
            message: Some("Moves scene encoding to vello 0.9".into()),
        },
        Tag {
            name: "v0.3.2".into(),
            target: "2c3d4e5".into(),
            message: Some("Fixes the text shaping advance".into()),
        },
        Tag {
            name: "v0.3.0".into(),
            target: "8a9b0c1".into(),
            message: Some("First public build".into()),
        },
    ]
}

fn sample_stashes() -> Vec<Stash> {
    vec![
        Stash {
            index: 0,
            message: "On main: Try the MSAA resolve path".into(),
            branch: "main".into(),
        },
        Stash {
            index: 1,
            message: "On wgpu-pipeline: Bind group cache experiment".into(),
            branch: "wgpu-pipeline".into(),
        },
    ]
}

fn sample_remotes() -> Vec<Remote> {
    vec![
        Remote {
            name: "origin".into(),
            url: "git@code.tilo.so:tilo/renderer.git".into(),
        },
        Remote {
            name: "upstream".into(),
            url: "https://github.com/linebender/vello.git".into(),
        },
    ]
}

fn rl(name: &str, kind: RefKind) -> RefLabel {
    RefLabel {
        name: name.to_string(),
        kind,
    }
}

/// The people who wrote the sample history: name and address.
const MIRA: (&str, &str) = ("Mira Okafor", "mira.okafor@example.com");
const ALEX: (&str, &str) = ("Alex Rivera", "alex.rivera@example.com");
const PRIYA: (&str, &str) = ("Priya Nair", "priya.nair@example.com");

const HOUR: i64 = 3_600;
const DAY: i64 = 24 * HOUR;

/// One sample commit, `ago` seconds before `now`.
struct Sample<'a> {
    id: &'a str,
    summary: &'a str,
    body: &'a str,
    author: (&'a str, &'a str),
    ago: i64,
    parents: &'a [&'a str],
    refs: Vec<RefLabel>,
    files: Vec<FileChange>,
}

impl Sample<'_> {
    fn build(self, now: i64) -> Commit {
        let time = now - self.ago;
        Commit {
            id: self.id.to_string(),
            summary: self.summary.to_string(),
            body: self.body.to_string(),
            author_name: self.author.0.to_string(),
            author_email: self.author.1.to_string(),
            when: ui::format::relative(self.ago),
            time,
            parents: self.parents.iter().map(|p| (*p).to_string()).collect(),
            refs: self.refs,
            lane: 0,
            files: self.files,
        }
    }
}

/// A file a commit touched.
fn touched(path: &str, kind: ChangeKind, add: u32, del: u32) -> FileChange {
    fc(path, ChangeArea::Staged, kind, add, del)
}

/// Newest first; `parents` reference ids, structured so the graph uses two
/// lanes with a merge. Times are relative to the toolkit clock, so a capture
/// (whose clock is pinned) shows the same ages on every run.
fn commit_history() -> Vec<Commit> {
    let now = ui::clock::local_now().timestamp();
    commit_history_recent()
        .into_iter()
        .chain(commit_history_older())
        .map(|sample| sample.build(now))
        .collect()
}

/// The four most recent commits: the wgpu-pipeline merge, its two-lane
/// history, and the commits both lanes branch from (part of
/// [`commit_history`]).
fn commit_history_recent() -> Vec<Sample<'static>> {
    vec![
        Sample {
            id: "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678",
            summary: "Merge branch 'wgpu-pipeline'",
            body: "Bring the new wgpu render pipeline onto main.\n\nReplaces the legacy blit path with a compute-driven\nblur and a reusable bind-group cache.",
            author: MIRA,
            ago: 2 * HOUR,
            parents: &[
                "b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8091a2b",
                "9f8e7d6c5b4a39281706f5e4d3c2b1a098765432",
            ],
            refs: vec![rl("HEAD", RefKind::Head), rl("main", RefKind::LocalBranch)],
            files: vec![
                touched("src/render/pipeline.rs", ChangeKind::Added, 58, 0),
                touched("src/render/shaders/blur.wgsl", ChangeKind::Added, 37, 0),
                touched("src/legacy/blit.rs", ChangeKind::Deleted, 0, 41),
            ],
        },
        Sample {
            id: "9f8e7d6c5b4a39281706f5e4d3c2b1a098765432",
            summary: "Add a compute blur pass",
            body: "Implements a separable gaussian blur in WGSL, driven\nfrom a compute pipeline.",
            author: ALEX,
            ago: 5 * HOUR,
            parents: &["3a2b1c0d9e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b"],
            refs: vec![rl("wgpu-pipeline", RefKind::LocalBranch)],
            files: vec![
                touched("src/render/shaders/blur.wgsl", ChangeKind::Added, 37, 0),
                touched("src/render/scene.rs", ChangeKind::Modified, 12, 2),
            ],
        },
        Sample {
            id: "b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8091a2b",
            summary: "Cache bind groups per material",
            body: "",
            author: MIRA,
            ago: DAY + 3 * HOUR,
            parents: &["7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a1b0c9d8e"],
            refs: vec![],
            files: vec![touched(
                "src/render/material.rs",
                ChangeKind::Modified,
                24,
                9,
            )],
        },
        Sample {
            id: "3a2b1c0d9e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b",
            summary: "Sketch the pipeline builder",
            body: "",
            author: ALEX,
            ago: DAY + 5 * HOUR,
            parents: &["7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a1b0c9d8e"],
            refs: vec![rl("origin/wgpu-pipeline", RefKind::RemoteBranch)],
            files: vec![touched("src/render/pipeline.rs", ChangeKind::Added, 31, 0)],
        },
    ]
}

/// The four older commits: the 0.4.0 release, the scene-leak fix, the
/// text-shaping hotfix and the first commit (part of [`commit_history`]).
fn commit_history_older() -> Vec<Sample<'static>> {
    vec![
        Sample {
            id: "7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a1b0c9d8e",
            summary: "Release 0.4.0",
            body: "Cuts scene encoding over to vello 0.9.",
            author: MIRA,
            ago: 2 * DAY + 4 * HOUR,
            parents: &["5c4b3a2f1e0d9c8b7a6f5e4d3c2b1a0f9e8d7c6b"],
            refs: vec![
                rl("origin/main", RefKind::RemoteBranch),
                rl("v0.4.0", RefKind::Tag),
            ],
            files: vec![
                touched("Cargo.toml", ChangeKind::Modified, 1, 1),
                touched("CHANGELOG.md", ChangeKind::Modified, 14, 0),
            ],
        },
        Sample {
            id: "5c4b3a2f1e0d9c8b7a6f5e4d3c2b1a0f9e8d7c6b",
            summary: "Fix a scene memory leak on resize",
            body: "The encoded scene wasn't reset between frames when the\nsurface was reconfigured.",
            author: PRIYA,
            ago: 3 * DAY + 2 * HOUR,
            parents: &["2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d"],
            refs: vec![rl("fix/scene-leak", RefKind::LocalBranch)],
            files: vec![touched("src/render/scene.rs", ChangeKind::Modified, 3, 1)],
        },
        Sample {
            id: "2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d",
            summary: "Correct the text shaping advance",
            body: "",
            author: PRIYA,
            ago: 4 * DAY + 6 * HOUR,
            parents: &["8a9b0c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b"],
            refs: vec![rl("v0.3.2", RefKind::Tag)],
            files: vec![touched("src/text/shape.rs", ChangeKind::Modified, 6, 4)],
        },
        Sample {
            id: "8a9b0c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b",
            summary: "Create the renderer skeleton",
            body: "Opens a window, configures the surface and clears it.",
            author: MIRA,
            ago: 8 * DAY,
            parents: &[],
            refs: vec![rl("v0.3.0", RefKind::Tag)],
            files: vec![
                touched("Cargo.toml", ChangeKind::Added, 12, 0),
                touched("src/main.rs", ChangeKind::Added, 48, 0),
                touched("src/app.rs", ChangeKind::Added, 64, 0),
            ],
        },
    ]
}

// === Per-file diff fixtures ==========================================

fn build(path: &str, raw: &str) -> FileDiff {
    parse_unified(path, &language_for(path), raw)
}

fn diff_scene() -> FileDiff {
    build(
        "src/render/scene.rs",
        "\
diff --git a/src/render/scene.rs b/src/render/scene.rs
index 4f2a1b0..9c3d2e1 100644
--- a/src/render/scene.rs
+++ b/src/render/scene.rs
@@ -12,9 +12,11 @@ impl Scene {
     /// Reset the scene for a fresh frame.
     pub fn reset(&mut self) {
         self.encoding.reset();
-        self.layers.clear();
+        self.layers.clear();
+        self.bind_cache.invalidate();
     }

-    pub fn push_layer(&mut self, blend: BlendMode) {
+    pub fn push_layer(&mut self, blend: BlendMode, alpha: f32) {
         self.layers.push(Layer { blend, alpha });
     }
@@ -48,6 +50,12 @@ impl Scene {
         for layer in &self.layers {
             encoder.encode_layer(layer);
         }
+        // Flush the compute blur pass before presenting.
+        if self.needs_blur {
+            encoder.dispatch_blur(&self.blur_params);
+            self.needs_blur = false;
+        }
         encoder.finish()
     }
 }",
    )
}

fn diff_pipeline() -> FileDiff {
    build(
        "src/render/pipeline.rs",
        "\
diff --git a/src/render/pipeline.rs b/src/render/pipeline.rs
new file mode 100644
index 0000000..a1b2c3d
--- /dev/null
+++ b/src/render/pipeline.rs
@@ -0,0 +1,20 @@
+//! Reusable render pipeline with a bind-group cache.
+use std::collections::HashMap;
+
+pub struct Pipeline {
+    inner: wgpu::RenderPipeline,
+    bind_cache: HashMap<u64, wgpu::BindGroup>,
+}
+
+impl Pipeline {
+    pub fn new(device: &wgpu::Device, shader: &wgpu::ShaderModule) -> Self {
+        let inner = device.create_render_pipeline(&descriptor(shader));
+        Self { inner, bind_cache: HashMap::new() }
+    }
+
+    pub fn bind(&mut self, key: u64, layout: &wgpu::BindGroupLayout) -> &wgpu::BindGroup {
+        self.bind_cache.entry(key).or_insert_with(|| {
+            create_bind_group(layout)
+        })
+    }
+}",
    )
}

fn diff_rename() -> FileDiff {
    let mut d = build(
        "src/gpu/device.rs",
        "\
diff --git a/src/gpu/adapter.rs b/src/gpu/device.rs
similarity index 92%
rename from src/gpu/adapter.rs
rename to src/gpu/device.rs
index 1122334..5566778 100644
--- a/src/gpu/adapter.rs
+++ b/src/gpu/device.rs
@@ -1,6 +1,6 @@
-//! GPU adapter selection.
+//! GPU device + adapter selection.
 use wgpu::Adapter;

-pub struct AdapterPick {
+pub struct DevicePick {
     pub adapter: Adapter,
 }",
    );
    d.old_path = Some("src/gpu/adapter.rs".into());
    d
}

fn diff_app() -> FileDiff {
    build(
        "src/app.rs",
        "\
diff --git a/src/app.rs b/src/app.rs
index aabbccd..ddeeff0 100644
--- a/src/app.rs
+++ b/src/app.rs
@@ -30,10 +30,11 @@ impl App {
     pub fn redraw(&mut self) {
         let mut scene = Scene::new();
-        self.root.draw(&mut scene);
-        self.surface.present(scene);
+        self.root.draw(&mut scene, self.theme());
+        let frame = self.surface.acquire();
+        self.renderer.render(&scene, &frame);
+        frame.present();
     }

     fn theme(&self) -> &Theme {
-        &self.theme
+        self.themes.active()
     }
 }",
    )
}

fn diff_cargo() -> FileDiff {
    build(
        "Cargo.toml",
        "\
diff --git a/Cargo.toml b/Cargo.toml
index 7788990..0011223 100644
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -8,7 +8,8 @@ edition = \"2024\"
 [dependencies]
 wgpu = \"0.20\"
-vello = \"0.8\"
+vello = \"0.9\"
+pollster = \"0.3\"
 winit = \"0.30\"",
    )
}

fn diff_deleted() -> FileDiff {
    build(
        "src/legacy/blit.rs",
        "\
diff --git a/src/legacy/blit.rs b/src/legacy/blit.rs
deleted file mode 100644
index 9988776..0000000
--- a/src/legacy/blit.rs
+++ /dev/null
@@ -1,8 +0,0 @@
-//! Legacy fixed-function blit path. Superseded by the compute blur.
-pub fn blit(src: &Texture, dst: &Texture) {
-    let mut encoder = device.create_command_encoder(&Default::default());
-    encoder.copy_texture_to_texture(src.as_copy(), dst.as_copy(), src.size());
-    queue.submit([encoder.finish()]);
-}",
    )
}

fn diff_untracked_md() -> FileDiff {
    build(
        "docs/rendering.md",
        "\
diff --git a/docs/rendering.md b/docs/rendering.md
new file mode 100644
index 0000000..1234567
--- /dev/null
+++ b/docs/rendering.md
@@ -0,0 +1,12 @@
+# Rendering pipeline
+
+The renderer encodes a `Scene` once per frame and hands it to vello.
+
+## Passes
+
+1. Layer composition
+2. Compute blur (optional)
+3. Present
+
+See `src/render/pipeline.rs` for the bind-group cache.",
    )
}

fn diff_untracked_wgsl() -> FileDiff {
    build(
        "src/render/shaders/blur.wgsl",
        "\
diff --git a/src/render/shaders/blur.wgsl b/src/render/shaders/blur.wgsl
new file mode 100644
index 0000000..89abcde
--- /dev/null
+++ b/src/render/shaders/blur.wgsl
@@ -0,0 +1,10 @@
+@group(0) @binding(0) var src: texture_2d<f32>;
+@group(0) @binding(1) var dst: texture_storage_2d<rgba8unorm, write>;
+
+@compute @workgroup_size(8, 8)
+fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
+    let coord = vec2<i32>(gid.xy);
+    let c = textureLoad(src, coord, 0);
+    textureStore(dst, coord, c);
+}",
    )
}

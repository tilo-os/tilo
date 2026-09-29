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
    let staged = sample_staged_changes();
    let unstaged = sample_unstaged_changes();
    let untracked = sample_untracked_changes();
    let diffs = sample_diffs();

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
        graph_width: layout.lanes,
        author_name: "Quinten".into(),
        author_email: "ada@example.com".into(),
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
            name: "feature/wgpu-pipeline".into(),
            is_head: false,
            is_remote: false,
            upstream: Some("origin/feature/wgpu-pipeline".into()),
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
            name: "origin/feature/wgpu-pipeline".into(),
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
            message: Some("Release 0.4.0 — vello migration".into()),
        },
        Tag {
            name: "v0.3.2".into(),
            target: "2c3d4e5".into(),
            message: Some("Hotfix: text shaping".into()),
        },
        Tag {
            name: "v0.3.0".into(),
            target: "8a9b0c1".into(),
            message: None,
        },
    ]
}

fn sample_stashes() -> Vec<Stash> {
    vec![
        Stash {
            index: 0,
            message: "WIP on main: experiment with msaa".into(),
            branch: "main".into(),
        },
        Stash {
            index: 1,
            message: "WIP on feature/wgpu-pipeline: bind group cache".into(),
            branch: "feature/wgpu-pipeline".into(),
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

fn commit_history() -> Vec<Commit> {
    let mut commits = commit_history_recent();
    commits.extend(commit_history_older());
    commits
}

/// The four most recent commits: the wgpu-pipeline merge, its two-lane
/// history, and the shared root they both branch from (part of
/// [`commit_history`]). Newest-first; ids are short hex-ish strings and
/// `parents` reference ids — structured to exercise 3 graph lanes.
fn commit_history_recent() -> Vec<Commit> {
    vec![
        Commit {
            id: "a1b2c3d".into(),
            summary: "Merge branch 'feature/wgpu-pipeline'".into(),
            body: "Bring the new wgpu render pipeline onto main.\n\nReplaces the legacy blit path with a compute-driven\nblur and a reusable bind-group cache.".into(),
            author_name: "Quinten".into(),
            author_email: "ada@example.com".into(),
            when: "2 hr ago".into(),
            time: 1_719_750_000,
            parents: vec!["b2c3d4e".into(), "9f8e7d6".into()],
            refs: vec![rl("HEAD", RefKind::Head), rl("main", RefKind::LocalBranch)],
            lane: 0,
            files: vec![
                FileChange { path: "src/render/pipeline.rs".into(), old_path: None, area: ChangeArea::Staged, kind: ChangeKind::Added, additions: 58, deletions: 0 },
            ],
        },
        Commit {
            id: "9f8e7d6".into(),
            summary: "Add compute blur pass".into(),
            body: "Implements a separable gaussian blur in WGSL driven\nfrom a compute pipeline.".into(),
            author_name: "Alex Rivera".into(),
            author_email: "alex@example.com".into(),
            when: "5 hr ago".into(),
            time: 1_719_739_000,
            parents: vec!["3a2b1c0".into()],
            refs: vec![rl("feature/wgpu-pipeline", RefKind::LocalBranch)],
            lane: 1,
            files: vec![],
        },
        Commit {
            id: "b2c3d4e".into(),
            summary: "Cache bind groups per material".into(),
            body: String::new(),
            author_name: "Quinten".into(),
            author_email: "ada@example.com".into(),
            when: "yesterday".into(),
            time: 1_719_660_000,
            parents: vec!["7d6e5f4".into()],
            refs: vec![],
            lane: 0,
            files: vec![],
        },
        Commit {
            id: "3a2b1c0".into(),
            summary: "Sketch the pipeline builder".into(),
            body: String::new(),
            author_name: "Alex Rivera".into(),
            author_email: "alex@example.com".into(),
            when: "yesterday".into(),
            time: 1_719_655_000,
            parents: vec!["7d6e5f4".into()],
            refs: vec![rl("origin/feature/wgpu-pipeline", RefKind::RemoteBranch)],
            lane: 1,
            files: vec![],
        },
    ]
}

/// The four older commits: the 0.4.0 release, the scene-leak fix branch,
/// the text-shaping hotfix, and the initial skeleton (part of
/// [`commit_history`]).
fn commit_history_older() -> Vec<Commit> {
    vec![
        Commit {
            id: "7d6e5f4".into(),
            summary: "Release 0.4.0 — vello migration".into(),
            body: "Cuts over scene encoding to vello 0.9.".into(),
            author_name: "Quinten".into(),
            author_email: "ada@example.com".into(),
            when: "2 days ago".into(),
            time: 1_719_570_000,
            parents: vec!["5c4b3a2".into()],
            refs: vec![
                rl("origin/main", RefKind::RemoteBranch),
                rl("v0.4.0", RefKind::Tag),
            ],
            lane: 0,
            files: vec![],
        },
        Commit {
            id: "5c4b3a2".into(),
            summary: "Fix scene memory leak on resize".into(),
            body:
                "The encoded scene wasn't reset between frames when the\nsurface was reconfigured."
                    .into(),
            author_name: "Priya N.".into(),
            author_email: "priya@example.com".into(),
            when: "3 days ago".into(),
            time: 1_719_480_000,
            parents: vec!["2c3d4e5".into()],
            refs: vec![rl("fix/scene-leak", RefKind::LocalBranch)],
            lane: 0,
            files: vec![],
        },
        Commit {
            id: "2c3d4e5".into(),
            summary: "Hotfix: text shaping advance".into(),
            body: String::new(),
            author_name: "Priya N.".into(),
            author_email: "priya@example.com".into(),
            when: "4 days ago".into(),
            time: 1_719_400_000,
            parents: vec!["8a9b0c1".into()],
            refs: vec![rl("v0.3.2", RefKind::Tag)],
            lane: 0,
            files: vec![],
        },
        Commit {
            id: "8a9b0c1".into(),
            summary: "Initial renderer skeleton".into(),
            body: "First commit. Window, surface, clear color.".into(),
            author_name: "Quinten".into(),
            author_email: "ada@example.com".into(),
            when: "1 week ago".into(),
            time: 1_719_000_000,
            parents: vec![],
            refs: vec![rl("v0.3.0", RefKind::Tag)],
            lane: 0,
            files: vec![],
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

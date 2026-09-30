//! Headless `--shots` mode: render one offscreen frame of the notes
//! app to a PNG and exit. Used for visual review and CI snapshots at a
//! chosen viewport, without opening a window.
//!
//! The workspace is seeded into a throwaway temp directory so a shot is
//! hermetic and never touches the user's real `~/notes`. Sample content
//! only ever lives on this path — the shipped app starts from the
//! user's own pages.

use std::{fmt::Write as _, path::Path};

use crate::model::Workspace;
use crate::storage;

/// Default viewport when `--viewport` is omitted. A 16:9-ish desktop
/// size that shows the sidebar and a full page.
pub const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 720);

/// Parse a `WxH` string like `800x600` into a `(u32, u32)` viewport.
pub fn parse_viewport(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x').or_else(|| s.split_once('X'))?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Seed a small, realistic workspace into `root`: a handful of pages in
/// the tree plus one content-rich note selected so the shot shows real
/// blocks (headings, lists, checklist, code, an inline link).
#[allow(
    clippy::too_many_lines,
    reason = "one visual fixture keeps its mutually linked page corpus readable in one place"
)]
pub(crate) fn seed_sample_workspace(
    root: &Path,
    scenario: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    storage::ensure_layout(root)?;
    let mut ws = Workspace::new();

    let inbox = ws.create_page(None);
    ws.rename(inbox, "Inbox");
    let projects = ws.create_page(None);
    ws.rename(projects, "Projects");
    let launch = ws.create_page(Some(projects));
    ws.rename(launch, "Launch checklist");
    let reading = ws.create_page(None);
    ws.rename(reading, "Reading list");
    let recipes = ws.create_page(None);
    ws.rename(recipes, "Recipes");
    let formulas = ws.create_page(None);
    ws.rename(formulas, "Formula notebook");
    if scenario == Some("sidebar_scroll") {
        for index in 1..=28 {
            let page = ws.create_page(None);
            ws.rename(page, &format!("Reference {index:02}"));
        }
    }

    // Tags are workspace metadata rather than syntax hidden in the note body.
    // The sample keeps the chips visible in ordinary visual review while the
    // resource/action tests exercise their collaborative behavior.
    let release = ws.create_tag("Release");
    let writing = ws.create_tag("Writing");
    let _ = ws.set_tag_assignment(launch, release, true);
    let _ = ws.set_tag_assignment(launch, writing, true);

    ws.set_selected(Some(launch));

    let bodies: [(uuid::Uuid, &str); 6] = [
        (
            launch,
            "\
What has to land before we ship. Owner in **bold**.

## This week

- [x] Finalise the onboarding copy

- [x] Cut the release branch

- [ ] Write the migration guide

- [ ] Record the walkthrough video

## Notes

See [[Reading list]] for sources.

Read the [release writing guide](https://example.test/writing) before publishing.

![[Reading list]]

```sh
git checkout -b release/v1.0
git tag -a v1.0 -m \"release\"
git push origin v1.0 --tags
```
",
        ),
        (
            inbox,
            "\
Quick captures to sort later.

- Call the dentist

- Reply to the venue about the date

- Look into a standing desk
",
        ),
        (
            reading,
            "\
Things worth coming back to.

- *The Timeless Way of Building* — Alexander

- `SICP`, chapter 2

- **Notes on the Synthesis of Form**
",
        ),
        (
            projects,
            "\
Active work, one note each.

- [[Launch checklist]]
",
        ),
        (
            recipes,
            "\
## Weeknight pasta

- 200g pasta

- garlic, chili, good olive oil

- a handful of parsley

## Method

1. Boil the pasta in well-salted water.

2. Warm the oil, garlic and chili gently.

3. Toss the drained pasta through the oil.

4. Finish with parsley off the heat.
",
        ),
        (
            formulas,
            r#"# Formula notebook

Edit the source; the mathematical preview stays live underneath.

## TeX

```tex
\sum_{i=1}^{n} \frac{x_i^2}{\sqrt{1+x_i}} + \begin{bmatrix}a&b\\c&d\end{bmatrix}
```

## Typst

```typst visual
integral_0^infinity frac(1, 1+x^2) + vec(alpha, beta, gamma)
```

## Typst cases

```typst
f(x) = cases(x^2 & "if" x >= 0, -x & "otherwise")
```
"#,
        ),
    ];

    storage::save_workspace(root, &ws)?;
    for (id, body) in bodies {
        storage::write_page_body(root, id, body)?;
    }
    if scenario == Some("adjacent_code_fences") {
        storage::write_page_body(
            root,
            launch,
            "## Two separate examples\n\nEach Python example keeps its own code block.\n\n```python\nmessage = \"First example\"\nprint(message)\n```\n\n```python\nmessage = \"Second example\"\nprint(message.upper())\n```\n\nContinue writing below either block with Ctrl+Enter.\n",
        )?;
    }
    if scenario == Some("scrolled") {
        let mut body = storage::read_page_body(root, launch)?;
        for index in 1..=20 {
            let _ = write!(
                body,
                "\nFollow-up item {index}: keep the document readable while scrolling.\n\n"
            );
        }
        storage::write_page_body(root, launch, &body)?;
    }
    if matches!(scenario, Some("image" | "image_edit")) {
        // Reuse a checked-in PNG fixture to exercise the real decoder and
        // workspace-relative loader without network access.
        let fixture = include_bytes!("../../settings/shots/settings.png");
        crate::files::write(&root.join(storage::PAGES_DIR).join("image.png"), fixture)
            .map_err(std::io::Error::other)?;
    }
    Ok(())
}

/// Render one canonical component capture. Unknown scenarios fail instead of
/// falling back to a second, app-owned renderer.
///
/// # Errors
/// Returns an error for unsupported scenarios or when fixture setup/rendering fails.
pub fn render_to_path(
    viewport: (u32, u32),
    out: &Path,
    scenario: Option<&str>,
    text_scale: Option<f32>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !crate::capture::supports(scenario) {
        return Err(format!(
            "unsupported Notes capture scenario: {}",
            scenario.unwrap_or("overview")
        )
        .into());
    }
    crate::capture::render_to_path(viewport, out, scenario, text_scale).map_err(Into::into)
}

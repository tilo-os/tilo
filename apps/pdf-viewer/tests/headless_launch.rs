//! Exercise the actual PDF CLI, its asynchronous owner worker and durable records.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions fail closed when the actual CLI cannot be observed"
)]

use serde_json::{Value, json};
use std::path::Path;
use std::process::{Command, Stdio};

#[cfg(windows)]
fn assert_default_main_stack() {
    use std::io::{Read, Seek, SeekFrom};

    // This regression runs the actual application, whose main thread does not
    // inherit libtest's larger worker stack. Keep it sensitive to excess launch
    // temporaries instead of hiding them with a larger linker stack reserve.
    let mut image = std::fs::File::open(env!("CARGO_BIN_EXE_pdf-viewer")).unwrap();
    let mut word = [0; 2];
    image.read_exact(&mut word).unwrap();
    assert_eq!(&word, b"MZ");
    image.seek(SeekFrom::Start(60)).unwrap();
    let mut dword = [0; 4];
    image.read_exact(&mut dword).unwrap();
    image
        .seek(SeekFrom::Start(u64::from(u32::from_le_bytes(dword))))
        .unwrap();
    image.read_exact(&mut dword).unwrap();
    assert_eq!(&dword, b"PE\0\0");
    image.seek(SeekFrom::Current(20)).unwrap();
    image.read_exact(&mut word).unwrap();
    assert_eq!(
        u16::from_le_bytes(word),
        0x20b,
        "the actual Windows application must have a PE32+ image"
    );
    image.seek(SeekFrom::Current(70)).unwrap();
    let mut qword = [0; 8];
    image.read_exact(&mut qword).unwrap();
    let reserve = u64::from_le_bytes(qword);
    assert!(
        reserve > 0 && reserve <= 1024 * 1024,
        "exercise the default main-thread stack, not an enlarged reserve: {reserve}"
    );
}

/// The pdfium library the reader would find on this host, resolved before the
/// child's environment is isolated.
fn host_pdfium() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("TILO_PDFIUM_LIB").filter(|path| !path.is_empty()) {
        return Some(path.into());
    }
    let home = std::env::var_os("HOME")?;
    let path = Path::new(&home).join(".local/lib/pdfium/libpdfium.so");
    path.exists().then_some(path)
}

fn run(root: &Path, name: &str, source: &Path, extra: Vec<Value>) -> (Value, Value) {
    let mut actions = vec![
        json!({"expect": {"target": "pdf_app", "semantic_target": "pdf.page:source-1", "name": "Page 1", "selected": true, "timeout_ms": 30000}}),
    ];
    actions.extend(extra);
    let dump = root.join(format!("{name}-state.json"));
    let inspect = root.join(format!("{name}-inspect.json"));
    let mut command = Command::new(env!("CARGO_BIN_EXE_pdf-viewer"));
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("TILO_") || name.starts_with("QUICK_") {
            command.env_remove(key);
        }
    }
    for key in [
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
        "LOCALAPPDATA",
        "APPDATA",
        "HOME",
        "USERPROFILE",
        "TEMP",
        "TMP",
        "TMPDIR",
    ] {
        let directory = root.join(key);
        std::fs::create_dir_all(&directory).unwrap();
        command.env(key, directory);
    }
    // The isolated HOME hides a per-user pdfium install, and opening a PDF
    // requires the native engine, so hand the host's library over explicitly.
    if let Some(pdfium) = host_pdfium() {
        command.env("TILO_PDFIUM_LIB", pdfium);
    }
    command
        .current_dir(root)
        .env("TILO_PACKAGE_ID", "so.tilo.pdf-viewer")
        .env("TILO_WORKSPACE_ID", "00000000-0000-0000-0000-000000000000")
        .arg("--headless-drive")
        .arg(
            json!({"viewport": [1280, 800], "actions": actions, "dump": dump, "inspect": inspect})
                .to_string(),
        )
        .arg(source);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let stdout = root.join(format!("{name}-stdout.txt"));
    let stderr = root.join(format!("{name}-stderr.txt"));
    command
        .stdout(Stdio::from(std::fs::File::create(&stdout).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(&stderr).unwrap()));
    let mut child = command.spawn().unwrap();
    let deadline = tilo_test_env::hang_deadline();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if ui::time::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!(
                "actual PDF CLI timed out at {name}: {}",
                std::fs::read_to_string(&stderr).unwrap()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(
        status.success(),
        "actual PDF CLI failed at {name}: {}\n{}",
        std::fs::read_to_string(stdout).unwrap(),
        std::fs::read_to_string(stderr).unwrap()
    );
    (
        serde_json::from_slice(&std::fs::read(dump).unwrap()).unwrap(),
        serde_json::from_slice(&std::fs::read(inspect).unwrap()).unwrap(),
    )
}

/// A dumped comment's text: the paragraphs of its rich text, a line each.
fn comment_text(comment: &Value) -> String {
    let mut blocks: Vec<(String, String)> = comment["text"]["blocks"]
        .as_object()
        .map(|blocks| {
            blocks
                .values()
                .map(|block| {
                    (
                        block["order"].to_string(),
                        block["text"].as_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    blocks.sort();
    blocks
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn find_canvas<'a>(node: &'a Value, found: &mut Vec<&'a Value>) {
    if node["name"] == "Page 1 of 3" {
        found.push(node);
    }
    if let Some(children) = node["children"].as_array() {
        for child in children {
            find_canvas(child, found);
        }
    }
}

#[test]
fn cli_note_survives_other_document_and_restart() {
    #[cfg(windows)]
    assert_default_main_stack();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let bytes =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pdf"))
            .unwrap();
    let a = root.join("alpha document.pdf");
    let b = root.join("beta document.pdf");
    std::fs::write(&a, &bytes).unwrap();
    std::fs::write(&b, &bytes).unwrap();
    let (initial, inspection) = run(&root, "a-first", &a, vec![]);
    assert_eq!(inspection["root"]["widget"], "PdfApp");
    assert_eq!(initial["page-plan"].as_array().unwrap().len(), 3);
    assert_eq!(initial["annotations"].as_array().unwrap().len(), 0);
    let record = initial["tilo:record"]["id"].as_str().unwrap();
    let mut canvases = vec![];
    find_canvas(&inspection["root"]["semantics"], &mut canvases);
    assert_eq!(canvases.len(), 1, "production canvas must be observable");
    let bounds = &canvases[0]["bounds"];
    let x0 = bounds["x0"].as_f64().unwrap();
    let y0 = bounds["y0"].as_f64().unwrap();
    let x1 = bounds["x1"].as_f64().unwrap();
    let y1 = bounds["y1"].as_f64().unwrap();
    assert!(x1 > x0 && y1 > y0);
    let marker = "durable PDF comment";
    let (edited, _) = run(
        &root,
        "a-edit",
        &a,
        vec![
            json!({"perform": {"target": "pdf_app", "semantic_target": "pdf.tool:4", "action": "activate"}}),
            json!({"expect": {"target": "pdf_app", "semantic_target": "pdf.tool:4", "checked": true}}),
            json!({"click_at": [f64::midpoint(x0, x1), f64::midpoint(y0, y1)]}),
            json!({"type": marker}),
            json!({"key": "Enter"}),
        ],
    );
    assert_eq!(edited["annotations"].as_array().unwrap().len(), 1);
    assert_eq!(edited["comments"].as_array().unwrap().len(), 1);
    assert_eq!(edited["annotations"][0]["kind"], "note");
    assert_eq!(comment_text(&edited["comments"][0]), marker);
    assert_eq!(edited["tilo:record"]["id"], record);
    let (other, _) = run(&root, "b-open", &b, vec![]);
    assert_ne!(other["tilo:record"]["id"], record);
    assert_eq!(other["annotations"].as_array().unwrap().len(), 0);
    assert_eq!(other["comments"].as_array().unwrap().len(), 0);
    let (reopened, _) = run(&root, "a-restart", &a, vec![]);
    assert_eq!(reopened["tilo:record"]["id"], record);
    assert_eq!(reopened["annotations"], edited["annotations"]);
    assert_eq!(reopened["comments"], edited["comments"]);
    assert_eq!(std::fs::read(a).unwrap(), bytes);
    assert_eq!(std::fs::read(b).unwrap(), bytes);
    let package = root.join("XDG_DATA_HOME/so.tilo.pdf-viewer");
    assert!(
        !package.join("state.automerge").exists(),
        "PDF CLI must use its native room index, not a package-global substitute"
    );
    let room_dir = package.join("rooms/00000000-0000-0000-0000-000000000000");
    let index =
        automerge::AutoCommit::load(&std::fs::read(room_dir.join("state.automerge")).unwrap())
            .unwrap();
    assert!(ui::record::room::holds(
        &index,
        &ui::RecordId::parse(record).unwrap()
    ));
}

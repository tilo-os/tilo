//! `tilo system health --gpu`: is this session rendering on its GPU?
//!
//! The running compositor keeps what it decided about the GPU in
//! `$XDG_RUNTIME_DIR/tilo/gpu-health.json` — the render node it advertised to
//! apps, the Vulkan driver it pinned, the adapter every app window reported and
//! every reason something is not on the GPU. This command reads that record and
//! answers with one of three states:
//!
//! - `ok` — a GPU exists and the session and its apps use it;
//! - `degraded` — a GPU exists and something is not on it (exit status 1);
//! - `no-gpu` — the machine has no render node with a GPU behind it, so
//!   software rendering is the only rendering there is. A virtio GPU without 3D
//!   acceleration registers a render node all the same; the compositor records
//!   such nodes as `nodes_without_gpu`, and they do not count.
//!
//! It is the hardware check for GPU support: CI machines have no Adreno, Mali or
//! discrete GPU, so on a real device run `tilo system health --gpu` after
//! opening an app or two.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};

fn render_nodes(dev_dri: &Path) -> Vec<PathBuf> {
    let mut nodes: Vec<PathBuf> = std::fs::read_dir(dev_dri)
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("renderD"))
        })
        .collect();
    nodes.sort();
    nodes
}

/// `value[key]`, or `null` when absent.
fn at<'v>(value: &'v Value, key: &str) -> &'v Value {
    value.get(key).unwrap_or(&Value::Null)
}

fn record_path() -> PathBuf {
    tilo_paths::xdg::runtime_dir_or_default().join("tilo/gpu-health.json")
}

/// The verdict for `nodes` and the compositor's `record` (`None` when no
/// session has written one).
pub fn assess(nodes: &[PathBuf], record: Option<&Value>) -> Value {
    let without_gpu = record.map_or(&Value::Null, |record| at(record, "nodes_without_gpu"));
    if nodes
        .iter()
        .all(|node| !at(without_gpu, &node.display().to_string()).is_null())
    {
        return json!({
            "status": "no-gpu",
            "problems": [],
            "render_nodes": nodes.iter().map(|n| n.display().to_string()).collect::<Vec<_>>(),
            "session": record.cloned().unwrap_or(Value::Null),
        });
    }
    let mut problems: Vec<String> = Vec::new();
    match record {
        None => problems.push(
            "no running Tilo session has reported its GPU (start a session, or check \
             that it can write $XDG_RUNTIME_DIR/tilo)"
                .to_string(),
        ),
        Some(record) => {
            problems.extend(
                at(record, "problems")
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned),
            );
            if at(record, "dmabuf_device").is_null()
                && !problems.iter().any(|p| p.contains("zwp_linux_dmabuf_v1"))
            {
                problems.push(
                    "zwp_linux_dmabuf_v1 is not advertised, so apps cannot present from a GPU"
                        .to_string(),
                );
            }
            for (package, adapter) in at(record, "app_adapters").as_object().into_iter().flatten() {
                if adapter
                    .as_str()
                    .is_some_and(|line| line.contains("type=Cpu"))
                {
                    let problem = format!(
                        "{package} renders on a CPU adapter: {}",
                        adapter.as_str().unwrap_or("")
                    );
                    problems.push(problem);
                }
            }
        }
    }
    let status = if problems.is_empty() {
        "ok"
    } else {
        "degraded"
    };
    json!({
        "status": status,
        "problems": problems,
        "render_nodes": nodes.iter().map(|n| n.display().to_string()).collect::<Vec<_>>(),
        "session": record.cloned().unwrap_or(Value::Null),
    })
}

/// Print the GPU health verdict; exit status 1 when degraded.
pub fn run_gpu(json_output: bool) -> Result<std::process::ExitCode> {
    let nodes = render_nodes(Path::new("/dev/dri"));
    let record = std::fs::read(record_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let verdict = assess(&nodes, record.as_ref());
    if json_output {
        println!("{}", serde_json::to_string_pretty(&verdict)?);
    } else {
        print_human(&verdict);
    }
    Ok(if at(&verdict, "status") == "degraded" {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    })
}

fn print_human(verdict: &Value) {
    let session = at(verdict, "session");
    match at(verdict, "status").as_str() {
        Some("no-gpu") => {
            println!(
                "GPU: none — this machine has no render node with a GPU behind it; everything \
                 renders in software"
            );
            for (node, why) in at(session, "nodes_without_gpu")
                .as_object()
                .into_iter()
                .flatten()
            {
                println!("  {node:<20} {}", why.as_str().unwrap_or(""));
            }
            return;
        }
        Some("ok") => println!("GPU: ok"),
        _ => println!("GPU: degraded"),
    }
    let field = |key: &str| at(session, key).as_str().unwrap_or("—").to_owned();
    println!("  render node     {} ({})", field("node"), field("driver"));
    println!("  vulkan device   {}", field("vulkan_device"));
    println!("  dmabuf device   {}", field("dmabuf_device"));
    for (package, adapter) in at(session, "app_adapters")
        .as_object()
        .into_iter()
        .flatten()
    {
        println!("  {package:<28} {}", adapter.as_str().unwrap_or(""));
    }
    for problem in at(verdict, "problems").as_array().into_iter().flatten() {
        println!("  problem: {}", problem.as_str().unwrap_or(""));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes() -> Vec<PathBuf> {
        vec![PathBuf::from("/dev/dri/renderD128")]
    }

    #[test]
    fn a_machine_without_render_nodes_has_no_gpu_to_miss() {
        let verdict = assess(&[], None);
        assert_eq!(verdict["status"], "no-gpu");
    }

    #[test]
    fn render_nodes_without_a_gpu_behind_them_are_no_gpu() {
        let record = json!({
            "node": null,
            "dmabuf_device": null,
            "nodes_without_gpu": {
                "/dev/dri/renderD128": "virtio GPU without 3D acceleration"
            },
            "problems": [],
            "app_adapters": {
                "so.tilo.notes": "window adapter \"llvmpipe (LLVM 22.1.0, 128 bits)\" type=Cpu backend=Vulkan driver=\"llvmpipe\""
            }
        });
        let verdict = assess(&nodes(), Some(&record));
        assert_eq!(verdict["status"], "no-gpu", "{verdict}");

        // A second node the compositor did not set aside is a GPU the session must use.
        let both = [
            PathBuf::from("/dev/dri/renderD128"),
            PathBuf::from("/dev/dri/renderD129"),
        ];
        let verdict = assess(&both, Some(&record));
        assert_eq!(verdict["status"], "degraded", "{verdict}");
    }

    #[test]
    fn a_healthy_session_is_ok() {
        let record = json!({
            "node": "/dev/dri/renderD128",
            "driver": "msm_dpu",
            "vulkan_device": "Turnip Adreno (TM) X1-85",
            "dmabuf_device": "/dev/dri/renderD128",
            "problems": [],
            "app_adapters": {
                "so.tilo.file-explorer": "window adapter \"Turnip Adreno (TM) X1-85\" type=IntegratedGpu backend=Vulkan driver=\"turnip Mesa driver\""
            }
        });
        let verdict = assess(&nodes(), Some(&record));
        assert_eq!(verdict["status"], "ok", "{verdict}");
    }

    #[test]
    fn a_cpu_adapter_or_a_missing_dmabuf_global_is_degraded() {
        let record = json!({
            "node": "/dev/dri/renderD128",
            "dmabuf_device": null,
            "problems": [],
            "app_adapters": {
                "so.tilo.notes": "window adapter \"llvmpipe (LLVM 22.1.0, 128 bits)\" type=Cpu backend=Vulkan driver=\"llvmpipe\""
            }
        });
        let verdict = assess(&nodes(), Some(&record));
        assert_eq!(verdict["status"], "degraded");
        let problems = verdict["problems"].to_string();
        assert!(problems.contains("zwp_linux_dmabuf_v1"), "{problems}");
        assert!(
            problems.contains("so.tilo.notes renders on a CPU adapter"),
            "{problems}"
        );

        let verdict = assess(&nodes(), None);
        assert_eq!(
            verdict["status"], "degraded",
            "no session record is not health"
        );
    }
}

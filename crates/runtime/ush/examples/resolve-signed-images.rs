//! Developer probe of the existing image provider; never mounts or launches tools.
use tilo_app_package::PackageId;
use ush::images::{Images, StoreImages};

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: resolve-signed-images RUNTIME_PACKAGE TOOL_PACKAGE".into());
    }
    let runtime: PackageId = args
        .first()
        .ok_or("missing runtime package")?
        .parse()
        .map_err(|error| format!("{error}"))?;
    let tool: PackageId = args
        .get(1)
        .ok_or("missing tool package")?
        .parse()
        .map_err(|error| format!("{error}"))?;
    let images = StoreImages::from_env()?;
    images.check_tool(&tool, &runtime)?;
    let runtime_root = images.tree(&runtime)?;
    let tool_root = images.tree(&tool)?;
    println!(
        "{}",
        serde_json::json!({
            "runtime": {"package_id": runtime.to_string(), "root": runtime_root},
            "tool": {"package_id": tool.to_string(), "root": tool_root},
        })
    );
    Ok(())
}

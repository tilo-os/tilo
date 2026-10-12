use std::process::ExitCode;

use clap::Parser;
use serde_json::{Value, json};
use tilo_vm::app::App;
use tilo_vm::cli::{Cli, writes_receipt};
use tilo_vm::context::Worktree;
use tilo_vm::hypervisor::SystemRunner;
use tilo_vm::receipt::Receipt;

fn main() -> ExitCode {
    let cli = Cli::parse().normalized();
    let cwd = std::env::current_dir().unwrap_or_default();
    let worktree = Worktree::detect(&cwd);
    // Receipts go to the worktree's target/, or a per-user dir outside one.
    let base = worktree.as_ref().map_or_else(
        || {
            tilo_paths::data_local_dir()
                .unwrap_or_else(|| cwd.clone())
                .join("tilo-vm")
        },
        |w| w.root.clone(),
    );
    let receipt = Receipt::new(&base, std::env::args().collect());
    let json_mode = cli.json;
    let keep_receipt = writes_receipt(cli.command.as_ref());
    let mut app = App {
        json: json_mode,
        target: cli.target,
        // External tools (os/dev/vm.sh, cargo) run from the worktree root.
        runner: SystemRunner {
            cwd: worktree.as_ref().map(|w| w.root.clone()),
        },
        worktree,
        registry_path: tilo_vm::config::registry_path(),
        receipt,
        warnings: Vec::new(),
        peer: None,
    };
    let outcome = app.run(cli.command);
    let App {
        receipt, warnings, ..
    } = app;
    let finish =
        |result: Result<&Value, &Value>| keep_receipt.then(|| receipt.finish(result)).flatten();
    match outcome {
        Ok(outcome) => {
            let mut data = outcome.data;
            if let Value::Object(map) = &mut data {
                map.insert("next".into(), json!(outcome.next));
                map.insert("warnings".into(), json!(warnings));
            }
            let path = finish(Ok(&data));
            if json_mode {
                if let (Value::Object(map), Some(path)) = (&mut data, &path) {
                    map.insert("receipt".into(), json!(path));
                }
                println!("{data:#}");
            } else {
                if !outcome.text.is_empty() {
                    println!("{}", outcome.text);
                }
                for warning in &warnings {
                    eprintln!("warning: {warning}");
                }
                if !outcome.next.is_empty() {
                    println!("next: {}", outcome.next.join("   |   "));
                }
                if let Some(path) = path {
                    eprintln!("receipt: {}", path.display());
                }
            }
            if outcome.failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            let data = error.to_json();
            let path = finish(Err(&data));
            if json_mode {
                let mut data = data;
                if let (Value::Object(map), Some(path)) = (&mut data, &path) {
                    map.insert("receipt".into(), json!(path));
                }
                println!("{data:#}");
            } else {
                eprintln!("{}", error.render());
                if let Some(path) = path {
                    eprintln!("receipt: {}", path.display());
                }
            }
            ExitCode::FAILURE
        }
    }
}

use std::process::ExitCode;

use clap::Parser;
use serde_json::{Value, json};
use tilo_dev::app::App;
use tilo_dev::cli::{Cli, Command};
use tilo_dev::context::Worktree;
use tilo_dev::hypervisor::SystemRunner;
use tilo_dev::receipt::Receipt;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let cwd = std::env::current_dir().unwrap_or_default();
    let worktree = Worktree::detect(&cwd);
    // Receipts go to the worktree's target/, or a per-user dir outside one.
    let base = worktree.as_ref().map_or_else(
        || {
            dirs::data_local_dir()
                .unwrap_or_else(|| cwd.clone())
                .join("tilo-dev")
        },
        |w| w.root.clone(),
    );
    let receipt = Receipt::new(&base, std::env::args().collect());
    let json_mode = cli.json;
    let quiet_receipt = matches!(cli.command, Some(Command::Guide { .. }));
    let mut app = App {
        json: json_mode,
        worktree,
        registry_path: tilo_dev::config::registry_path(),
        runner: SystemRunner { cwd: None },
        receipt,
        warnings: Vec::new(),
    };
    let outcome = app.run(cli.command);
    let App {
        receipt, warnings, ..
    } = app;
    match outcome {
        Ok(outcome) => {
            let mut data = outcome.data;
            if let Value::Object(map) = &mut data {
                map.insert("next".into(), json!(outcome.next));
                map.insert("warnings".into(), json!(warnings));
            }
            let path = receipt.finish(Ok(&data));
            if json_mode {
                if let Value::Object(map) = &mut data {
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
                if let (Some(path), false) = (path, quiet_receipt) {
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
            let path = receipt.finish(Err(&data));
            if json_mode {
                let mut data = data;
                if let Value::Object(map) = &mut data {
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

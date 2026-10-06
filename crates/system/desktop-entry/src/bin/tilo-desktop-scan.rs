//! `tilo-desktop-scan` — what the desktop sees when it looks for host
//! applications.
//!
//! Prints the directories searched, the entries found, and — the part
//! that makes it a diagnostic rather than a listing — the count of
//! everything that was *declined*, with the reason. "My app is not in
//! the palette" has half a dozen legitimate causes, and a tool that
//! reports only the survivors cannot tell any of them apart.

use std::process::ExitCode;

use desktop_entry::launch::{LaunchEnv, plan};
use desktop_entry::scan::{Scan, application_dirs, scan_dirs};

/// What `parse_args` found: either the flags to run with, or an outcome
/// (help printed, a usage error) that ends the program right away.
enum Args {
    Run {
        show_exec: bool,
        filter: Option<String>,
        plan_id: Option<String>,
    },
    Exit(ExitCode),
}

fn parse_args(args: &[String]) -> Args {
    let mut show_exec = false;
    let mut filter: Option<String> = None;
    let mut plan_id: Option<String> = None;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--exec" => show_exec = true,
            "--plan" => {
                if let Some(id) = it.next() {
                    plan_id = Some(id.clone());
                } else {
                    eprintln!("--plan needs a desktop-file id");
                    return Args::Exit(ExitCode::from(2));
                }
            }
            "-h" | "--help" => {
                print_help();
                return Args::Exit(ExitCode::SUCCESS);
            }
            other if other.starts_with('-') => {
                eprintln!("unknown flag {other}");
                print_help();
                return Args::Exit(ExitCode::from(2));
            }
            other => filter = Some(other.to_lowercase()),
        }
    }
    Args::Run {
        show_exec,
        filter,
        plan_id,
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (show_exec, filter, plan_id) = match parse_args(&args) {
        Args::Run {
            show_exec,
            filter,
            plan_id,
        } => (show_exec, filter, plan_id),
        Args::Exit(code) => return code,
    };

    let dirs = application_dirs();
    let scan = scan_dirs(&dirs, &desktop_entry::scan::current_desktops());

    if let Some(id) = plan_id {
        return print_plan(&scan, &id);
    }

    print_listing(&scan, show_exec, filter.as_deref());
    ExitCode::SUCCESS
}

/// The `--plan <id>` output: the exact argv and environment a launch would use.
fn print_plan(scan: &Scan, id: &str) -> ExitCode {
    let Some(entry) = scan.find(id) else {
        eprintln!("no entry with desktop-file id {id}");
        return ExitCode::from(1);
    };
    match plan(entry, &LaunchEnv::default()) {
        Ok(launch) => {
            println!("id        : {}", launch.desktop_id);
            println!("name      : {}", launch.display_name);
            println!("source    : {}", launch.source.label());
            println!("confined  : {}", launch.is_confined());
            println!("program   : {}", launch.program.display());
            println!("args      : {:?}", launch.args);
            println!("windows   : {:?}", launch.window_identities);
            println!("env       :");
            for (k, v) in &launch.env {
                println!("  {k}={v}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cannot launch {id}: {e}");
            ExitCode::from(1)
        }
    }
}

/// The default output: the search path, every launchable entry (optionally filtered), and the
/// decline counts.
fn print_listing(scan: &Scan, show_exec: bool, filter: Option<&str>) {
    println!("search path (precedence order):");
    for dir in &scan.dirs {
        let exists = if dir.is_dir() { "" } else { "   (absent)" };
        println!("  {}{exists}", dir.display());
    }
    println!();

    let shown: Vec<_> = scan
        .entries
        .iter()
        .filter(|e| match filter {
            None => true,
            Some(needle) => {
                e.name.to_lowercase().contains(needle) || e.id.to_lowercase().contains(needle)
            }
        })
        .collect();

    for entry in &shown {
        println!(
            "{:<10}  {:<44}  {}",
            entry.source.label(),
            entry.id,
            entry.name
        );
        if show_exec {
            println!(
                "{:<10}  {:<44}  Exec={}",
                "",
                "",
                entry.exec.as_deref().unwrap_or("")
            );
        }
    }

    println!();
    match filter {
        Some(needle) => println!(
            "{} launchable application(s) matching {needle:?}, out of {}",
            shown.len(),
            scan.entries.len()
        ),
        None => println!("{} launchable application(s)", scan.entries.len()),
    }
    let s = &scan.stats;
    println!(
        "declined: {} shadowed, {} duplicate aliases, {} tombstoned, {} not-an-application, {} no-exec, \
         {} no-display, {} other-desktop, {} try-exec-missing, {} unparseable \
         (out of {} files seen)",
        s.shadowed,
        s.duplicate_aliases,
        s.tombstoned,
        s.not_an_application,
        s.no_exec,
        s.no_display,
        s.other_desktop,
        s.try_exec_missing,
        s.unparseable,
        s.files_seen,
    );
}

fn print_help() {
    println!(
        "tilo-desktop-scan [--exec] [--plan <desktop-id>] [name-or-id-substring]\n\
         \n\
         Lists the host applications Tilo can launch, and accounts for every\n\
         .desktop file it declined.\n\
         \n\
         --exec            also print each entry's raw Exec= line\n\
         --plan <id>       print the exact argv and environment a launch would use"
    );
}

//! `tilolang check <dir> [--json]`, `tilolang build <dir>`, `tilolang ids <dir> --rename old new`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use tilolang::{Diagnostic, IdsLock, PackageMeta};

const USAGE: &str = "usage:
  tilolang check <dir> [--json] [--import <app.wasm>]...
  tilolang build <dir> [--import <app.wasm>]...
  tilolang ids <dir> [--rename <old> <new>]";

struct Options {
    dir: PathBuf,
    json: bool,
    imports: Vec<PathBuf>,
    rename: Option<(String, String)>,
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut dir = None;
    let mut json = false;
    let mut imports = Vec::new();
    let mut rename = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "--import" => imports.push(PathBuf::from(
                it.next().ok_or("--import needs a path to an app.wasm")?,
            )),
            "--rename" => {
                let old = it.next().ok_or("--rename needs the old and the new name")?;
                let new = it.next().ok_or("--rename needs the old and the new name")?;
                rename = Some((old.clone(), new.clone()));
            }
            other if other.starts_with("--") => return Err(format!("unknown option `{other}`")),
            other => dir = Some(PathBuf::from(other)),
        }
    }
    Ok(Options {
        dir: dir.ok_or("a package directory is required")?,
        json,
        imports,
        rename,
    })
}

fn read_imports(paths: &[PathBuf]) -> Result<Vec<PackageMeta>, String> {
    paths
        .iter()
        .map(|p| {
            let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
            PackageMeta::from_wasm(&bytes).ok_or_else(|| {
                format!(
                    "{}: not a tilolang package (no tilolang.meta section)",
                    p.display()
                )
            })
        })
        .collect()
}

fn read_ids(dir: &Path) -> Result<IdsLock, String> {
    match std::fs::read_to_string(dir.join("app.ids")) {
        Ok(text) => IdsLock::parse(&text).map_err(|e| e.to_string()),
        Err(_) => Ok(IdsLock::new()),
    }
}

fn report(diags: &[Diagnostic], path: &str, json: bool) {
    if json {
        println!("{}", tilolang::diagnostics_json(diags));
    } else {
        for d in diags {
            eprintln!("{}", d.render(path));
        }
    }
}

fn run(args: &[String]) -> Result<bool, String> {
    let (cmd, rest) = args.split_first().ok_or(USAGE)?;
    let opts = parse_options(rest)?;
    let source_path = opts.dir.join("app.tl");
    match cmd.as_str() {
        "check" | "build" => {
            let source = std::fs::read_to_string(&source_path)
                .map_err(|e| format!("{}: {e}", source_path.display()))?;
            let imports = read_imports(&opts.imports)?;
            let ids = read_ids(&opts.dir)?;
            let shown = source_path.display().to_string();
            if cmd == "check" {
                let diags = tilolang::check(&source, &ids, &imports);
                report(&diags, &shown, opts.json);
                if opts.json && diags.is_empty() {
                    println!("[]");
                }
                return Ok(diags.is_empty());
            }
            match tilolang::compile_package(&source, &ids, &imports) {
                Ok(c) => {
                    std::fs::write(opts.dir.join("app.wasm"), &c.wasm)
                        .map_err(|e| e.to_string())?;
                    std::fs::write(opts.dir.join("app.ids"), c.ids.to_text())
                        .map_err(|e| e.to_string())?;
                    Ok(true)
                }
                Err(diags) => {
                    report(&diags, &shown, opts.json);
                    Ok(false)
                }
            }
        }
        "ids" => {
            let mut ids = read_ids(&opts.dir)?;
            if let Some((old, new)) = &opts.rename {
                ids.rename(old, new).map_err(|e| e.to_string())?;
                std::fs::write(opts.dir.join("app.ids"), ids.to_text())
                    .map_err(|e| e.to_string())?;
            } else {
                print!("{}", ids.to_text());
            }
            Ok(true)
        }
        _ => Err(USAGE.to_string()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::from(2)
        }
    }
}

//! The reference page of the `tilo` command, for the documentation.
//!
//! `tilo help-markdown` prints it. It walks the command tree itself, built the
//! way a computer with everything installed sees it, so what the page says is
//! what the command parses: a verb added, renamed or reworded reaches the page
//! with no second place to keep in step. `tools/docs/reference/cli.py` writes
//! the page's title and lead around these sections.
//!
//! Hidden commands are left out: they are what another command or a test
//! runs, not what a person types.

use std::fmt::Write as _;

use clap::builder::StyledStr;
use clap::{Arg, Command};

/// The sections of the page: `##` for each noun, `###` for each verb under it.
///
/// A visible command or argument with no help text is an error: the page is a
/// reference, so nothing on it may stand as a bare name a reader cannot place.
pub fn markdown() -> Result<String, String> {
    let everything = crate::Here {
        desktop: true,
        apps: true,
        system: true,
        has: |_| true,
    };
    let root = crate::command_here(everything).bin_name("tilo");
    let mut page = String::new();
    page.push_str("## Global options\n\n");
    page.push_str("These go with every command, before or after its own:\n\n");
    page.push_str(&argument_table(&root, "tilo", &[])?);
    page.push_str("\n\n");
    // A global option is documented once, above, not under every command that takes it.
    let globals: Vec<String> = root
        .get_arguments()
        .filter(|argument| !argument.is_hide_set())
        .map(|argument| argument.get_id().as_str().to_owned())
        .collect();
    commands(&root, "tilo", 2, &globals, &mut page)?;
    Ok(page.trim_end().to_owned())
}

/// Every command under `command`, in the order help lists them.
fn commands(
    command: &Command,
    path: &str,
    level: usize,
    globals: &[String],
    out: &mut String,
) -> Result<(), String> {
    for subcommand in command.get_subcommands() {
        if subcommand.is_hide_set() || subcommand.get_name() == "help" {
            continue;
        }
        let name = subcommand.get_name();
        let full = format!("{path} {name}");
        let mut command = subcommand.clone().bin_name(&full);
        let heading = level.min(6);
        let _ = writeln!(out, "{} {full}\n", "#".repeat(heading));
        let usage = command
            .render_usage()
            .to_string()
            .trim()
            .trim_start_matches("Usage: ")
            .replace('\n', " ");
        let _ = writeln!(out, "```sh fragment\n{usage}\n```\n");
        let about = text(command.get_long_about().or_else(|| command.get_about()))
            .ok_or_else(|| format!("`{full}` has no help text: give it one"))?;
        let _ = writeln!(out, "{about}\n");
        if command.is_disable_help_flag_set() {
            let _ = writeln!(
                out,
                "Its arguments and options are the program's own that this runs; \
                 `{full} --help` prints them.\n"
            );
        } else {
            let table = argument_table(&command, &full, globals)?;
            if !table.is_empty() {
                let _ = writeln!(out, "{table}\n");
            }
        }
        if let Some(after) = text(command.get_after_help()) {
            let _ = writeln!(out, "{}\n", examples(&after));
        }
        commands(&command, &full, level.saturating_add(1), globals, out)?;
    }
    Ok(())
}

/// A command's arguments as a table, its examples and hidden ones left out.
fn argument_table(command: &Command, path: &str, globals: &[String]) -> Result<String, String> {
    let mut rows: Vec<String> = Vec::new();
    for argument in command.get_arguments() {
        let id = argument.get_id().as_str();
        if argument.is_hide_set()
            || matches!(id, "help" | "version")
            || globals.iter().any(|global| global == id)
        {
            continue;
        }
        let name = written_as(argument);
        let kind = if argument.get_long().is_none() && argument.get_short().is_none() {
            "argument"
        } else if takes_value(argument) {
            "option"
        } else {
            "flag"
        };
        let required = if argument.is_required_set() {
            "yes"
        } else {
            "no"
        };
        let what = what_it_does(argument)
            .ok_or_else(|| format!("`{path}`: `{name}` has no help text: give it one"))?;
        rows.push(format!("| `{name}` | {kind} | {required} | {what} |"));
    }
    if rows.is_empty() {
        return Ok(String::new());
    }
    let mut table =
        String::from("| Option | Kind | Required | What it does |\n| --- | --- | --- | --- |\n");
    for row in rows {
        table.push_str(&row);
        table.push('\n');
    }
    Ok(table.trim_end().to_owned())
}

/// How an argument is typed on the command line.
fn written_as(argument: &Arg) -> String {
    let value = if takes_value(argument) {
        argument
            .get_value_names()
            .map_or_else(String::new, |names| {
                names
                    .iter()
                    .map(|name| format!("<{name}>"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
    } else {
        String::new()
    };
    let mut written = String::new();
    if let Some(short) = argument.get_short() {
        let _ = write!(written, "-{short}, ");
    }
    if let Some(long) = argument.get_long() {
        let _ = write!(written, "--{long}");
    }
    if written.is_empty() {
        let name = argument
            .get_value_names()
            .and_then(|names| names.first())
            .map_or_else(
                || argument.get_id().as_str().to_uppercase(),
                ToString::to_string,
            );
        return format!("<{name}>");
    }
    if !value.is_empty() {
        let _ = write!(written, " {value}");
    }
    written
}

fn takes_value(argument: &Arg) -> bool {
    argument.get_action().takes_values()
}

/// What an argument does, and the values it takes when it names them.
fn what_it_does(argument: &Arg) -> Option<String> {
    let help = text(argument.get_long_help().or_else(|| argument.get_help()))
        .unwrap_or_default()
        .replace('\n', " ");
    let values = argument.get_possible_values();
    let named: Vec<&str> = values
        .iter()
        .filter(|value| !value.is_hide_set())
        .map(clap::builder::PossibleValue::get_name)
        .collect();
    let choices = if named.is_empty() {
        String::new()
    } else {
        format!("one of: {}", named.join(", "))
    };
    match (help.is_empty(), choices.is_empty()) {
        (true, true) => None,
        (false, true) => Some(help),
        (true, false) => Some(choices),
        (false, false) => Some(format!("{help} ({choices})")),
    }
}

/// A command's or argument's help text, line breaks as it was written.
fn text(styled: Option<&StyledStr>) -> Option<String> {
    let written = styled.map(ToString::to_string).unwrap_or_default();
    let trimmed = written.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// `after_help` as the page shows it: a sentence, the commands as a block, and
/// whatever prose follows.
fn examples(written: &str) -> String {
    let mut lead = String::new();
    let mut block: Vec<&str> = Vec::new();
    let mut rest: Vec<&str> = Vec::new();
    let mut in_block = false;
    let mut after_block = false;
    for line in written.lines() {
        if line.trim().is_empty() {
            if in_block {
                after_block = true;
            }
            continue;
        }
        if line.starts_with("  ") && !after_block {
            in_block = true;
            block.push(line.trim_start());
        } else if !in_block {
            if !lead.is_empty() {
                lead.push(' ');
            }
            lead.push_str(line.trim());
        } else {
            after_block = true;
            rest.push(line.trim());
        }
    }
    let mut page = String::new();
    if !lead.is_empty() {
        let _ = writeln!(page, "{lead}\n");
    }
    if !block.is_empty() {
        let _ = writeln!(page, "```sh fragment\n{}\n```\n", block.join("\n"));
    }
    if !rest.is_empty() {
        let _ = writeln!(page, "{}", rest.join(" "));
    }
    page.trim_end().to_owned()
}

//! Parsing the GNU command lines into a [`FileAction`].
//!
//! Drop-in means the line a script or a model already writes keeps working:
//! the same short flags, clustered (`rm -rf`) and attached (`rm -f`), the same
//! long flags that matter, `--` ending the flags, and `-` alone as a name.
//! A flag this runtime does not implement is refused by name rather than
//! ignored, because ignoring `-i` (ask before overwriting) or `-a`
//! (preserve attributes) would silently do something other than what was
//! asked.
//!
//! `append` and `touch` extend the same reading to querying and creating
//! text: `append FILE TEXT…` is `>>`, and `touch FILE` creates without
//! truncating, the two things a script or a model does to a text file that
//! `truncate` and `cp` cannot say.

use std::path::PathBuf;

use super::{
    AppendFileOp, CopyOp, FileAction, LinkOp, MakeDirOp, MoveOp, RemoveDirOp, RemoveOp, TruncateOp,
};
/// Which command a line was written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Cp,
    Mv,
    Rm,
    Mkdir,
    Rmdir,
    Ln,
    Truncate,
    Touch,
    Append,
}

impl Tool {
    /// The tool `name` spells, if this runtime answers for it.
    #[must_use]
    pub fn of(name: &str) -> Option<Self> {
        match name {
            "cp" => Some(Self::Cp),
            "mv" => Some(Self::Mv),
            "rm" => Some(Self::Rm),
            "mkdir" => Some(Self::Mkdir),
            "rmdir" => Some(Self::Rmdir),
            "ln" => Some(Self::Ln),
            "truncate" => Some(Self::Truncate),
            "touch" => Some(Self::Touch),
            "append" => Some(Self::Append),
            _ => None,
        }
    }

    /// The name it answers to.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cp => "cp",
            Self::Mv => "mv",
            Self::Rm => "rm",
            Self::Mkdir => "mkdir",
            Self::Rmdir => "rmdir",
            Self::Ln => "ln",
            Self::Truncate => "truncate",
            Self::Touch => "touch",
            Self::Append => "append",
        }
    }

    /// Whether this tool only records what it did, writing nothing: `touch`
    /// on a file that already has contents keeps them.
    #[must_use]
    pub const fn records_only(self) -> bool {
        matches!(self, Self::Touch)
    }
}

/// What a command line could not be read as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// A flag this runtime does not implement, by its own spelling.
    Unsupported { tool: &'static str, flag: String },
    /// The operands are missing or of the wrong number.
    Operands {
        tool: &'static str,
        usage: &'static str,
    },
    /// `truncate`'s size was not a number.
    Size(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported { tool, flag } => {
                write!(f, "{tool}: `{flag}` is not supported here")
            }
            Self::Operands { tool, usage } => {
                write!(f, "{tool}: {usage}")
            }
            Self::Size(size) => write!(f, "truncate: invalid size `{size}`"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Read `args` (the operands and flags after the tool's name) as `tool`.
///
/// # Errors
///
/// An unimplemented flag, the wrong number of operands, or a size that is not
/// a number.
pub fn parse(tool: Tool, args: &[String]) -> Result<FileAction, ParseError> {
    let parsed = Flags::read(tool, args);
    if let Some(flag) = parsed.unsupported.first() {
        return Err(ParseError::Unsupported {
            tool: tool.name(),
            flag: flag.clone(),
        });
    }
    let operands = parsed.operands;
    let has = |flag: &str| parsed.seen.iter().any(|s| s == flag);
    match tool {
        Tool::Cp => {
            if operands.len() < 2 {
                return Err(ParseError::Operands {
                    tool: "cp",
                    usage: "missing destination file operand after the source",
                });
            }
            let (destination, sources) = split_destination(operands);
            Ok(FileAction::Copy(CopyOp {
                sources,
                destination,
                recursive: has("-r") || has("-R") || has("-a"),
                update: has("-u"),
                no_clobber: has("-n"),
            }))
        }
        Tool::Mv => {
            if operands.len() < 2 {
                return Err(ParseError::Operands {
                    tool: "mv",
                    usage: "missing destination file operand after the source",
                });
            }
            let (destination, sources) = split_destination(operands);
            Ok(FileAction::Move(MoveOp {
                sources,
                destination,
                no_clobber: has("-n"),
            }))
        }
        Tool::Rm => {
            if operands.is_empty() && !has("-f") {
                return Err(ParseError::Operands {
                    tool: "rm",
                    usage: "missing operand",
                });
            }
            Ok(FileAction::Remove(RemoveOp {
                paths: operands,
                recursive: has("-r") || has("-R"),
                force: has("-f"),
            }))
        }
        Tool::Mkdir => {
            if operands.is_empty() {
                return Err(ParseError::Operands {
                    tool: "mkdir",
                    usage: "missing operand",
                });
            }
            Ok(FileAction::MakeDir(MakeDirOp {
                dirs: operands,
                parents: has("-p"),
            }))
        }
        Tool::Rmdir => {
            if operands.is_empty() {
                return Err(ParseError::Operands {
                    tool: "rmdir",
                    usage: "missing operand",
                });
            }
            Ok(FileAction::RemoveDir(RemoveDirOp {
                dirs: operands,
                parents: has("-p"),
            }))
        }
        Tool::Ln => parse_ln(&operands, &parsed.seen),
        Tool::Touch => parse_touch(operands),
        Tool::Append => parse_append(operands),
        // Answered above, before the operands were matched over: `truncate`
        // is the one tool whose `-s` is a size, not a link.
        Tool::Truncate => parse_truncate(operands, &parsed.values, &has),
    }
}

fn parse_touch(operands: Vec<PathBuf>) -> Result<FileAction, ParseError> {
    if operands.is_empty() {
        return Err(ParseError::Operands {
            tool: "touch",
            usage: "missing operand",
        });
    }
    Ok(FileAction::Touch(AppendFileOp {
        files: operands,
        text: String::new(),
    }))
}

/// `append TEXT FILE...`: the text comes first, then every file it goes into.
fn parse_append(mut operands: Vec<PathBuf>) -> Result<FileAction, ParseError> {
    if operands.len() < 2 {
        return Err(ParseError::Operands {
            tool: "append",
            usage: "missing file operand after the text",
        });
    }
    let text = operands.remove(0).to_string_lossy().into_owned();
    Ok(FileAction::Append(AppendFileOp {
        files: operands,
        text,
    }))
}

fn parse_truncate(
    operands: Vec<PathBuf>,
    values: &[(&'static str, String)],
    has: &dyn Fn(&str) -> bool,
) -> Result<FileAction, ParseError> {
    let size = values
        .iter()
        .find_map(|(flag, value)| (*flag == "-s").then(|| value.clone()))
        .ok_or(ParseError::Operands {
            tool: "truncate",
            usage: "missing `-s SIZE`",
        })?;
    if operands.is_empty() {
        return Err(ParseError::Operands {
            tool: "truncate",
            usage: "missing operand",
        });
    }
    Ok(FileAction::Truncate(TruncateOp {
        files: operands,
        size: size
            .parse::<u64>()
            .map_err(|_| ParseError::Size(size.clone()))?,
        create: !has("-c"),
    }))
}

fn parse_ln(operands: &[PathBuf], seen: &[String]) -> Result<FileAction, ParseError> {
    let has = |flag: &str| seen.iter().any(|s| s == flag);
    let (target, path, into_directory) = match operands {
        // `ln -s TARGET` links into the current directory, under the
        // target's own name, as GNU does.
        [target] => {
            let name = target
                .file_name()
                .map(PathBuf::from)
                .ok_or(ParseError::Operands {
                    tool: "ln",
                    usage: "missing target",
                })?;
            (target.clone(), name, true)
        }
        [target, path] => (target.clone(), path.clone(), path.is_dir()),
        _ => {
            return Err(ParseError::Operands {
                tool: "ln",
                usage: "missing destination file operand after the target",
            });
        }
    };
    Ok(FileAction::Link(LinkOp {
        target,
        path,
        symbolic: has("-s"),
        force: has("-f"),
        into_directory,
    }))
}

/// The operands GNU takes as `SOURCES… DESTINATION`.
fn split_destination(mut operands: Vec<PathBuf>) -> (PathBuf, Vec<PathBuf>) {
    let destination = operands.pop().unwrap_or_default();
    (destination, operands)
}

#[derive(Debug, Default)]
struct Flags {
    operands: Vec<PathBuf>,
    /// Every short flag and long flag seen, as it was written: `-r`, `-f`.
    seen: Vec<String>,
    /// Flags that take a value (`-s 512`), with it.
    values: Vec<(&'static str, String)>,
    /// Flags that mean something this runtime does not do, refused by name.
    unsupported: Vec<String>,
}

impl Flags {
    /// Read `args` as flags and operands. `-s` takes a value only where it is
    /// `--size`; for every other tool it is `--symbolic`.
    fn read(tool: Tool, args: &[String]) -> Self {
        let size_takes_a_value = tool == Tool::Truncate;
        let mut out = Self::default();
        let mut rest = args.iter();
        let mut after_double_dash = false;
        while let Some(arg) = rest.next() {
            if after_double_dash || arg == "-" || !arg.starts_with('-') {
                out.operands.push(PathBuf::from(arg));
                continue;
            }
            if arg == "--" {
                after_double_dash = true;
                continue;
            }
            if let Some(long) = arg.strip_prefix("--") {
                let (name, value) = match long.split_once('=') {
                    Some((name, value)) => (name, Some(value.to_string())),
                    None => (long, None),
                };
                match long_flag(name).filter(|short| *short != "-s" || size_takes_a_value) {
                    Some("-s") if size_takes_a_value => {
                        match value.or_else(|| rest.next().cloned()) {
                            Some(value) => out.values.push(("-s", value)),
                            None => out.unsupported.push(arg.clone()),
                        }
                    }
                    Some(short) if value.is_none() => out.seen.push(short.to_string()),
                    // A flag that does not take a value was given one, or a
                    // flag this runtime does not do: either way, by name.
                    _ => out.unsupported.push(arg.clone()),
                }
                continue;
            }
            for (index, short) in arg.chars().skip(1).enumerate() {
                let flag = format!("-{short}");
                if short == 's' && size_takes_a_value {
                    // `-s 512` and `-s512` are both the flag's value; in the
                    // latter, what follows `s` in the same word is it.
                    let attached: String = arg.chars().skip(index.saturating_add(2)).collect();
                    let value = if attached.is_empty() {
                        rest.next().cloned()
                    } else {
                        Some(attached)
                    };
                    match value {
                        Some(value) => out.values.push(("-s", value)),
                        None => out.unsupported.push(flag),
                    }
                    break;
                }
                if supported(short) {
                    out.seen.push(flag);
                } else {
                    out.unsupported.push(flag);
                }
            }
        }
        out
    }
}

/// The short flags the file actions implement, and the long spellings of
/// them. Anything else — `-i`, `-a`, `-L`, `-Z`, `--preserve`, … — is
/// refused by name rather than quietly ignored.
fn supported(short: char) -> bool {
    matches!(
        short,
        'f' | 'r' | 'R' | 'n' | 'u' | 'p' | 's' | 'c' | 'v' | 'a'
    )
}

/// The short flag a long one stands for, if this runtime implements it.
fn long_flag(name: &str) -> Option<&'static str> {
    Some(match name {
        "recursive" => "-r",
        "force" => "-f",
        "parents" => "-p",
        "symbolic" | "size" => "-s",
        "no-clobber" => "-n",
        "update" => "-u",
        "verbose" => "-v",
        "no-create" => "-c",
        "append" => "-a",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn clustered_and_attached_short_flags_read_as_gnu_reads_them() {
        let FileAction::Remove(op) = parse(Tool::Rm, &args("-rf build tmp")).unwrap() else {
            panic!("rm reads as a removal");
        };
        assert!(op.recursive && op.force);
        assert_eq!(op.paths, vec![PathBuf::from("build"), PathBuf::from("tmp")]);
    }

    #[test]
    fn a_double_dash_ends_the_flags() {
        let FileAction::Remove(op) = parse(Tool::Rm, &args("-f -- -weird")).unwrap() else {
            panic!("rm reads as a removal");
        };
        assert_eq!(
            op.paths,
            vec![PathBuf::from("-weird")],
            "a name may start with a dash"
        );
    }

    #[test]
    fn a_flag_this_runtime_does_not_do_is_refused_by_name() {
        let error = parse(Tool::Cp, &args("-i a b")).unwrap_err();
        assert_eq!(
            error,
            ParseError::Unsupported {
                tool: "cp",
                flag: "-i".into()
            }
        );
        assert!(
            error.to_string().contains("`-i`"),
            "the person is told which flag"
        );
    }

    #[test]
    fn the_last_operand_is_the_destination() {
        let FileAction::Copy(op) = parse(Tool::Cp, &args("-r a b c")).unwrap() else {
            panic!("cp reads as a copy");
        };
        assert_eq!(op.destination, PathBuf::from("c"));
        assert_eq!(op.sources, vec![PathBuf::from("a"), PathBuf::from("b")]);
    }

    #[test]
    fn truncate_takes_its_size_attached_or_apart() {
        let size = |line: &str| match parse(Tool::Truncate, &args(line)).unwrap() {
            FileAction::Truncate(op) => op.size,
            other => panic!("truncate reads as a truncation: {other:?}"),
        };
        assert_eq!(size("-s 512 log"), 512);
        assert_eq!(size("-s512 log"), 512);
    }

    #[test]
    fn touch_names_files_and_carries_no_text() {
        let FileAction::Touch(op) = parse(Tool::Touch, &args("notes/today.md draft.md")).unwrap()
        else {
            panic!("touch reads as a touch");
        };
        assert_eq!(
            op.files,
            vec![PathBuf::from("notes/today.md"), PathBuf::from("draft.md")]
        );
        assert!(op.text.is_empty());
    }

    #[test]
    fn append_takes_its_text_before_the_files_it_goes_into() {
        let FileAction::Append(op) =
            parse(Tool::Append, &args("bought-milk notes/today.md")).unwrap()
        else {
            panic!("append reads as an append");
        };
        assert_eq!(op.text, "bought-milk");
        assert_eq!(op.files, vec![PathBuf::from("notes/today.md")]);
    }

    #[test]
    fn append_with_no_file_is_refused_with_what_it_takes() {
        let error = parse(Tool::Append, &args("only-the-text")).unwrap_err();
        assert_eq!(
            error.to_string(),
            "append: missing file operand after the text"
        );
    }

    #[test]
    fn a_sole_target_links_into_the_current_directory() {
        let FileAction::Link(op) = parse(Tool::Ln, &args("-s ../shared/config")).unwrap() else {
            panic!("ln reads as a link");
        };
        assert!(op.symbolic && op.into_directory);
        assert_eq!(op.path, PathBuf::from("config"));
    }
}

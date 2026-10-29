//! The options each tier-1 command and shell built-in takes, as Tab
//! completion offers them.
//!
//! Each list is read off the command's own parser: a flag the parser
//! refuses ("is not supported") is not here, and neither is one it accepts
//! and ignores (`cat -u`, `sort -S`), as offering it would promise
//! something it does not do. A long option that takes a value ends in `=`
//! (`--max-count=`); a short one that takes a value is written bare (`-m`),
//! the value going in the next word.

/// One option a command takes, as Tab completion offers it.
pub struct Opt {
    pub spelling: &'static str,
    pub help: &'static str,
}

const fn o(spelling: &'static str, help: &'static str) -> Opt {
    Opt { spelling, help }
}

/// The options the tier-1 command or built-in `name` accepts, in the order a
/// person reads them; empty for a name with none or one it does not know.
pub fn of(name: &str) -> &'static [Opt] {
    match name {
        // Files and paths.
        "basename" => BASENAME,
        "dirname" => DIRNAME,
        "find" => FIND,
        "ls" => LS,
        "readlink" => READLINK,
        "realpath" => REALPATH,
        "stat" => STAT,
        "tree" => TREE,
        // Text.
        "cat" => CAT,
        "cut" => CUT,
        "diff" => DIFF,
        "env" => ENV,
        "grep" => GREP,
        "head" | "tail" => HEAD_TAIL,
        "nl" => NL,
        "printenv" => PRINTENV,
        "seq" => SEQ,
        "sort" => SORT,
        "tee" => TEE,
        "tr" => TR,
        "uniq" => UNIQ,
        "wc" => WC,
        // Commands that run commands.
        "nice" => NICE,
        "timeout" => TIMEOUT,
        "xargs" => XARGS,
        // File actions.
        "cp" => CP,
        "mv" => MV,
        "rm" => RM,
        "mkdir" | "rmdir" => MKDIR,
        "ln" => LN,
        "truncate" => TRUNCATE,
        // Shell built-ins.
        "alias" => ALIAS,
        "cd" | "pwd" => CD,
        "command" => COMMAND,
        "declare" | "typeset" => DECLARE,
        "echo" => ECHO,
        "export" => EXPORT,
        "local" => LOCAL,
        "mapfile" | "readarray" => MAPFILE,
        "printf" => PRINTF,
        "read" => READ,
        "readonly" => READONLY,
        "set" => SET,
        "shopt" => SHOPT,
        "test" | "[" => TEST,
        "trap" => TRAP,
        "type" => TYPE,
        "unalias" => UNALIAS,
        "unset" => UNSET,
        _ => &[],
    }
}

const BASENAME: &[Opt] = &[
    o("-a", "every operand a name"),
    o("--multiple", "every operand a name"),
    o("-s", "remove a suffix"),
    o("--suffix=", "remove a suffix"),
    o("-z", "end with NUL"),
    o("--zero", "end with NUL"),
];

const DIRNAME: &[Opt] = &[o("-z", "end with NUL"), o("--zero", "end with NUL")];

const FIND: &[Opt] = &[
    o("-name", "name matches a pattern"),
    o("-iname", "name matches, any case"),
    o("-path", "path matches a pattern"),
    o("-ipath", "path matches, any case"),
    o("-wholename", "path matches a pattern"),
    o("-iwholename", "path matches, any case"),
    o("-type", "kind of file"),
    o("-empty", "empty file or directory"),
    o("-size", "size in units"),
    o("-mtime", "modified days ago"),
    o("-mmin", "modified minutes ago"),
    o("-newer", "modified after a file"),
    o("-maxdepth", "descend at most levels"),
    o("-mindepth", "skip shallower levels"),
    o("-depth", "contents before directory"),
    o("-true", "always true"),
    o("-false", "always false"),
    o("-not", "negate"),
    o("-and", "both"),
    o("-or", "either"),
    o("-print", "print the path"),
    o("-print0", "print the path, NUL ended"),
    o("-prune", "do not descend"),
    o("-quit", "stop at once"),
    o("-delete", "delete the file"),
    o("-exec", "run a command"),
    o("-execdir", "run a command in its directory"),
    o("-ok", "run a command after asking"),
    o("-okdir", "run in its directory after asking"),
    o("-P", "never follow symlinks"),
];

const LS: &[Opt] = &[
    o("-a", "show hidden entries"),
    o("--all", "show hidden entries"),
    o("-A", "hidden, without the dot entries"),
    o("--almost-all", "hidden, without the dot entries"),
    o("-l", "long listing"),
    o("-1", "one name per line"),
    o("-C", "columns"),
    o("-h", "human-readable sizes"),
    o("--human-readable", "human-readable sizes"),
    o("-n", "numeric user and group"),
    o("--numeric-uid-gid", "numeric user and group"),
    o("-R", "list subdirectories"),
    o("--recursive", "list subdirectories"),
    o("-d", "directories themselves"),
    o("--directory", "directories themselves"),
    o("-t", "newest first"),
    o("-S", "largest first"),
    o("-U", "directory order"),
    o("-r", "reverse order"),
    o("--reverse", "reverse order"),
    o("-F", "mark kinds"),
    o("--classify", "mark kinds"),
    o("-p", "mark directories"),
    o("--color=", "never or auto"),
];

const READLINK: &[Opt] = &[
    o("-f", "canonicalize"),
    o("--canonicalize", "canonicalize"),
    o("-e", "every component must exist"),
    o("--canonicalize-existing", "every component must exist"),
    o("-m", "no component need exist"),
    o("--canonicalize-missing", "no component need exist"),
    o("-n", "no trailing newline"),
    o("--no-newline", "no trailing newline"),
    o("-q", "quiet"),
    o("--quiet", "quiet"),
    o("--silent", "quiet"),
    o("-v", "report errors"),
    o("--verbose", "report errors"),
    o("-z", "end with NUL"),
    o("--zero", "end with NUL"),
];

const REALPATH: &[Opt] = &[
    o("-e", "every component must exist"),
    o("--canonicalize-existing", "every component must exist"),
    o("-m", "no component need exist"),
    o("--canonicalize-missing", "no component need exist"),
    o("-s", "do not follow symlinks"),
    o("--strip", "do not follow symlinks"),
    o("--no-symlinks", "do not follow symlinks"),
    o("-P", "follow symlinks"),
    o("--physical", "follow symlinks"),
    o("-q", "quiet"),
    o("--quiet", "quiet"),
    o("-z", "end with NUL"),
    o("--zero", "end with NUL"),
];

const STAT: &[Opt] = &[
    o("-c", "format"),
    o("--format=", "format"),
    o("--printf=", "format with escapes"),
    o("-L", "follow symlinks"),
    o("--dereference", "follow symlinks"),
];

const TREE: &[Opt] = &[
    o("-a", "show hidden entries"),
    o("-d", "directories only"),
    o("-L", "levels deep"),
    o("--noreport", "no count at the end"),
];

const CAT: &[Opt] = &[
    o("-n", "number lines"),
    o("--number", "number lines"),
    o("-b", "number nonblank lines"),
    o("--number-nonblank", "number nonblank lines"),
    o("-s", "squeeze blank lines"),
    o("--squeeze-blank", "squeeze blank lines"),
    o("-E", "mark line ends"),
    o("--show-ends", "mark line ends"),
    o("-T", "show tabs"),
    o("--show-tabs", "show tabs"),
    o("-v", "show nonprinting"),
    o("--show-nonprinting", "show nonprinting"),
    o("-A", "show everything"),
    o("--show-all", "show everything"),
    o("-e", "nonprinting and ends"),
    o("-t", "nonprinting and tabs"),
];

const CUT: &[Opt] = &[
    o("-f", "fields"),
    o("--fields=", "fields"),
    o("-d", "field delimiter"),
    o("--delimiter=", "field delimiter"),
    o("-b", "bytes"),
    o("--bytes=", "bytes"),
    o("-c", "characters"),
    o("--characters=", "characters"),
    o("-s", "only delimited lines"),
    o("--only-delimited", "only delimited lines"),
    o("--complement", "everything else"),
    o("--output-delimiter=", "delimiter in output"),
    o("-z", "lines end with NUL"),
    o("--zero-terminated", "lines end with NUL"),
];

const DIFF: &[Opt] = &[
    o("-u", "unified format"),
    o("--unified", "unified format"),
    o("-U", "unified, lines of context"),
    o("-q", "only whether they differ"),
    o("--brief", "only whether they differ"),
    o("--normal", "normal format"),
    o("-i", "ignore case"),
    o("--ignore-case", "ignore case"),
    o("-b", "ignore space changes"),
    o("--ignore-space-change", "ignore space changes"),
    o("-w", "ignore all space"),
    o("--ignore-all-space", "ignore all space"),
    o("-a", "treat as text"),
    o("--text", "treat as text"),
    o("-s", "report identical files"),
    o("--report-identical-files", "report identical files"),
    o("--label=", "name in the header"),
    o("--color=", "never or auto"),
];

const ENV: &[Opt] = &[
    o("-i", "start empty"),
    o("--ignore-environment", "start empty"),
    o("-u", "remove a variable"),
    o("--unset=", "remove a variable"),
    o("-C", "change directory"),
    o("--chdir=", "change directory"),
    o("-0", "end with NUL"),
    o("--null", "end with NUL"),
];

const GREP: &[Opt] = &[
    o("-i", "ignore case"),
    o("-y", "ignore case"),
    o("--ignore-case", "ignore case"),
    o("--no-ignore-case", "match case"),
    o("-v", "non-matching lines"),
    o("--invert-match", "non-matching lines"),
    o("-w", "whole words"),
    o("--word-regexp", "whole words"),
    o("-x", "whole lines"),
    o("--line-regexp", "whole lines"),
    o("-n", "line numbers"),
    o("--line-number", "line numbers"),
    o("-c", "count matching lines"),
    o("--count", "count matching lines"),
    o("-l", "files with matches"),
    o("--files-with-matches", "files with matches"),
    o("-L", "files without matches"),
    o("--files-without-match", "files without matches"),
    o("-o", "only the matched parts"),
    o("--only-matching", "only the matched parts"),
    o("-q", "quiet"),
    o("--quiet", "quiet"),
    o("--silent", "quiet"),
    o("-s", "no file errors"),
    o("--no-messages", "no file errors"),
    o("-m", "stop after matches"),
    o("--max-count=", "stop after matches"),
    o("-H", "print file names"),
    o("--with-filename", "print file names"),
    o("-h", "no file names"),
    o("--no-filename", "no file names"),
    o("--label=", "name for standard input"),
    o("-A", "lines after"),
    o("--after-context=", "lines after"),
    o("-B", "lines before"),
    o("--before-context=", "lines before"),
    o("-C", "lines around"),
    o("--context=", "lines around"),
    o("-r", "search directories"),
    o("--recursive", "search directories"),
    o("-R", "search, following symlinks"),
    o("--dereference-recursive", "search, following symlinks"),
    o("--include=", "only matching files"),
    o("--exclude=", "skip matching files"),
    o("--exclude-dir=", "skip matching directories"),
    o("-E", "extended regex"),
    o("--extended-regexp", "extended regex"),
    o("-F", "fixed strings"),
    o("--fixed-strings", "fixed strings"),
    o("-G", "basic regex"),
    o("--basic-regexp", "basic regex"),
    o("-e", "pattern"),
    o("--regexp=", "pattern"),
    o("-f", "patterns from a file"),
    o("--file=", "patterns from a file"),
    o("-a", "treat binary as text"),
    o("--text", "treat binary as text"),
    o("-I", "skip binary files"),
    o("--binary-files=", "binary, text or without-match"),
    o("--color=", "never or auto"),
];

const HEAD_TAIL: &[Opt] = &[
    o("-n", "number of lines"),
    o("--lines=", "number of lines"),
    o("-c", "number of bytes"),
    o("--bytes=", "number of bytes"),
    o("-q", "no file headers"),
    o("--quiet", "no file headers"),
    o("--silent", "no file headers"),
    o("-v", "always file headers"),
    o("--verbose", "always file headers"),
    o("-z", "lines end with NUL"),
    o("--zero-terminated", "lines end with NUL"),
];

const NL: &[Opt] = &[
    o("-b", "which lines: a, t or n"),
    o("--body-numbering=", "which lines: a, t or n"),
    o("-n", "format: ln, rn or rz"),
    o("--number-format=", "format: ln, rn or rz"),
    o("-w", "number width"),
    o("--number-width=", "number width"),
    o("-s", "after the number"),
    o("--number-separator=", "after the number"),
    o("-v", "first number"),
    o("--starting-line-number=", "first number"),
    o("-i", "increment"),
    o("--line-increment=", "increment"),
];

const PRINTENV: &[Opt] = &[o("-0", "end with NUL"), o("--null", "end with NUL")];

const SEQ: &[Opt] = &[
    o("-s", "separator"),
    o("--separator=", "separator"),
    o("-w", "equal width"),
    o("--equal-width", "equal width"),
];

const SORT: &[Opt] = &[
    o("-n", "numeric"),
    o("--numeric-sort", "numeric"),
    o("-g", "general numeric"),
    o("--general-numeric-sort", "general numeric"),
    o("-h", "human-readable sizes"),
    o("--human-numeric-sort", "human-readable sizes"),
    o("-r", "reverse"),
    o("--reverse", "reverse"),
    o("-u", "unique lines"),
    o("--unique", "unique lines"),
    o("-k", "sort key"),
    o("--key=", "sort key"),
    o("-t", "field separator"),
    o("--field-separator=", "field separator"),
    o("-f", "ignore case"),
    o("--ignore-case", "ignore case"),
    o("-b", "ignore leading blanks"),
    o("--ignore-leading-blanks", "ignore leading blanks"),
    o("-d", "dictionary order"),
    o("--dictionary-order", "dictionary order"),
    o("-i", "ignore nonprinting"),
    o("--ignore-nonprinting", "ignore nonprinting"),
    o("-s", "stable"),
    o("--stable", "stable"),
    o("-c", "check sorted"),
    o("--check", "check sorted"),
    o("-C", "check sorted, quietly"),
    o("-o", "output file"),
    o("--output=", "output file"),
    o("-z", "lines end with NUL"),
    o("--zero-terminated", "lines end with NUL"),
];

const TEE: &[Opt] = &[o("-a", "append"), o("--append", "append")];

const TR: &[Opt] = &[
    o("-d", "delete"),
    o("--delete", "delete"),
    o("-s", "squeeze repeats"),
    o("--squeeze-repeats", "squeeze repeats"),
    o("-c", "complement"),
    o("-C", "complement"),
    o("--complement", "complement"),
    o("-t", "truncate the first set"),
    o("--truncate-set1", "truncate the first set"),
];

const UNIQ: &[Opt] = &[
    o("-c", "count"),
    o("--count", "count"),
    o("-d", "repeated lines once"),
    o("--repeated", "repeated lines once"),
    o("-D", "every repeated line"),
    o("--all-repeated", "every repeated line"),
    o("-u", "unrepeated lines"),
    o("--unique", "unrepeated lines"),
    o("-i", "ignore case"),
    o("--ignore-case", "ignore case"),
    o("-f", "skip fields"),
    o("--skip-fields=", "skip fields"),
    o("-s", "skip characters"),
    o("--skip-chars=", "skip characters"),
    o("-w", "compare characters"),
    o("--check-chars=", "compare characters"),
    o("-z", "lines end with NUL"),
    o("--zero-terminated", "lines end with NUL"),
];

const WC: &[Opt] = &[
    o("-l", "lines"),
    o("--lines", "lines"),
    o("-w", "words"),
    o("--words", "words"),
    o("-c", "bytes"),
    o("--bytes", "bytes"),
    o("-m", "characters"),
    o("--chars", "characters"),
    o("-L", "longest line"),
    o("--max-line-length", "longest line"),
    o("--total=", "auto, always, only or never"),
];

const NICE: &[Opt] = &[o("-n", "adjustment"), o("--adjustment=", "adjustment")];

const TIMEOUT: &[Opt] = &[
    o("-s", "signal"),
    o("--signal=", "signal"),
    o("-k", "kill after"),
    o("--kill-after=", "kill after"),
    o("--preserve-status", "the command's own status"),
    o("-v", "say when it times out"),
    o("--verbose", "say when it times out"),
];

const XARGS: &[Opt] = &[
    o("-0", "items end with NUL"),
    o("--null", "items end with NUL"),
    o("-d", "item delimiter"),
    o("--delimiter=", "item delimiter"),
    o("-n", "arguments per command"),
    o("--max-args=", "arguments per command"),
    o("-L", "lines per command"),
    o("--max-lines=", "lines per command"),
    o("-I", "replace a string"),
    o("--replace", "replace {}"),
    o("-P", "commands at once"),
    o("--max-procs=", "commands at once"),
    o("-s", "longest command line"),
    o("--max-chars=", "longest command line"),
    o("-r", "nothing if no input"),
    o("--no-run-if-empty", "nothing if no input"),
    o("-t", "print each command"),
    o("--verbose", "print each command"),
    o("-p", "ask before each"),
    o("--interactive", "ask before each"),
    o("-x", "stop if too long"),
    o("--exit", "stop if too long"),
    o("-E", "end-of-input string"),
    o("--eof=", "end-of-input string"),
    o("-a", "read items from a file"),
    o("--arg-file=", "read items from a file"),
];

const CP: &[Opt] = &[
    o("-r", "copy directories"),
    o("-R", "copy directories"),
    o("--recursive", "copy directories"),
    o("-a", "copy directories"),
    o("-n", "never overwrite"),
    o("--no-clobber", "never overwrite"),
    o("-u", "only newer"),
    o("--update", "only newer"),
];

const MV: &[Opt] = &[
    o("-n", "never overwrite"),
    o("--no-clobber", "never overwrite"),
    o("-f", "overwrite"),
    o("--force", "overwrite"),
];

const RM: &[Opt] = &[
    o("-r", "remove directories"),
    o("-R", "remove directories"),
    o("--recursive", "remove directories"),
    o("-f", "ignore missing files"),
    o("--force", "ignore missing files"),
];

const MKDIR: &[Opt] = &[o("-p", "parents too"), o("--parents", "parents too")];

const LN: &[Opt] = &[
    o("-s", "symbolic link"),
    o("--symbolic", "symbolic link"),
    o("-f", "replace an existing file"),
    o("--force", "replace an existing file"),
];

const TRUNCATE: &[Opt] = &[
    o("-s", "size in bytes"),
    o("--size=", "size in bytes"),
    o("-c", "do not create files"),
    o("--no-create", "do not create files"),
];

const ALIAS: &[Opt] = &[o("-p", "print every alias")];

const CD: &[Opt] = &[o("-L", "logical path"), o("-P", "physical path")];

const COMMAND: &[Opt] = &[
    o("-v", "how a name resolves"),
    o("-V", "how a name resolves, in words"),
    o("-p", "default search path"),
];

const DECLARE: &[Opt] = &[
    o("-a", "indexed array"),
    o("-A", "associative array"),
    o("-i", "integer"),
    o("-r", "read-only"),
    o("-x", "export"),
    o("-g", "global"),
    o("-p", "print declarations"),
    o("-f", "functions"),
    o("-F", "function names"),
];

const ECHO: &[Opt] = &[
    o("-n", "no trailing newline"),
    o("-e", "expand escapes"),
    o("-E", "no escapes"),
];

const EXPORT: &[Opt] = &[o("-n", "stop exporting"), o("-p", "print exports")];

const LOCAL: &[Opt] = &[
    o("-a", "indexed array"),
    o("-A", "associative array"),
    o("-i", "integer"),
    o("-r", "read-only"),
    o("-x", "export"),
];

const MAPFILE: &[Opt] = &[
    o("-t", "drop the delimiter"),
    o("-d", "line delimiter"),
    o("-n", "at most lines"),
    o("-s", "skip lines"),
];

const PRINTF: &[Opt] = &[o("-v", "assign to a variable")];

const READ: &[Opt] = &[
    o("-r", "keep backslashes"),
    o("-p", "prompt"),
    o("-a", "into an array"),
    o("-d", "delimiter"),
    o("-n", "at most characters"),
    o("-N", "exactly characters"),
    o("-u", "file descriptor"),
];

const READONLY: &[Opt] = &[
    o("-a", "indexed array"),
    o("-A", "associative array"),
    o("-p", "print read-only names"),
];

const SET: &[Opt] = &[
    o("-e", "exit on error"),
    o("-u", "unset is an error"),
    o("-x", "trace commands"),
    o("-f", "no globbing"),
    o("-C", "no clobbering"),
    o("-a", "export all"),
    o("-o", "option by name"),
];

const SHOPT: &[Opt] = &[o("-s", "set"), o("-u", "unset"), o("-q", "quiet")];

const TEST: &[Opt] = &[
    o("-e", "exists"),
    o("-f", "regular file"),
    o("-d", "directory"),
    o("-L", "symbolic link"),
    o("-h", "symbolic link"),
    o("-s", "not empty"),
    o("-r", "readable"),
    o("-w", "writable"),
    o("-x", "executable"),
    o("-z", "empty string"),
    o("-n", "nonempty string"),
    o("-v", "variable is set"),
    o("-t", "descriptor is a terminal"),
    o("-p", "named pipe"),
    o("-S", "socket"),
    o("-b", "block device"),
    o("-c", "character device"),
    o("-u", "setuid"),
    o("-g", "setgid"),
    o("-k", "sticky"),
    o("-eq", "numbers equal"),
    o("-ne", "numbers differ"),
    o("-lt", "less than"),
    o("-le", "at most"),
    o("-gt", "greater than"),
    o("-ge", "at least"),
    o("-nt", "newer than"),
    o("-ot", "older than"),
    o("-ef", "same file"),
];

const TRAP: &[Opt] = &[o("-p", "print traps"), o("-l", "list signals")];

const TYPE: &[Opt] = &[o("-t", "one word")];

const UNALIAS: &[Opt] = &[o("-a", "every alias")];

const UNSET: &[Opt] = &[o("-f", "functions"), o("-v", "variables")];

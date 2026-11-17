//! A working copy and a branch of its own for each coding agent.
//!
//! Several coding agents may work on one repository at once: a lead and its
//! team on one feature. Each gets a working copy of its own on a branch of
//! its own (a git worktree of the repository), so two never write one tree,
//! and what each made comes together through git. Git itself keeps the rule:
//! a branch is checked out in one working copy at a time, and a second agent
//! asked onto it is refused in git's words.
//!
//! The copy is made in the machine, by the run itself, before its harness
//! starts: the harness's program is wrapped in a small shell program that
//! makes the working copy when it is not there yet (a run that is taken up
//! again finds it) and then becomes the harness inside it.

/// Where one coding agent works.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkingCopy {
    /// The repository: a folder of the workspace's files, or a path of the
    /// machine's own.
    pub repository: String,
    /// The agent's own working copy.
    pub directory: String,
    /// The agent's own branch.
    pub branch: String,
    /// What a new branch starts from; the repository's current commit when
    /// none is named.
    pub base: Option<String>,
}

impl WorkingCopy {
    /// The working copy an agent `agent` gets in `repository` when nobody
    /// names another: a folder beside the repository, and a branch named for
    /// the agent.
    #[must_use]
    pub fn of(repository: &str, agent: &str) -> Self {
        let repository = repository.trim_end_matches('/');
        Self {
            repository: repository.to_owned(),
            directory: format!("{repository}.agents/{agent}"),
            branch: format!("agent/{agent}"),
            base: None,
        }
    }
}

/// Makes the working copy named by its first four arguments when it is not
/// there, then becomes the program the rest of them name, inside it. What
/// git prints goes to standard error: standard output is the program's.
const ENTERS: &str = r#"set -e
repository=$1 directory=$2 branch=$3 base=$4
shift 4
if [ ! -e "$directory/.git" ]; then
  if ! git -C "$repository" rev-parse --git-dir >/dev/null 2>&1; then
    echo "$repository is not a git repository, so no working copy of it can be made" >&2
    exit 66
  fi
  mkdir -p "$(dirname "$directory")"
  if git -C "$repository" show-ref --verify --quiet "refs/heads/$branch"; then
    git -C "$repository" worktree add "$directory" "$branch" >&2
  else
    git -C "$repository" worktree add -b "$branch" "$directory" ${base:+"$base"} >&2
  fi
fi
cd "$directory"
exec "$@""#;

/// `program`, started inside `copy`: the working copy is made first when it
/// is not there yet.
#[must_use]
pub fn within(copy: &WorkingCopy, program: &[String]) -> Vec<String> {
    let mut argv = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        ENTERS.to_owned(),
        "working-copy".to_owned(),
        copy.repository.clone(),
        copy.directory.clone(),
        copy.branch.clone(),
        copy.base.clone().unwrap_or_default(),
    ];
    argv.extend(program.iter().cloned());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_coding_agent_gets_a_working_copy_named_for_it;

        phase nobody_named_one_so_it_is_beside_the_repository_on_a_branch_of_its_own;
        {
            assert_eq!(
                WorkingCopy::of("code/app/", "usage-drop"),
                WorkingCopy {
                    repository: "code/app".into(),
                    directory: "code/app.agents/usage-drop".into(),
                    branch: "agent/usage-drop".into(),
                    base: None,
                }
            );
        }

        phase the_harness_is_started_by_what_makes_its_working_copy;
        {
            let copy = WorkingCopy {
                base: Some("main".into()),
                ..WorkingCopy::of("/home/tilo/app", "coder")
            };
            let program = vec!["claude".to_owned(), "--print".to_owned()];
            assert_eq!(
                within(&copy, &program),
                [
                    "sh", "-c", ENTERS, "working-copy", "/home/tilo/app",
                    "/home/tilo/app.agents/coder", "agent/coder", "main", "claude", "--print",
                ]
            );
            assert_eq!(
                within(&WorkingCopy::of("app", "coder"), &program).get(7).map(String::as_str),
                Some(""),
                "no base is an empty word, which the shell leaves out"
            );
        }
    }
}

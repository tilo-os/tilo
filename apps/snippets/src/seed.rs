//! The starter library a screenshot capture shows.

use crate::model::{Language, Snippet};

pub fn starter_library() -> Vec<Snippet> {
    let mut lib = ops_snippets();
    lib.extend(git_snippets());
    lib.extend(database_snippets());
    lib.extend(rust_snippets());
    lib.extend(container_snippets());
    lib.extend(writing_snippets());
    lib
}

/// One starter snippet, bundled into a struct rather than passed as
/// positional fields so `mk` doesn't collect an unreadable pile of
/// same-typed arguments.
struct SeedSnippet<'a> {
    title: &'a str,
    language: Language,
    folder: &'a str,
    tags: &'a [&'a str],
    description: &'a str,
    body: &'a str,
    favorite: bool,
    uses: i64,
}

fn mk(seed: &SeedSnippet<'_>) -> Snippet {
    let mut snippet = Snippet {
        language: seed.language,
        description: seed.description.to_string(),
        body: seed.body.to_string(),
        favorite: seed.favorite,
        tags: seed.tags.iter().map(|tag| (*tag).to_owned()).collect(),
        ..Snippet::titled(seed.title, seed.folder)
    };
    snippet.uses.add(seed.uses);
    snippet
}

fn ops_snippets() -> Vec<Snippet> {
    vec![
        mk(&SeedSnippet {
            title: "Restart deployment",
            language: Language::Shell,
            folder: "Ops",
            tags: &["k8s", "deploy"],
            description: "Roll a Kubernetes deployment in a namespace.",
            body: "kubectl -n ${namespace} rollout restart deployment/${deployment}\n\
             kubectl -n ${namespace} rollout status deployment/${deployment} --timeout=120s",
            favorite: true,
            uses: 12,
        }),
        mk(&SeedSnippet {
            title: "Tail container logs",
            language: Language::Shell,
            folder: "Ops",
            tags: &["k8s", "logs"],
            description: "Follow the last 200 log lines from a pod.",
            body: "kubectl -n ${namespace} logs -f --tail=200 ${pod}",
            favorite: false,
            uses: 5,
        }),
        mk(&SeedSnippet {
            title: "Bash strict mode",
            language: Language::Shell,
            folder: "Ops",
            tags: &["bash", "safety"],
            description: "The header every robust shell script should start with.",
            body: "#!/usr/bin/env bash\n\
             set -euo pipefail\n\
             IFS=$'\\n\\t'",
            favorite: true,
            uses: 14,
        }),
    ]
}

fn git_snippets() -> Vec<Snippet> {
    vec![
        mk(&SeedSnippet {
            title: "Amend last commit",
            language: Language::Git,
            folder: "Git",
            tags: &["commit", "fixup"],
            description: "Reword the last commit, keeping its changes.",
            body: "git commit --amend -m \"${message}\"",
            favorite: true,
            uses: 21,
        }),
        mk(&SeedSnippet {
            title: "Interactive rebase",
            language: Language::Git,
            folder: "Git",
            tags: &["rebase", "history"],
            description: "Squash and reorder the last N commits onto the base branch.",
            body: "git fetch origin\n\
             git rebase -i origin/${base_branch}",
            favorite: false,
            uses: 8,
        }),
        mk(&SeedSnippet {
            title: "Wipe a branch back to remote",
            language: Language::Git,
            folder: "Git",
            tags: &["reset", "danger"],
            description: "Drop local changes and match the upstream.",
            body: "git reset --hard origin/${branch}\n\
             git clean -fd",
            favorite: false,
            uses: 2,
        }),
        mk(&SeedSnippet {
            title: "Pretty git log",
            language: Language::Git,
            folder: "Git",
            tags: &["log", "alias"],
            description: "Compact one-line graph of recent history.",
            body: "git log --graph --oneline --decorate -n ${count}",
            favorite: false,
            uses: 11,
        }),
    ]
}

fn database_snippets() -> Vec<Snippet> {
    vec![
        mk(&SeedSnippet {
            title: "Find recent users",
            language: Language::Sql,
            folder: "Database",
            tags: &["postgres", "query"],
            description: "Most recently created accounts, newest first.",
            body: "SELECT id, email, created_at\n\
             FROM users\n\
             WHERE created_at > now() - interval '${days} days'\n\
             ORDER BY created_at DESC\n\
             LIMIT ${limit};",
            favorite: true,
            uses: 17,
        }),
        mk(&SeedSnippet {
            title: "Add a non-null column",
            language: Language::Sql,
            folder: "Database",
            tags: &["postgres", "migration"],
            description: "Backfill-safe column add with a default.",
            body: "ALTER TABLE ${table}\n\
             ADD COLUMN ${column} ${type} NOT NULL DEFAULT ${default};",
            favorite: false,
            uses: 3,
        }),
    ]
}

fn rust_snippets() -> Vec<Snippet> {
    vec![
        mk(&SeedSnippet {
            title: "Result-returning main",
            language: Language::Rust,
            folder: "Rust",
            tags: &["boilerplate"],
            description: "A main that bubbles errors up with the ? operator.",
            body: "fn main() -> Result<(), Box<dyn std::error::Error>> {\n\
             \x20   let ${var} = ${value};\n\
             \x20   println!(\"{}\", ${var});\n\
             \x20   Ok(())\n\
             }",
            favorite: true,
            uses: 9,
        }),
        mk(&SeedSnippet {
            title: "Derive the usual traits",
            language: Language::Rust,
            folder: "Rust",
            tags: &["derive"],
            description: "The everyday derive set for a plain data struct.",
            body: "#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]\n\
             pub struct ${Name} {\n\
             \x20   pub ${field}: ${Type},\n\
             }",
            favorite: false,
            uses: 6,
        }),
    ]
}

fn container_snippets() -> Vec<Snippet> {
    vec![
        mk(&SeedSnippet {
            title: "Multi-stage Rust build",
            language: Language::Dockerfile,
            folder: "Containers",
            tags: &["docker", "rust"],
            description: "Build in a Rust image, ship a slim runtime.",
            body: "FROM rust:1 AS builder\n\
             WORKDIR /app\n\
             COPY . .\n\
             RUN cargo build --release --bin ${binary}\n\
             \n\
             FROM debian:bookworm-slim\n\
             COPY --from=builder /app/target/release/${binary} /usr/local/bin/${binary}\n\
             ENTRYPOINT [\"/usr/local/bin/${binary}\"]",
            favorite: false,
            uses: 4,
        }),
        mk(&SeedSnippet {
            title: "Compose service block",
            language: Language::Yaml,
            folder: "Containers",
            tags: &["docker", "compose"],
            description: "A service with a healthcheck and a volume.",
            body: "services:\n\
             \x20 ${name}:\n\
             \x20   image: ${image}\n\
             \x20   ports:\n\
             \x20     - \"${host_port}:${container_port}\"\n\
             \x20   environment:\n\
             \x20     - LOG_LEVEL=${log_level}\n\
             \x20   restart: unless-stopped",
            favorite: false,
            uses: 7,
        }),
    ]
}

fn writing_snippets() -> Vec<Snippet> {
    vec![mk(&SeedSnippet {
        title: "Meeting note",
        language: Language::Markdown,
        folder: "Writing",
        tags: &["template"],
        description: "A reusable meeting-notes scaffold.",
        body: "# ${topic}\n\
         \n\
         **Date:** ${date}  \n\
         **Attendees:** ${attendees}\n\
         \n\
         ## Decisions\n\
         - \n\
         \n\
         ## Action items\n\
         - [ ] ${owner}: ",
        favorite: false,
        uses: 1,
    })]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The starter library is non-trivial, exposes placeholders and has unique ids.
    #[test]
    fn starter_library_is_substantial_templated_and_unique() {
        {
            let lib = starter_library();
            assert!(lib.len() >= 10);
            for s in &lib {
                assert!(!s.title.is_empty());
                assert!(!s.body.is_empty());
            }
            let langs: std::collections::HashSet<_> = lib.iter().map(|s| s.language).collect();
            assert!(langs.contains(&Language::Shell));
            assert!(langs.contains(&Language::Rust));
            assert!(langs.contains(&Language::Sql));
            assert!(langs.contains(&Language::Git));
        }

        {
            let lib = starter_library();
            let with_vars = lib.iter().filter(|s| s.has_placeholders()).count();
            assert!(
                with_vars >= 8,
                "most starter snippets should have variables"
            );
        }

        // Case `titles_are_unique`: a capture and an agent tell the
        // starter snippets apart by name.
        {
            let lib = starter_library();
            let mut titles: Vec<_> = lib.iter().map(|s| s.title.as_str()).collect();
            titles.sort_unstable();
            titles.dedup();
            assert_eq!(titles.len(), lib.len());
        }
    }
}

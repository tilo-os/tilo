//! Starter library seeded on first launch (and for the headless shot).
//!
//! A realistic spread of shell / rust / sql / git / docker snippets with
//! folders, tags, descriptions and `${placeholder}` variables, so a fresh
//! install (or a screenshot) shows the app doing real work rather than an
//! empty shell.

use crate::model::{Language, Snippet};

/// Build the starter set of snippets. Ids are fresh each call, so seeding
/// always produces a distinct library.
pub fn starter_library() -> Vec<Snippet> {
    let mk = |title: &str,
              language: Language,
              folder: &str,
              tags: &[&str],
              description: &str,
              body: &str,
              favorite: bool,
              uses: u32| {
        let mut s = Snippet::blank(folder);
        s.title = title.to_string();
        s.language = language;
        s.tags = tags.iter().map(std::string::ToString::to_string).collect();
        s.description = description.to_string();
        s.body = body.to_string();
        s.favorite = favorite;
        s.uses = uses;
        s
    };

    vec![
        mk(
            "Restart deployment",
            Language::Shell,
            "Ops",
            &["k8s", "deploy"],
            "Roll a Kubernetes deployment in the given namespace.",
            "kubectl -n ${namespace} rollout restart deployment/${deployment}\n\
             kubectl -n ${namespace} rollout status deployment/${deployment} --timeout=120s",
            true,
            12,
        ),
        mk(
            "Tail container logs",
            Language::Shell,
            "Ops",
            &["k8s", "logs"],
            "Follow the last 200 log lines from a pod.",
            "kubectl -n ${namespace} logs -f --tail=200 ${pod}",
            false,
            5,
        ),
        mk(
            "Amend last commit",
            Language::Git,
            "Git",
            &["commit", "fixup"],
            "Reword the most recent commit without changing its contents.",
            "git commit --amend -m \"${message}\"",
            true,
            21,
        ),
        mk(
            "Interactive rebase",
            Language::Git,
            "Git",
            &["rebase", "history"],
            "Squash and reorder the last N commits onto the base branch.",
            "git fetch origin\n\
             git rebase -i origin/${base_branch}",
            false,
            8,
        ),
        mk(
            "Wipe a branch back to remote",
            Language::Git,
            "Git",
            &["reset", "danger"],
            "Discard all local changes and match the upstream branch exactly.",
            "git reset --hard origin/${branch}\n\
             git clean -fd",
            false,
            2,
        ),
        mk(
            "Find recent users",
            Language::Sql,
            "Database",
            &["postgres", "query"],
            "Most recently created accounts, newest first.",
            "SELECT id, email, created_at\n\
             FROM users\n\
             WHERE created_at > now() - interval '${days} days'\n\
             ORDER BY created_at DESC\n\
             LIMIT ${limit};",
            true,
            17,
        ),
        mk(
            "Add a non-null column",
            Language::Sql,
            "Database",
            &["postgres", "migration"],
            "Backfill-safe column add with a default.",
            "ALTER TABLE ${table}\n\
             ADD COLUMN ${column} ${type} NOT NULL DEFAULT ${default};",
            false,
            3,
        ),
        mk(
            "Result-returning main",
            Language::Rust,
            "Rust",
            &["boilerplate"],
            "A main that bubbles errors up with the ? operator.",
            "fn main() -> Result<(), Box<dyn std::error::Error>> {\n\
             \x20   let ${var} = ${value};\n\
             \x20   println!(\"{}\", ${var});\n\
             \x20   Ok(())\n\
             }",
            true,
            9,
        ),
        mk(
            "Derive the usual traits",
            Language::Rust,
            "Rust",
            &["derive"],
            "The everyday derive set for a plain data struct.",
            "#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]\n\
             pub struct ${Name} {\n\
             \x20   pub ${field}: ${Type},\n\
             }",
            false,
            6,
        ),
        mk(
            "Multi-stage Rust build",
            Language::Dockerfile,
            "Containers",
            &["docker", "rust"],
            "Cargo build in a builder image, copy the binary into a slim runtime.",
            "FROM rust:1 AS builder\n\
             WORKDIR /app\n\
             COPY . .\n\
             RUN cargo build --release --bin ${binary}\n\
             \n\
             FROM debian:bookworm-slim\n\
             COPY --from=builder /app/target/release/${binary} /usr/local/bin/${binary}\n\
             ENTRYPOINT [\"/usr/local/bin/${binary}\"]",
            false,
            4,
        ),
        mk(
            "Compose service block",
            Language::Yaml,
            "Containers",
            &["docker", "compose"],
            "A service definition with a healthcheck and a named volume.",
            "services:\n\
             \x20 ${name}:\n\
             \x20   image: ${image}\n\
             \x20   ports:\n\
             \x20     - \"${host_port}:${container_port}\"\n\
             \x20   environment:\n\
             \x20     - LOG_LEVEL=${log_level}\n\
             \x20   restart: unless-stopped",
            false,
            7,
        ),
        mk(
            "Bash strict mode",
            Language::Shell,
            "Ops",
            &["bash", "safety"],
            "The header every robust shell script should start with.",
            "#!/usr/bin/env bash\n\
             set -euo pipefail\n\
             IFS=$'\\n\\t'",
            true,
            14,
        ),
        mk(
            "Pretty git log",
            Language::Git,
            "Git",
            &["log", "alias"],
            "Compact one-line graph of recent history.",
            "git log --graph --oneline --decorate -n ${count}",
            false,
            11,
        ),
        mk(
            "Meeting note",
            Language::Markdown,
            "Writing",
            &["template"],
            "A reusable meeting-notes scaffold.",
            "# ${topic}\n\
             \n\
             **Date:** ${date}  \n\
             **Attendees:** ${attendees}\n\
             \n\
             ## Decisions\n\
             - \n\
             \n\
             ## Action items\n\
             - [ ] ${owner}: ",
            false,
            1,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_library_is_non_trivial() {
        let lib = starter_library();
        assert!(lib.len() >= 10);
        // Every starter snippet has a title and a body.
        for s in &lib {
            assert!(!s.title.is_empty());
            assert!(!s.body.is_empty());
        }
        // A spread of languages is represented.
        let langs: std::collections::HashSet<_> = lib.iter().map(|s| s.language).collect();
        assert!(langs.contains(&Language::Shell));
        assert!(langs.contains(&Language::Rust));
        assert!(langs.contains(&Language::Sql));
        assert!(langs.contains(&Language::Git));
    }

    #[test]
    fn starter_snippets_expose_placeholders() {
        let lib = starter_library();
        let with_vars = lib.iter().filter(|s| s.has_placeholders()).count();
        assert!(
            with_vars >= 8,
            "most starter snippets should have variables"
        );
    }

    #[test]
    fn ids_are_unique() {
        let lib = starter_library();
        let mut ids: Vec<_> = lib.iter().map(|s| s.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), lib.len());
    }
}

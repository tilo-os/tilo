//! The hosts a tool build needs: the package repositories an `env-runtime`
//! image is configured with.
//!
//! A build sandbox gets exactly these, through the same stub and filter as
//! an environment's grants. `baseurl` hosts are taken as they are. A
//! `metalink` or `mirrorlist` names a service that answers with a list of
//! mirrors on other hosts: its own host is allowed, and the mirrors it
//! lists are added with [`metalink_mirror_hosts`] once the document is
//! fetched (or the build pins a `baseurl`).

use std::path::Path;

use crate::policy::{EgressPolicy, HostPattern};

/// What a runtime image's repositories reach.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoHosts {
    /// Hosts of `baseurl`, `metalink` and `mirrorlist` URLs of enabled
    /// repositories.
    pub hosts: Vec<HostPattern>,
    /// The `metalink`/`mirrorlist` URLs, with `$releasever` and `$basearch`
    /// expanded, for a builder that fetches them to learn the mirrors.
    pub mirror_lists: Vec<String>,
}

impl RepoHosts {
    /// Read `etc/yum.repos.d/*.repo` (and `etc/dnf/vars`, `etc/os-release`
    /// for the variables) under `runtime_root`, for `arch`.
    ///
    /// # Errors
    /// Returns the error listing `etc/yum.repos.d`.
    pub fn read(runtime_root: &Path, arch: &str) -> std::io::Result<Self> {
        let vars = DnfVars::read(runtime_root, arch);
        let mut out = Self::default();
        let mut files: Vec<_> = std::fs::read_dir(runtime_root.join("etc/yum.repos.d"))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "repo"))
            .collect();
        files.sort();
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            out.add_repo_file(&text, &vars);
        }
        out.hosts.sort();
        out.hosts.dedup();
        out.mirror_lists.sort();
        out.mirror_lists.dedup();
        Ok(out)
    }

    /// The policy a build sandbox gets: these hosts.
    pub fn policy(&self) -> EgressPolicy {
        EgressPolicy::hosts(self.hosts.clone())
    }

    /// Add the mirrors a fetched metalink/mirrorlist document names.
    pub fn add_mirrors(&mut self, document: &str) {
        for host in metalink_mirror_hosts(document) {
            if let Ok(p) = HostPattern::parse(&host) {
                self.hosts.push(p);
            }
        }
        self.hosts.sort();
        self.hosts.dedup();
    }

    fn add_repo_file(&mut self, text: &str, vars: &DnfVars) {
        for section in sections(text) {
            if section.get("enabled").is_some_and(|v| v.trim() == "0") {
                continue;
            }
            for key in ["baseurl", "metalink", "mirrorlist"] {
                let Some(value) = section.get(key) else {
                    continue;
                };
                for url in value.split(|c: char| c.is_whitespace() || c == ',') {
                    let url = vars.expand(url.trim());
                    if url.is_empty() {
                        continue;
                    }
                    if let Some(host) = url_host(&url)
                        && let Ok(p) = HostPattern::parse(&host)
                    {
                        self.hosts.push(p);
                    }
                    if key != "baseurl" {
                        self.mirror_lists.push(url);
                    }
                }
            }
        }
    }
}

/// One `[section]` of a `.repo` file: its keys, continuation lines joined.
#[derive(Debug, Default)]
struct Section(Vec<(String, String)>);

impl Section {
    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

fn sections(text: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    for raw in text.lines() {
        if raw.trim_start().starts_with(['#', ';']) || raw.trim().is_empty() {
            continue;
        }
        if raw.trim_start().starts_with('[') {
            out.push(Section::default());
            continue;
        }
        let Some(section) = out.last_mut() else {
            continue;
        };
        if raw.starts_with([' ', '\t']) {
            // A continuation of the previous value (a multi-line baseurl).
            if let Some((_, v)) = section.0.last_mut() {
                v.push(' ');
                v.push_str(raw.trim());
            }
            continue;
        }
        if let Some((k, v)) = raw.split_once('=') {
            section
                .0
                .push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
        }
    }
    out
}

/// `$releasever`, `$basearch`, `$arch` and the image's own `etc/dnf/vars`.
#[derive(Debug, Default)]
struct DnfVars(Vec<(String, String)>);

impl DnfVars {
    fn read(root: &Path, arch: &str) -> Self {
        let mut vars = vec![
            ("basearch".to_owned(), arch.to_owned()),
            ("arch".to_owned(), arch.to_owned()),
        ];
        if let Ok(release) = std::fs::read_to_string(root.join("etc/os-release"))
            .or_else(|_| std::fs::read_to_string(root.join("usr/lib/os-release")))
            && let Some(v) = release.lines().find_map(|l| l.strip_prefix("VERSION_ID="))
        {
            vars.push(("releasever".to_owned(), v.trim_matches('"').to_owned()));
        }
        if let Ok(entries) = std::fs::read_dir(root.join("etc/dnf/vars")) {
            for entry in entries.flatten() {
                if let (Some(name), Ok(value)) = (
                    entry.file_name().to_str().map(str::to_owned),
                    std::fs::read_to_string(entry.path()),
                ) {
                    vars.push((name, value.trim().to_owned()));
                }
            }
        }
        // Longest names first, so `$releasever_major` is not read as
        // `$releasever` followed by `_major`.
        vars.sort_by_key(|v| std::cmp::Reverse(v.0.len()));
        Self(vars)
    }

    fn expand(&self, s: &str) -> String {
        let mut out = s.to_owned();
        for (name, value) in &self.0 {
            out = out
                .replace(&format!("${{{name}}}"), value)
                .replace(&format!("${name}"), value);
        }
        out
    }
}

/// The host of a URL: `https://host:port/path` → `host`.
fn url_host(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if authority.starts_with('[') {
        authority.split(']').next()?.trim_start_matches('[')
    } else {
        authority.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// The hosts of the mirror URLs a metalink (`<url …>https://…</url>`) or a
/// plain mirrorlist (one URL per line) names.
pub fn metalink_mirror_hosts(document: &str) -> Vec<String> {
    let mut hosts = Vec::new();
    for piece in document.split('<') {
        let text = if let Some(rest) = piece.strip_prefix("url") {
            rest.split_once('>').map_or("", |(_, body)| body)
        } else if !document.contains('<') {
            piece
        } else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if (line.starts_with("http://") || line.starts_with("https://"))
                && let Some(host) = url_host(line)
            {
                hosts.push(host);
            }
        }
    }
    hosts.sort();
    hosts.dedup();
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_repositories_contribute_their_hosts_and_mirror_lists() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("etc/yum.repos.d")).unwrap();
        std::fs::create_dir_all(root.join("etc/dnf/vars")).unwrap();
        std::fs::write(root.join("etc/os-release"), "NAME=Fedora\nVERSION_ID=44\n").unwrap();
        std::fs::write(root.join("etc/dnf/vars/contentdir"), "fedora\n").unwrap();
        std::fs::write(
            root.join("etc/yum.repos.d/fedora.repo"),
            "[fedora]\nname=Fedora $releasever\n\
             metalink=https://mirrors.fedoraproject.org/metalink?repo=fedora-$releasever&arch=$basearch\n\
             #baseurl=http://download.example/pub/fedora/linux/releases/$releasever/\n\
             enabled=1\n\
             [fedora-source]\nbaseurl=https://source.example.org/x\nenabled=0\n",
        )
        .unwrap();
        std::fs::write(
            root.join("etc/yum.repos.d/pinned.repo"),
            "[pinned]\nbaseurl=https://ftp.fau.de/${contentdir}/linux/\n\
             \thttps://user@Mirror.Example.net:8443/fedora/\n",
        )
        .unwrap();
        std::fs::write(
            root.join("etc/yum.repos.d/notes.txt"),
            "[x]\nbaseurl=https://no.example\n",
        )
        .unwrap();

        let repos = RepoHosts::read(root, "x86_64").unwrap();
        let hosts: Vec<String> = repos.hosts.iter().map(ToString::to_string).collect();
        assert_eq!(
            hosts,
            [
                "ftp.fau.de",
                "mirror.example.net",
                "mirrors.fedoraproject.org"
            ]
        );
        assert_eq!(
            repos.mirror_lists,
            ["https://mirrors.fedoraproject.org/metalink?repo=fedora-44&arch=x86_64"]
        );
        let policy = repos.policy();
        assert!(policy.allows("ftp.fau.de").is_some());
        assert!(
            policy.allows("source.example.org").is_none(),
            "a disabled repo"
        );
        assert!(
            policy.allows("download.example").is_none(),
            "a commented-out url"
        );
    }

    #[test]
    fn a_metalink_names_its_mirrors() {
        let metalink = r#"<?xml version="1.0"?>
<metalink><files><file name="repomd.xml"><resources>
  <url protocol="https" type="https" location="DE" preference="100">https://ftp.fau.de/fedora/linux/releases/44/repodata/repomd.xml</url>
  <url protocol="http" type="http" location="US">http://mirror.example.edu/fedora/repomd.xml</url>
  <url protocol="rsync" type="rsync">rsync://rsync.example.org/fedora/</url>
</resources></file></files></metalink>"#;
        assert_eq!(
            metalink_mirror_hosts(metalink),
            ["ftp.fau.de", "mirror.example.edu"]
        );
        let mirrorlist = "# mirrors\nhttps://a.example/fedora/\nhttps://b.example/fedora/\n";
        assert_eq!(
            metalink_mirror_hosts(mirrorlist),
            ["a.example", "b.example"]
        );

        let mut repos = RepoHosts::default();
        repos.add_mirrors(metalink);
        assert!(repos.policy().allows("mirror.example.edu").is_some());
    }

    #[test]
    fn hosts_are_read_out_of_urls() {
        assert_eq!(url_host("https://a.b/c").as_deref(), Some("a.b"));
        assert_eq!(
            url_host("http://u:p@H.example:80/x?y").as_deref(),
            Some("h.example")
        );
        assert_eq!(
            url_host("https://[2001:db8::1]:443/").as_deref(),
            Some("2001:db8::1")
        );
        assert_eq!(url_host("file:///srv/repo"), None);
        assert_eq!(url_host("no-scheme"), None);
    }
}

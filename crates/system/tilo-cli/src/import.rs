//! `tilo import`: bring another tool's records into a workspace.
//!
//! Each source is read where it lives, with the address and key the person
//! gives, and written through the workspace's own apps' actions: Outline's
//! collections and documents become Notes, Linear's and a repository's issues
//! become Tasks, files go up and open in the app that reads them, and the
//! company's mail accounts are set up in Mail. An app that lacked the fields
//! an import needs was given them (Tasks' source page and issue import, Notes'
//! page import), so an import is not a second way of storing anything.
//!
//! One call of an import is one step of one run: History names the import,
//! and undo takes it back in a step per app. Running an import again follows
//! the source: a record it made is updated where it is, never written twice.

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crate::api::{Client, Reach, Run};

/// The most a page or issue list carries in one action: a batch much larger
/// than this is many steps of the same run.
const BATCH_BYTES: usize = 4 * 1024 * 1024;
/// The largest picture a note keeps. One bigger stays where it is linked.
const NOTE_PICTURE_BYTES: usize = 1024 * 1024;
/// The most one request of a source is given.
const SOURCE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Subcommand)]
pub enum ImportCmd {
    /// Bring Outline's collections and documents in as notes.
    #[command(
        after_help = "Example:\n  tilo import outline --url https://outline.example --token-file outline.token -w company\n\n\
                      Every collection becomes a note holding its documents, and a document that has \
                      one under it becomes its sub-note. Pictures the documents show are kept with \
                      the notes. The API token comes from --token-file, or from TILO_IMPORT_TOKEN."
    )]
    Outline(OutlineArgs),
    /// Bring Linear's issues in as tasks.
    #[command(
        after_help = "Example:\n  tilo import linear --token-file linear.token -w company\n\n\
                      Every issue keeps its identifier, description, state, labels, assignee, \
                      comments and the issues it links to, and links back to its Linear page."
    )]
    Linear(TokenArgs),
    /// Bring a repository's issues in as tasks.
    #[command(
        after_help = "Example:\n  tilo import forgejo --url https://code.example --token-file forgejo.token \
                      --repo tilo/tilo -w company\n\n\
                      Issues of the repository keep their number, description, state, labels, \
                      assignee and comments, and `#42` in one becomes a link to the issue it names."
    )]
    Forgejo(ForgejoArgs),
    /// Bring files and folders in: each opens in the app that reads it.
    #[command(
        after_help = "Example:\n  tilo import files ./contracts ./notes.md -w company\n\n\
                      A folder keeps its structure. A Markdown or text file becomes a note, a \
                      picture joins Photos, and a PDF opens in PDF Viewer; a file no app reads \
                      still goes into the workspace, and prints its reference."
    )]
    Files(FilesArgs),
    /// Set up the company's mail accounts in Mail.
    #[command(
        after_help = "Example:\n  tilo import mail --imap-host imap.example --smtp-host smtp.example \
                      --user ops@example --password-file ops.password\n\n\
                      The account is added through Mail's own save-account action on this \
                      computer, so Mail syncs its mail into the workspace it opens in, where agents \
                      read it. Accounts are the person's: they reach every device, and no -w names \
                      one. Gmail and Outlook need an app password or OAuth: pass --auth oauth and \
                      finish the sign-in in Mail."
    )]
    Mail(MailArgs),
}

#[derive(Args)]
pub struct OutlineArgs {
    /// The Outline site, like https://outline.example.
    #[arg(long)]
    url: String,
    /// A file holding the API token; --token-env reads it from TILO_IMPORT_TOKEN.
    #[arg(long, conflicts_with = "token")]
    token_file: Option<PathBuf>,
    /// Read the API token from TILO_IMPORT_TOKEN instead.
    #[arg(long)]
    token_env: bool,
    /// Only these collections, by id; every collection without.
    #[arg(long = "collection")]
    collections: Vec<String>,
}

#[derive(Args)]
pub struct TokenArgs {
    /// A file holding the API token; --token-env reads it from TILO_IMPORT_TOKEN.
    #[arg(long, conflicts_with = "token")]
    token_file: Option<PathBuf>,
    /// Read the API token from TILO_IMPORT_TOKEN instead.
    #[arg(long)]
    token_env: bool,
    /// The API endpoint, when the tool is not at its usual one.
    #[arg(long)]
    url: Option<String>,
}

#[derive(Args)]
pub struct ForgejoArgs {
    /// The Forgejo site, like https://code.example.
    #[arg(long)]
    url: String,
    /// A file holding the API token; --token-env reads it from TILO_IMPORT_TOKEN.
    #[arg(long, conflicts_with = "token")]
    token_file: Option<PathBuf>,
    /// Read the API token from TILO_IMPORT_TOKEN instead.
    #[arg(long)]
    token_env: bool,
    /// The repository whose issues come in, as owner/name.
    #[arg(long = "repo")]
    repos: Vec<String>,
}

#[derive(Args)]
pub struct FilesArgs {
    /// The files and folders to bring in.
    #[arg(required = true)]
    paths: Vec<PathBuf>,
}

#[derive(Args)]
pub struct MailArgs {
    /// The address the account signs in as.
    #[arg(long)]
    user: String,
    /// What Mail calls the account; the address without.
    #[arg(long)]
    name: Option<String>,
    /// The IMAP host, like imap.example.
    #[arg(long)]
    imap_host: String,
    /// Its port: 993 unless given.
    #[arg(long)]
    imap_port: Option<u16>,
    /// The SMTP host, like smtp.example.
    #[arg(long)]
    smtp_host: String,
    /// Its port: 465 unless given.
    #[arg(long)]
    smtp_port: Option<u16>,
    /// A file holding the password; without one, it is asked for.
    #[arg(long)]
    password_file: Option<PathBuf>,
    /// password, or oauth for a provider that blocks password sign-in.
    #[arg(long, default_value = "password")]
    auth: String,
    /// Print the account Mail would be given, and change nothing.
    #[arg(long)]
    dry_run: bool,
}

/// `tilo import`: the source `cmd` read and written into `workspace`.
pub fn run(reach: Reach<'_>, workspace: Option<String>, cmd: ImportCmd) -> Result<ExitCode> {
    // Mail's accounts are the person's, not a workspace's: they reach every
    // device, and Mail syncs their mail into whichever workspace it opens in.
    if let ImportCmd::Mail(args) = cmd {
        return mail(args);
    }
    let Some(workspace) = workspace else {
        bail!("name the workspace to import into: -w <name or id>");
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let client = Client::open(reach)?;
        let workspace = crate::hosted::workspace_id(&client, &workspace).await?;
        match cmd {
            ImportCmd::Outline(args) => outline(&client, &workspace, args).await,
            ImportCmd::Linear(args) => linear(&client, &workspace, args).await,
            ImportCmd::Forgejo(args) => forgejo(&client, &workspace, args).await,
            ImportCmd::Files(args) => files(&client, &workspace, args).await,
            ImportCmd::Mail(_) => unreachable!("mail is answered above"),
        }
    })
}

/// Where an import writes: a workspace, through its apps' actions, or, in a
/// test, a record of the calls it would have made.
trait Target {
    async fn act(&self, resource: &str, action: &str, args: &Value, title: &str) -> Result<Value>;
    async fn upload(&self, bytes: Vec<u8>) -> Result<String>;
    fn progress(&self, line: &str);
}

/// A workspace, written through the API as one run.
struct Api<'a> {
    client: &'a Client,
    workspace: &'a str,
    run: Run,
}

impl Target for Api<'_> {
    async fn act(&self, resource: &str, action: &str, args: &Value, title: &str) -> Result<Value> {
        self.client
            .act_in(self.workspace, resource, action, args, &self.run)
            .await
            .with_context(|| format!("{resource}#{action} ({title})"))
    }

    async fn upload(&self, bytes: Vec<u8>) -> Result<String> {
        let answer = self.client.upload(self.workspace, bytes).await?;
        Ok(answer
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned())
    }

    fn progress(&self, line: &str) {
        eprintln!("tilo import: {line}");
    }
}

/// A source's API: its base address and the key it takes requests with.
struct Source {
    http: tilo_net::Client,
    base: String,
    token: String,
}

impl Source {
    fn new(base: &str, token: String) -> Self {
        Self {
            http: tilo_net::Client::new(),
            base: base.trim_end_matches('/').to_owned(),
            token,
        }
    }

    /// `POST path` with `body` as JSON, as `header` names the key's place.
    async fn post_json(&self, path: &str, body: &Value, header: Key) -> Result<Value> {
        let url = format!("{}{path}", self.base);
        let mut request = self
            .http
            .post(&url)
            .timeout(SOURCE_TIMEOUT)
            .header("content-type", "application/json");
        request = self.authorized(request, header);
        let response = request
            .body(body.to_string().into_bytes())
            .send()
            .await
            .with_context(|| format!("reaching {url}"))?;
        let status = response.status();
        let bytes = response
            .bytes_limited(64 * 1024 * 1024)
            .await
            .with_context(|| format!("reading {url}"))?;
        if !status.is_success() {
            let said = String::from_utf8_lossy(&bytes);
            bail!("{url} answered {status}: {}", said.trim());
        }
        serde_json::from_slice(&bytes).with_context(|| format!("{url} answered something else"))
    }

    /// `GET path` (a route with its query) as JSON.
    async fn get_json(&self, path: &str, header: Key) -> Result<Value> {
        let url = format!("{}{path}", self.base);
        let mut request = self.http.get(&url).timeout(SOURCE_TIMEOUT);
        request = self.authorized(request, header);
        let response = request
            .send()
            .await
            .with_context(|| format!("reaching {url}"))?;
        let status = response.status();
        let bytes = response
            .bytes_limited(64 * 1024 * 1024)
            .await
            .with_context(|| format!("reading {url}"))?;
        if !status.is_success() {
            let said = String::from_utf8_lossy(&bytes);
            bail!("{url} answered {status}: {}", said.trim());
        }
        serde_json::from_slice(&bytes).with_context(|| format!("{url} answered something else"))
    }

    /// The bytes at `url` (relative to the source's own base or absolute),
    /// with the content type the source names them by.
    async fn get_bytes(&self, url: &str) -> Result<(String, Vec<u8>)> {
        let url = if url.starts_with("http://") || url.starts_with("https://") {
            url.to_owned()
        } else if url.starts_with('/') {
            format!("{}{url}", self.base)
        } else {
            format!("{}/{url}", self.base)
        };
        let response = self
            .http
            .get(&url)
            .timeout(SOURCE_TIMEOUT)
            .bearer_auth(&self.token)
            .send()
            .await
            .with_context(|| format!("reaching {url}"))?;
        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let bytes = response
            .bytes_limited(64 * 1024 * 1024)
            .await
            .with_context(|| format!("reading {url}"))?;
        if !status.is_success() {
            bail!("{url} answered {status}");
        }
        Ok((content_type, bytes.to_vec()))
    }

    fn authorized(
        &self,
        request: tilo_net::RequestBuilder,
        header: Key,
    ) -> tilo_net::RequestBuilder {
        match header {
            Key::Bearer => request.bearer_auth(&self.token),
            Key::Raw => request.header("authorization", self.token.as_str()),
            Key::Token => request.header("authorization", format!("token {}", self.token)),
        }
    }
}

/// Where a source's API takes its key.
#[derive(Clone, Copy)]
enum Key {
    /// `Authorization: Bearer <token>`: Outline.
    Bearer,
    /// `Authorization: <token>`: Linear.
    Raw,
    /// `Authorization: token <token>`: Forgejo.
    Token,
}

/// The token `file` holds, or the one `TILO_IMPORT_TOKEN` does.
fn token(token_file: Option<&Path>, from_env: bool) -> Result<String> {
    let token = match (token_file, from_env) {
        (Some(path), _) => {
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
        }
        (None, _) => std::env::var("TILO_IMPORT_TOKEN")
            .context("reading TILO_IMPORT_TOKEN: pass --token-file or --token-env")?,
    };
    let token = token.trim().to_owned();
    if token.is_empty() {
        bail!("the import's token is empty");
    }
    Ok(token)
}

// ---- Outline -----------------------------------------------------------------

/// Outline's collections and documents, as the pages Notes imports.
async fn outline(client: &Client, workspace: &str, args: OutlineArgs) -> Result<ExitCode> {
    let source = Source::new(
        &args.url,
        token(args.token_file.as_deref(), args.token_env)?,
    );
    let target = Api {
        client,
        workspace,
        run: Run::new("Import Outline"),
    };
    target.progress("reading Outline's collections");
    let collections = outline_collections(&source).await?;
    let mut wanted = Vec::new();
    for collection in &collections {
        let id = text(collection, "id");
        if args.collections.is_empty() || args.collections.iter().any(|want| want == id) {
            wanted.push(collection.clone());
        }
    }
    if !args.collections.is_empty() && wanted.is_empty() {
        bail!("Outline has none of those collections");
    }
    target.progress(&format!(
        "{} collections, reading their documents",
        wanted.len()
    ));
    let mut pages = Vec::new();
    for collection in &wanted {
        let key = format!("collection:{}", text(collection, "id"));
        pages.push(json!({
            "source": "outline",
            "key": key,
            "title": text(collection, "name"),
            "markdown": text(collection, "description"),
            "tags": [],
            "images": [],
        }));
    }
    let mut documents = 0_usize;
    for collection in &wanted {
        let collection_id = text(collection, "id");
        for document in outline_documents(&source, collection_id).await? {
            documents = documents.saturating_add(1);
            let page = outline_page(&source, &target, &document).await?;
            pages.push(page);
        }
    }
    target.progress(&format!(
        "{documents} documents read, writing {} notes",
        pages.len()
    ));
    let written = write_notes(&target, pages).await?;
    target.progress(&format!("imported {written} notes from Outline"));
    Ok(ExitCode::SUCCESS)
}

async fn outline_collections(source: &Source) -> Result<Vec<Value>> {
    let mut collections = Vec::new();
    let mut offset = 0_u64;
    loop {
        let answer = source
            .post_json(
                "/api/collections.list",
                &json!({"limit": 100, "offset": offset}),
                Key::Bearer,
            )
            .await?;
        let data = answer
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if data.is_empty() {
            break;
        }
        let count = data.len();
        collections.extend(data);
        if count < 100 {
            break;
        }
        offset = offset.saturating_add(100);
    }
    Ok(collections)
}

async fn outline_documents(source: &Source, collection: &str) -> Result<Vec<Value>> {
    let mut documents = Vec::new();
    let mut offset = 0_u64;
    loop {
        let answer = source
            .post_json(
                "/api/documents.list",
                &json!({"collectionId": collection, "limit": 100, "offset": offset}),
                Key::Bearer,
            )
            .await?;
        let data = answer
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if data.is_empty() {
            break;
        }
        let count = data.len();
        documents.extend(data);
        if count < 100 {
            break;
        }
        offset = offset.saturating_add(100);
    }
    Ok(documents)
}

/// One Outline document as a page: its words, the pictures they show, and
/// the page it sits under.
async fn outline_page(source: &Source, target: &impl Target, document: &Value) -> Result<Value> {
    let key = text(document, "id").to_owned();
    let title = text(document, "title").to_owned();
    let mut markdown = text(document, "text").to_owned();
    if markdown.trim().is_empty() {
        markdown = text(document, "description").to_owned();
    }
    let parent = document
        .get("parentDocumentId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            document
                .get("collectionId")
                .and_then(Value::as_str)
                .map(|collection| format!("collection:{collection}"))
        });
    let mut images = Vec::new();
    for url in shown_pictures(&markdown) {
        let Ok((content_type, bytes)) = source.get_bytes(&url).await else {
            continue;
        };
        if !content_type.starts_with("image/") || bytes.len() > NOTE_PICTURE_BYTES {
            continue;
        }
        let content = target.upload(bytes).await?;
        images.push(json!({"url": url, "bytes": content}));
    }
    Ok(json!({
        "source": "outline",
        "key": key,
        "title": title,
        "markdown": markdown,
        "parent": parent,
        "tags": [],
        "images": images,
    }))
}

/// The addresses `markdown` shows pictures at, as it writes them.
fn shown_pictures(markdown: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut rest = markdown;
    while let Some(at) = rest.find("![") {
        let after = &rest[at.saturating_add(2)..];
        let Some(close) = after.find("](") else {
            break;
        };
        let destination = &after[close.saturating_add(2)..];
        let Some(end) = destination.find(')') else {
            break;
        };
        let url = destination[..end].trim().trim_matches(['<', '>']);
        if !url.is_empty()
            && !url.starts_with("data:")
            && !url.starts_with("asset:")
            && !urls.iter().any(|known: &String| known == url)
        {
            urls.push(url.to_owned());
        }
        rest = &destination[end.saturating_add(1)..];
    }
    urls
}

// ---- Linear ------------------------------------------------------------------

async fn linear(client: &Client, workspace: &str, args: TokenArgs) -> Result<ExitCode> {
    let token = token(args.token_file.as_deref(), args.token_env)?;
    let base = args
        .url
        .unwrap_or_else(|| "https://api.linear.app".to_owned());
    let source = Source::new(&base, token);
    let target = Api {
        client,
        workspace,
        run: Run::new("Import Linear"),
    };
    target.progress("reading Linear's issues");
    let issues = linear_issues(&source).await?;
    target.progress(&format!("{} issues read", issues.len()));
    let written = write_issues(&target, issues).await?;
    target.progress(&format!("imported {written} issues from Linear"));
    Ok(ExitCode::SUCCESS)
}

const LINEAR_QUERY: &str = r"
query TiloImport($after: String) {
  issues(first: 100, after: $after, includeArchived: false) {
    pageInfo { hasNextPage endCursor }
    nodes {
      identifier
      title
      description
      url
      createdAt
      state { name type }
      assignee { name }
      labels { nodes { name } }
      parent { identifier }
      relations { nodes { relatedIssue { identifier } } }
      comments(first: 100) {
        nodes { body createdAt user { name } }
      }
    }
  }
}
";

async fn linear_issues(source: &Source) -> Result<Vec<Value>> {
    let mut issues = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let body = json!({"query": LINEAR_QUERY, "variables": {"after": after}});
        let answer = source.post_json("/graphql", &body, Key::Raw).await?;
        if let Some(errors) = answer.get("errors").filter(|errors| !errors.is_null()) {
            bail!("Linear refused the query: {errors}");
        }
        let connection = &answer["data"]["issues"];
        let nodes = connection
            .get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for issue in nodes {
            issues.push(linear_issue(&issue));
        }
        let more = connection
            .pointer("/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !more {
            break;
        }
        after = connection
            .pointer("/pageInfo/endCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if after.is_none() {
            break;
        }
    }
    Ok(issues)
}

fn linear_issue(issue: &Value) -> Value {
    let key = text(issue, "identifier").to_owned();
    let comments = issue
        .pointer("/comments/nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|comment| {
            json!({
                "author": text(comment.pointer("/user").unwrap_or(&Value::Null), "name"),
                "at_ms": iso_ms(text(comment, "createdAt")),
                "text": text(comment, "body"),
            })
        })
        .collect::<Vec<_>>();
    let labels = issue
        .pointer("/labels/nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|label| text(label, "name").to_owned())
        .collect::<Vec<_>>();
    let links = issue
        .pointer("/relations/nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|relation| {
            relation
                .pointer("/relatedIssue/identifier")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    let assignee = issue
        .pointer("/assignee/name")
        .and_then(Value::as_str)
        .map(|name| format!("person:{}", name.trim()))
        .filter(|assignee| assignee != "person:");
    json!({
        "source": "linear",
        "key": key,
        "title": text(issue, "title"),
        "body": text(issue, "description"),
        "status": linear_status(issue.pointer("/state/type").and_then(Value::as_str)),
        "assignee": assignee,
        "author": "",
        "labels": labels,
        "created_ms": iso_ms(text(issue, "createdAt")),
        "url": text(issue, "url"),
        "comments": comments,
        "links": links,
        "parent": issue.pointer("/parent/identifier").and_then(Value::as_str),
    })
}

/// Linear's state type, as the tracker's status.
fn linear_status(kind: Option<&str>) -> &'static str {
    match kind {
        Some("completed") => "done",
        Some("canceled") => "cancelled",
        Some("started") => "in-progress",
        Some("unstarted") => "todo",
        _ => "backlog",
    }
}

// ---- Forgejo -----------------------------------------------------------------

async fn forgejo(client: &Client, workspace: &str, args: ForgejoArgs) -> Result<ExitCode> {
    if args.repos.is_empty() {
        bail!("name the repositories to import: --repo owner/name");
    }
    let source = Source::new(
        &args.url,
        token(args.token_file.as_deref(), args.token_env)?,
    );
    let target = Api {
        client,
        workspace,
        run: Run::new("Import Forgejo issues"),
    };
    let mut issues = Vec::new();
    for repo in &args.repos {
        let (owner, name) = repo
            .split_once('/')
            .ok_or_else(|| anyhow!("--repo takes owner/name, not {repo:?}"))?;
        target.progress(&format!("reading {repo}'s issues"));
        let repo_issues = forgejo_issues(&source, owner, name).await?;
        for issue in &repo_issues {
            issues.push(forgejo_issue(&source, owner, name, issue).await?);
        }
        forgejo_links(&mut issues, owner, name);
    }
    target.progress(&format!("{} issues read", issues.len()));
    let written = write_issues(&target, issues).await?;
    target.progress(&format!("imported {written} issues from Forgejo"));
    Ok(ExitCode::SUCCESS)
}

async fn forgejo_issues(source: &Source, owner: &str, repo: &str) -> Result<Vec<Value>> {
    let mut issues = Vec::new();
    let mut page = 1_u64;
    loop {
        let path = format!(
            "/api/v1/repos/{owner}/{repo}/issues?state=all&type=issues&limit=50&page={page}"
        );
        let answer = source.get_json(&path, Key::Token).await?;
        let Some(rows) = answer.as_array() else {
            break;
        };
        if rows.is_empty() {
            break;
        }
        let count = rows.len();
        issues.extend(rows.iter().cloned());
        if count < 50 {
            break;
        }
        page = page.saturating_add(1);
    }
    Ok(issues)
}

async fn forgejo_issue(source: &Source, owner: &str, repo: &str, issue: &Value) -> Result<Value> {
    let number = issue
        .get("number")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let key = format!("{owner}/{repo}#{number}");
    let mut comments = Vec::new();
    let mut page = 1_u64;
    loop {
        let path =
            format!("/api/v1/repos/{owner}/{repo}/issues/{number}/comments?limit=50&page={page}");
        let answer = source.get_json(&path, Key::Token).await?;
        let Some(rows) = answer.as_array() else {
            break;
        };
        if rows.is_empty() {
            break;
        }
        for comment in rows {
            comments.push(json!({
                "author": comment
                    .get("user")
                    .and_then(|user| user.get("login"))
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                "at_ms": iso_ms(comment.get("created_at").and_then(Value::as_str).unwrap_or_default()),
                "text": comment.get("body").and_then(Value::as_str).unwrap_or_default(),
            }));
        }
        if rows.len() < 50 {
            break;
        }
        page = page.saturating_add(1);
    }
    let labels = issue
        .get("labels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|label| label.get("name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let assignee = issue
        .get("assignee")
        .and_then(|assignee| assignee.get("login"))
        .and_then(Value::as_str)
        .map(|login| format!("person:{login}"));
    let closed = issue.get("state").and_then(Value::as_str) == Some("closed");
    Ok(json!({
        "source": "forgejo",
        "key": key,
        "title": issue.get("title").and_then(Value::as_str).unwrap_or_default(),
        "body": issue.get("body").and_then(Value::as_str).unwrap_or_default(),
        "status": if closed { "done" } else { "todo" },
        "assignee": assignee,
        "author": issue
            .get("user")
            .and_then(|user| user.get("login"))
            .and_then(Value::as_str)
            .unwrap_or_default(),
        "labels": labels,
        "created_ms": iso_ms(issue.get("created_at").and_then(Value::as_str).unwrap_or_default()),
        "url": issue.get("html_url").and_then(Value::as_str).unwrap_or_default(),
        "comments": comments,
        "links": [],
        "parent": null,
    }))
}

/// `#42` in an issue's words is the issue it names, when this import brings
/// that issue too.
fn forgejo_links(issues: &mut [Value], owner: &str, repo: &str) {
    let known: Vec<String> = issues
        .iter()
        .map(|issue| text(issue, "key").to_owned())
        .collect();
    for issue in issues {
        let key = text(issue, "key").to_owned();
        let body = text(issue, "body").to_owned();
        let mut links = Vec::new();
        for number in cross_references(&body) {
            let target = format!("{owner}/{repo}#{number}");
            if target != key && known.contains(&target) {
                links.push(target);
            }
        }
        if !links.is_empty() {
            issue["links"] = json!(links);
        }
    }
}

/// The issue numbers `text` names as `#42`.
fn cross_references(text: &str) -> Vec<u64> {
    let mut numbers = Vec::new();
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'#' {
            at = at.saturating_add(1);
            continue;
        }
        let start = at;
        at = at.saturating_add(1);
        let digits = at;
        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at = at.saturating_add(1);
        }
        // A word before it makes it a fragment, not an issue.
        let starts_word = start == 0 || !bytes[start.saturating_sub(1)].is_ascii_alphanumeric();
        if at > digits
            && starts_word
            && let Ok(number) = text[digits..at].parse::<u64>()
            && !numbers.contains(&number)
        {
            numbers.push(number);
        }
    }
    numbers
}

// ---- writing ----------------------------------------------------------------

/// The pages written, in batches: one call per app step of the run.
async fn write_notes(target: &impl Target, pages: Vec<Value>) -> Result<usize> {
    let mut written = 0_usize;
    for batch in batches(pages) {
        let count = batch.len();
        target
            .act(
                "notes",
                "import-pages",
                &json!({"pages": batch}),
                "Import pages",
            )
            .await?;
        written = written.saturating_add(count);
        target.progress(&format!("{written} notes written"));
    }
    Ok(written)
}

async fn write_issues(target: &impl Target, issues: Vec<Value>) -> Result<usize> {
    let mut written = 0_usize;
    for batch in batches(issues) {
        let count = batch.len();
        target
            .act(
                "tasks",
                "import-issues",
                &json!({"issues": batch}),
                "Import issues",
            )
            .await?;
        written = written.saturating_add(count);
        target.progress(&format!("{written} issues written"));
    }
    Ok(written)
}

/// `items` in batches no larger than one action comfortably carries.
fn batches(items: Vec<Value>) -> Vec<Vec<Value>> {
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut size = 0_usize;
    for item in items {
        let weight = item.to_string().len();
        if !current.is_empty() && size.saturating_add(weight) > BATCH_BYTES {
            batches.push(std::mem::take(&mut current));
            size = 0;
        }
        size = size.saturating_add(weight);
        current.push(item);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

// ---- files -------------------------------------------------------------------

async fn files(client: &Client, workspace: &str, args: FilesArgs) -> Result<ExitCode> {
    let target = Api {
        client,
        workspace,
        run: Run::new("Import files"),
    };
    let (files, notes) = import_files(&target, &args.paths).await?;
    target.progress(&format!("imported {files} files in {notes} notes"));
    Ok(ExitCode::SUCCESS)
}

/// Walk `paths`, upload every file, open it in the app that reads it, and
/// write the folders and files as notes. Answers how many files went up and
/// how many notes hold them.
async fn import_files(target: &impl Target, paths: &[PathBuf]) -> Result<(usize, usize)> {
    let found = files_under(paths)?;
    if found.is_empty() {
        bail!("nothing to import: those paths hold no files");
    }
    target.progress(&format!("{} files found", found.len()));
    // The folders, so what is in them has somewhere to sit.
    let mut pages = Vec::new();
    let mut folders: Vec<String> = found
        .iter()
        .flat_map(|file| folders_of(&file.name))
        .collect();
    folders.sort();
    folders.dedup();
    for folder in &folders {
        pages.push(json!({
            "source": "files",
            "key": format!("folder:{folder}"),
            "title": folder.rsplit('/').next().unwrap_or(folder),
            "markdown": "",
            "parent": folders_of(folder).last().map(|parent| format!("folder:{parent}")),
            "tags": [],
            "images": [],
        }));
    }
    let mut landed = Vec::new();
    for file in &found {
        let bytes = std::fs::read(&file.path)
            .with_context(|| format!("reading {}", file.path.display()))?;
        let content = target.upload(bytes.clone()).await?;
        let opened = open_in_app(target, &file.name, &bytes, &content).await;
        let body = match opened {
            Ok(Some(record)) => format!("Opened in {record}.\n\nThe file is `{content}`."),
            Ok(None) => format!("The file is `{content}`."),
            Err(why) => format!("The file is `{content}`. No app opened it: {why}"),
        };
        let parent = folders_of(&file.name)
            .last()
            .map(|folder| format!("folder:{folder}"));
        pages.push(json!({
            "source": "files",
            "key": format!("file:{}", file.name),
            "title": file.name.rsplit('/').next().unwrap_or(&file.name),
            "markdown": if is_markdown(&file.name) { text_of(&bytes) } else { body },
            "parent": parent,
            "tags": [],
            "images": [],
        }));
        landed.push(file.name.clone());
        target.progress(&format!("{} uploaded", file.name));
    }
    let written = write_notes(target, pages).await?;
    Ok((landed.len(), written))
}

/// One file to bring in: where it is here, and the name it keeps in the
/// workspace.
struct Found {
    path: PathBuf,
    name: String,
}

fn files_under(paths: &[PathBuf]) -> Result<Vec<Found>> {
    let mut found = Vec::new();
    for path in paths {
        let meta = std::fs::symlink_metadata(path)
            .with_context(|| format!("reading {}", path.display()))?;
        if meta.is_file() {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            found.push(Found {
                path: path.clone(),
                name,
            });
            continue;
        }
        if !meta.is_dir() {
            bail!("{} is not a file or a folder", path.display());
        }
        let root = path.parent().unwrap_or_else(|| Path::new(""));
        let mut walking = vec![path.clone()];
        while let Some(dir) = walking.pop() {
            for entry in
                std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?
            {
                let entry = entry.with_context(|| format!("reading {}", dir.display()))?;
                let kind = entry
                    .file_type()
                    .with_context(|| format!("reading {}", entry.path().display()))?;
                if kind.is_dir() {
                    walking.push(entry.path());
                } else if kind.is_file() {
                    let name = entry
                        .path()
                        .strip_prefix(root)
                        .unwrap_or(&entry.path())
                        .to_string_lossy()
                        .replace('\\', "/");
                    found.push(Found {
                        path: entry.path(),
                        name,
                    });
                }
            }
        }
    }
    found.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(found)
}

/// The folders a file at `name` sits in, outer first.
fn folders_of(name: &str) -> Vec<String> {
    let mut folders = Vec::new();
    let mut at = String::new();
    let parts: Vec<&str> = name.split('/').collect();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        if !at.is_empty() {
            at.push('/');
        }
        at.push_str(part);
        folders.push(at.clone());
    }
    folders
}

fn is_markdown(name: &str) -> bool {
    matches!(
        extension(name).as_deref(),
        Some("md" | "markdown" | "mdown" | "mkd" | "txt")
    )
}

fn extension(name: &str) -> Option<String> {
    Path::new(name)
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
}

fn text_of(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Put the file in the app that reads it, and answer the record it landed in.
async fn open_in_app(
    target: &impl Target,
    name: &str,
    bytes: &[u8],
    content: &str,
) -> Result<Option<String>> {
    let title = format!("Import {name}");
    let Some(extension) = extension(name) else {
        return Ok(None);
    };
    match extension.as_str() {
        "md" | "markdown" | "mdown" | "mkd" | "txt" => Ok(None),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" => {
            let Some((width, height)) = image_size(bytes) else {
                return Ok(None);
            };
            let answer = target
                .act(
                    "photos",
                    "import-photo",
                    &json!({"image": {"name": name, "width": width, "height": height, "content": content}}),
                    &title,
                )
                .await?;
            Ok(record_of(&answer))
        }
        "pdf" => {
            let answer = target
                .act(
                    "pdf-viewer",
                    "import-pdf-content",
                    &json!({"content": content, "name": name}),
                    &title,
                )
                .await?;
            Ok(record_of(&answer))
        }
        _ => Ok(None),
    }
}

fn record_of(answer: &Value) -> Option<String> {
    answer
        .get("path")
        .and_then(Value::as_str)
        .and_then(|path| path.split_once('#'))
        .map(|(record, _)| record.to_owned())
}

/// A picture's size in pixels, as its own header states it: PNG, GIF, JPEG
/// and WebP.
fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
        let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
        return Some((width, height));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        let width = u32::from(u16::from_le_bytes(bytes.get(6..8)?.try_into().ok()?));
        let height = u32::from(u16::from_le_bytes(bytes.get(8..10)?.try_into().ok()?));
        return Some((width, height));
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        match bytes.get(12..16)? {
            b"VP8X" => {
                let width = 1 + u32::from_le_bytes([
                    bytes.get(24).copied()?,
                    bytes.get(25).copied()?,
                    bytes.get(26).copied()?,
                    0,
                ]);
                let height = 1 + u32::from_le_bytes([
                    bytes.get(27).copied()?,
                    bytes.get(28).copied()?,
                    bytes.get(29).copied()?,
                    0,
                ]);
                return Some((width, height));
            }
            b"VP8 " => {
                let width =
                    u32::from(u16::from_le_bytes(bytes.get(26..28)?.try_into().ok()?) & 0x3fff);
                let height =
                    u32::from(u16::from_le_bytes(bytes.get(28..30)?.try_into().ok()?) & 0x3fff);
                return Some((width, height));
            }
            b"VP8L" => {
                let bits = u32::from_le_bytes(bytes.get(21..25)?.try_into().ok()?);
                return Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1));
            }
            _ => return None,
        }
    }
    if bytes.starts_with(b"\xff\xd8") {
        let mut at = 2;
        while at + 9 < bytes.len() {
            if bytes[at] != 0xff {
                at = at.saturating_add(1);
                continue;
            }
            let marker = bytes[at + 1];
            let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
            if marker >= 0xc0 && marker <= 0xcf && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
                let height = u32::from(u16::from_be_bytes([bytes[at + 5], bytes[at + 6]]));
                let width = u32::from(u16::from_be_bytes([bytes[at + 7], bytes[at + 8]]));
                return Some((width, height));
            }
            at = at.saturating_add(2).saturating_add(length);
        }
    }
    None
}

// ---- mail --------------------------------------------------------------------

fn mail(args: MailArgs) -> Result<ExitCode> {
    let json = mail_account(&args)?;
    if args.dry_run {
        println!("{}", serde_json::to_string_pretty(&json)?);
        return Ok(ExitCode::SUCCESS);
    }
    let spelling = tilo_control::front::Spelling::tilo();
    let doors = crate::desktop::doors();
    if !crate::desktop::available() {
        bail!(
            "no Tilo desktop is running here: Mail's accounts are this person's, so run \
             `tilo import mail` on the computer whose Tilo runs Mail (the accounts then reach \
             every device, and Mail syncs the mail into this workspace)"
        );
    }
    let address = tilo_actions::ActionAddress::new("tilo.accounts", "save-account").encode();
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    let mut io = tilo_control::front::Io {
        out: &mut out,
        err: &mut err,
    };
    let code = tilo_control::front::run_address(
        &doors,
        ("so.tilo.mail", &address, "Set up a mail account"),
        &json,
        &spelling,
        &mut io,
    );
    Ok(if code == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The arguments Mail's save-account action takes for this account.
fn mail_account(args: &MailArgs) -> Result<Value> {
    let password = match &args.password_file {
        Some(path) => Some(
            std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?
                .trim_end_matches(['\n', '\r'])
                .to_owned(),
        ),
        None => None,
    };
    let auth = match args.auth.as_str() {
        "password" => "password",
        "oauth" => "oauth",
        other => bail!("--auth is password or oauth, not {other:?}"),
    };
    if auth == "password" && password.is_none() {
        bail!("--password-file holds the account's password (or pass --auth oauth)");
    }
    let config = json!({
        "display_name": args.name.clone().unwrap_or_default(),
        "email": args.user,
        "imap_host": args.imap_host,
        "imap_port": args.imap_port.unwrap_or(993),
        "imap_tls": true,
        "smtp_host": args.smtp_host,
        "smtp_port": args.smtp_port.unwrap_or(465),
        "smtp_tls": true,
        "username": args.user,
        "auth": auth,
    });
    // The account's id is its address, so running the import again updates
    // the account it made instead of adding another.
    let id = format!("mail-{}", crate::hosted::slug(&args.user));
    let mut account = json!({
        "id": id,
        "type_id": "email",
        "display_name": args.name.clone().unwrap_or_else(|| args.user.clone()),
        "config": config,
        "enabled": true,
        "tint": 0,
    });
    if let Some(password) = password {
        account["config"]["password"] = json!(password);
    }
    Ok(account)
}

// ---- shared ------------------------------------------------------------------

fn text<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or_default()
}

/// `text` as milliseconds since the epoch: an RFC 3339 time as sources write
/// it. A time it cannot read is zero.
fn iso_ms(text: &str) -> i64 {
    let bytes = text.as_bytes();
    let number = |from: usize, to: usize| -> Option<i64> {
        let slice = bytes.get(from..to)?;
        std::str::from_utf8(slice).ok()?.parse::<i64>().ok()
    };
    let (Some(year), Some(month), Some(day)) = (number(0, 4), number(5, 7), number(8, 10)) else {
        return 0;
    };
    let (hour, minute, second) = match (number(11, 13), number(14, 16), number(17, 19)) {
        (Some(hour), Some(minute), Some(second)) => (hour, minute, second),
        _ => (0, 0, 0),
    };
    let millis = text
        .get(20..23)
        .and_then(|fraction| fraction.parse::<i64>().ok())
        .unwrap_or(0);
    // Days from the civil date, by Howard Hinnant's count.
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second;
    seconds * 1000 + millis
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// What an import asked the workspace to do.
    #[derive(Clone, Debug)]
    struct Call {
        resource: String,
        action: String,
        args: Value,
    }

    #[derive(Default, Clone)]
    struct Recording(Arc<Mutex<Vec<Call>>>);

    impl Target for Recording {
        async fn act(
            &self,
            resource: &str,
            action: &str,
            args: &Value,
            _title: &str,
        ) -> Result<Value> {
            self.0.lock().unwrap().push(Call {
                resource: resource.to_owned(),
                action: action.to_owned(),
                args: args.clone(),
            });
            Ok(json!({"status": "ok", "path": format!("{resource}/record#work")}))
        }

        async fn upload(&self, bytes: Vec<u8>) -> Result<String> {
            Ok(format!("tilo.content:{}", bytes.len()))
        }

        fn progress(&self, _line: &str) {}
    }

    impl Recording {
        fn calls(&self) -> Vec<Call> {
            self.0.lock().unwrap().clone()
        }
    }

    /// A source fixture: an HTTP server answering each path with the JSON it
    /// was given, once, on the loopback.
    struct Fixture {
        base: String,
        _thread: std::thread::JoinHandle<()>,
    }

    impl Fixture {
        fn serve(routes: Vec<(String, Value)>) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let routes = Arc::new(routes);
            let thread = std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { break };
                    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                    let mut request_line = String::new();
                    use std::io::{BufRead as _, Read as _, Write as _};
                    if reader.read_line(&mut request_line).is_err() {
                        break;
                    }
                    let path = request_line
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("/")
                        .to_owned();
                    let mut length = 0_usize;
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                            break;
                        }
                        if let Some(value) =
                            header.to_ascii_lowercase().strip_prefix("content-length:")
                        {
                            length = value.trim().parse().unwrap_or(0);
                        }
                    }
                    let mut body = vec![0_u8; length];
                    let _ = reader.read_exact(&mut body);
                    let answer = routes
                        .iter()
                        .find(|(route, _)| path.starts_with(route.as_str()))
                        .map(|(_, answer)| answer.clone())
                        .unwrap_or_else(|| json!({"error": "no fixture"}));
                    let body = answer.to_string();
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                }
            });
            Self {
                base,
                _thread: thread,
            }
        }
    }

    #[test]
    fn outline_becomes_notes_with_their_tree() {
        let fixture = Fixture::serve(vec![
            (
                "/api/collections.list".to_owned(),
                json!({"data": [{"id": "c1", "name": "Handbook", "description": "How we work."}]}),
            ),
            (
                "/api/documents.list".to_owned(),
                json!({"data": [
                    {"id": "d1", "title": "Start here", "text": "Welcome.", "collectionId": "c1"},
                    {"id": "d2", "title": "Expenses", "text": "Claim it.", "collectionId": "c1", "parentDocumentId": "d1"},
                ]}),
            ),
        ]);
        let source = Source::new(&fixture.base, "token".to_owned());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let collections = outline_collections(&source).await.unwrap();
            assert_eq!(collections.len(), 1);
            let documents = outline_documents(&source, "c1").await.unwrap();
            assert_eq!(documents.len(), 2);
            let target = Recording::default();
            let pages = vec![
                json!({"source": "outline", "key": "collection:c1", "title": "Handbook", "markdown": "How we work."}),
                outline_page(&source, &target, &documents[0]).await.unwrap(),
                outline_page(&source, &target, &documents[1]).await.unwrap(),
            ];
            assert_eq!(text(&pages[2], "parent"), "d1");
            write_notes(&target, pages).await.unwrap();
            let calls = target.calls();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].action, "import-pages");
            assert_eq!(calls[0].args["pages"].as_array().unwrap().len(), 3);
        });
    }

    #[test]
    fn linear_issues_keep_their_state_links_and_comments() {
        let fixture = Fixture::serve(vec![(
            "/graphql".to_owned(),
            json!({"data": {"issues": {
                "pageInfo": {"hasNextPage": false, "endCursor": null},
                "nodes": [{
                    "identifier": "ENG-1",
                    "title": "Ship it",
                    "description": "The plan.",
                    "url": "https://issues.example/ENG-1",
                    "createdAt": "2026-01-02T03:04:05.678Z",
                    "state": {"name": "In progress", "type": "started"},
                    "assignee": {"name": "Ada"},
                    "labels": {"nodes": [{"name": "launch"}]},
                    "parent": null,
                    "relations": {"nodes": [{"relatedIssue": {"identifier": "ENG-2"}}]},
                    "comments": {"nodes": [{"body": "On it.", "createdAt": "2026-01-02T04:00:00.000Z", "user": {"name": "Grace"}}]}
                }]
            }}}),
        )]);
        let source = Source::new(&fixture.base, "token".to_owned());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let issues = linear_issues(&source).await.unwrap();
            assert_eq!(issues.len(), 1);
            let issue = &issues[0];
            assert_eq!(text(issue, "status"), "in-progress");
            assert_eq!(text(issue, "assignee"), "person:Ada");
            assert_eq!(issue["labels"], json!(["launch"]));
            assert_eq!(issue["links"], json!(["ENG-2"]));
            assert_eq!(issue["comments"][0]["author"], "Grace");
            assert_eq!(issue["created_ms"], 1_767_323_045_678_i64);
            let target = Recording::default();
            write_issues(&target, issues).await.unwrap();
            assert_eq!(target.calls()[0].action, "import-issues");
        });
    }

    #[test]
    fn forgejo_issues_keep_their_state_comments_and_cross_references() {
        let fixture = Fixture::serve(vec![
            (
                "/api/v1/repos/tilo/tilo/issues/1/comments".to_owned(),
                json!([{"body": "Fixed below.", "created_at": "2026-02-03T04:05:06Z", "user": {"login": "ada"}}]),
            ),
            (
                "/api/v1/repos/tilo/tilo/issues".to_owned(),
                json!([
                    {"number": 1, "title": "Break", "body": "See #2.", "state": "open", "labels": [{"name": "bug"}], "assignee": {"login": "grace"}, "user": {"login": "ada"}, "created_at": "2026-02-03T04:05:06Z", "html_url": "https://code.example/tilo/tilo/issues/1"},
                    {"number": 2, "title": "Fix", "body": "Done in #1.", "state": "closed", "labels": [], "assignee": null, "user": {"login": "grace"}, "created_at": "2026-02-04T04:05:06Z", "html_url": "https://code.example/tilo/tilo/issues/2"}
                ]),
            ),
        ]);
        let source = Source::new(&fixture.base, "token".to_owned());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let raw = forgejo_issues(&source, "tilo", "tilo").await.unwrap();
            let mut issues = Vec::new();
            for issue in &raw {
                issues.push(forgejo_issue(&source, "tilo", "tilo", issue).await.unwrap());
            }
            forgejo_links(&mut issues, "tilo", "tilo");
            assert_eq!(text(&issues[0], "title"), "Break");
            assert_eq!(text(&issues[1], "status"), "done");
            assert_eq!(issues[0]["links"], json!(["tilo/tilo#2"]));
            assert_eq!(issues[1]["links"], json!(["tilo/tilo#1"]));
            assert_eq!(issues[0]["comments"][0]["author"], "ada");
            // The same run written twice follows the source, not the run.
            let target = Recording::default();
            write_issues(&target, issues.clone()).await.unwrap();
            write_issues(&target, issues).await.unwrap();
            assert_eq!(target.calls().len(), 2);
        });
    }

    #[test]
    fn files_keep_their_folders_and_their_names() {
        let found = vec![
            Found {
                path: PathBuf::from("/tmp/contracts/2026/lease.pdf"),
                name: "2026/lease.pdf".to_owned(),
            },
            Found {
                path: PathBuf::from("/tmp/notes.md"),
                name: "notes.md".to_owned(),
            },
        ];
        assert_eq!(folders_of("2026/lease.pdf"), ["2026"]);
        assert_eq!(folders_of("a/b/c.txt"), ["a", "a/b"]);
        assert!(folders_of("notes.md").is_empty());
        assert!(!is_markdown("lease.pdf"));
        assert!(is_markdown("notes.md"));
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn files_keep_their_folders_and_open_in_photos() {
        let dir = tempfile::tempdir().unwrap();
        let contracts = dir.path().join("contracts");
        std::fs::create_dir_all(contracts.join("2026")).unwrap();
        std::fs::write(contracts.join("2026/plan.md"), "# Plan\n\nWords.\n").unwrap();
        let mut png = vec![137, 80, 78, 71, 13, 10, 26, 10];
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&4_u32.to_be_bytes());
        png.extend_from_slice(&3_u32.to_be_bytes());
        png.extend_from_slice(&[0; 5]);
        std::fs::write(contracts.join("scan.png"), &png).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let target = Recording::default();
            let (files, notes) = import_files(&target, &[contracts.clone()]).await.unwrap();
            assert_eq!(files, 2);
            assert_eq!(notes, 4, "two folders and two files");
            let calls = target.calls();
            let photo = calls
                .iter()
                .find(|call| call.action == "import-photo")
                .expect("the picture opens in Photos");
            assert_eq!(photo.args["image"]["width"], 4);
            assert_eq!(photo.args["image"]["name"], "contracts/scan.png");
            let pages = calls
                .iter()
                .find(|call| call.action == "import-pages")
                .expect("the folders and files are notes")
                .args["pages"]
                .clone();
            let pages = pages.as_array().unwrap();
            let by_key = |key: &str| {
                pages
                    .iter()
                    .find(|page| page["key"] == format!("{key}"))
                    .cloned()
                    .unwrap()
            };
            assert_eq!(by_key("folder:contracts")["parent"], Value::Null);
            assert_eq!(
                by_key("folder:contracts/2026")["parent"],
                "folder:contracts"
            );
            assert_eq!(
                by_key("file:contracts/2026/plan.md")["parent"],
                "folder:contracts/2026"
            );
            assert!(
                by_key("file:contracts/2026/plan.md")["markdown"]
                    .as_str()
                    .unwrap()
                    .contains("Words.")
            );
            assert!(
                by_key("file:contracts/scan.png")["markdown"]
                    .as_str()
                    .unwrap()
                    .contains("Opened in")
            );
        });
    }

    #[test]
    fn a_time_is_read_as_milliseconds() {
        assert_eq!(iso_ms("1970-01-01T00:00:00Z"), 0);
        assert_eq!(iso_ms("2026-01-02T03:04:05.678Z"), 1_767_323_045_678);
        assert_eq!(iso_ms("not a time"), 0);
    }

    #[test]
    fn a_picture_states_its_own_size() {
        let mut png = vec![137, 80, 78, 71, 13, 10, 26, 10];
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&4_u32.to_be_bytes());
        png.extend_from_slice(&3_u32.to_be_bytes());
        png.extend_from_slice(&[0; 5]);
        assert_eq!(image_size(&png), Some((4, 3)));
    }

    #[test]
    fn a_mail_account_is_named_by_its_address() {
        let args = MailArgs {
            user: "ops@example.com".to_owned(),
            name: Some("Company".to_owned()),
            imap_host: "imap.example".to_owned(),
            imap_port: None,
            smtp_host: "smtp.example".to_owned(),
            smtp_port: None,
            password_file: None,
            auth: "oauth".to_owned(),
            dry_run: false,
        };
        let account = mail_account(&args).unwrap();
        assert_eq!(text(&account, "id"), "mail-ops-example-com");
        assert_eq!(text(&account, "type_id"), "email");
        assert_eq!(account["config"]["auth"], "oauth");
        assert_eq!(account["config"]["imap_port"], 993);
        assert_eq!(account["config"]["smtp_port"], 465);
    }
}

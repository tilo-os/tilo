//! Every operation of the API, in one table: what routing matches a request
//! against ([`super::routing`]) and what the `OpenAPI` description is written
//! from ([`super::openapi`]). A route exists exactly when it is here.

use hyper::Method;

/// One operation: a method on a path template, and what it takes and answers.
#[derive(Debug)]
pub struct Operation {
    /// The `operationId` of the `OpenAPI` description, and what a handler is chosen by.
    pub id: &'static str,
    pub method: Method,
    /// The path, its parameters in braces. `{path}` is a resource path: one or
    /// more segments, each percent-encoded.
    pub path: &'static str,
    pub summary: &'static str,
    /// The query parameters it reads, each `(name, description)`.
    pub query: &'static [(&'static str, &'static str)],
    /// The JSON body it takes, by schema name ([`super::answers`]), if any.
    pub body: Body,
    /// What it answers on success.
    pub answers: Answer,
    /// Whether it changes something, so it takes an `Idempotency-Key`.
    pub mutating: bool,
}

/// What an operation takes as its body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    None,
    /// JSON of the named schema.
    Json(&'static str),
    /// An action's arguments: the action's own schema.
    Arguments,
    /// A file's bytes.
    Bytes,
}

/// What an operation answers on success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// `status` with JSON of the named schema.
    Json(u16, &'static str),
    /// An action's answer: `200` done, `202` pending.
    Action,
    /// A stream of server-sent events.
    Events,
    /// A file's bytes.
    Bytes,
}

const fn op(
    id: &'static str,
    method: Method,
    path: &'static str,
    summary: &'static str,
) -> Operation {
    Operation {
        id,
        method,
        path,
        summary,
        query: &[],
        body: Body::None,
        answers: Answer::Json(200, "Empty"),
        mutating: false,
    }
}

impl Operation {
    const fn query(mut self, query: &'static [(&'static str, &'static str)]) -> Self {
        self.query = query;
        self
    }
    const fn body(mut self, body: Body) -> Self {
        self.body = body;
        self.mutating = true;
        self
    }
    const fn mutating(mut self) -> Self {
        self.mutating = true;
        self
    }
    const fn answers(mut self, answers: Answer) -> Self {
        self.answers = answers;
        self
    }
    const fn json(self, status: u16, schema: &'static str) -> Self {
        self.answers(Answer::Json(status, schema))
    }
}

const PAGE: &[(&str, &str)] = &[
    (
        "path",
        "What to list: a resource path, or nothing for the apps",
    ),
    ("cursor", "Where the previous page ended"),
    ("limit", "How many rows at most"),
    ("kind", "`resources` or `actions` only"),
];

/// The API, in the order the description lists it.
pub static OPERATIONS: std::sync::LazyLock<Vec<Operation>> = std::sync::LazyLock::new(|| {
    vec![
        op(
            "getApi",
            Method::GET,
            "/v1",
            "The API's version, and what the key reaches",
        )
        .json(200, "ApiInfo"),
        op(
            "getAccount",
            Method::GET,
            "/v1/account",
            "The person's plan and what each workspace keeps",
        )
        .json(200, "Account"),
        op(
            "listWorkspaces",
            Method::GET,
            "/v1/workspaces",
            "Every workspace the key reaches, wherever it is hosted",
        )
        .json(200, "WorkspaceList"),
        op(
            "createWorkspace",
            Method::POST,
            "/v1/workspaces",
            "Make a workspace of the person's own",
        )
        .body(Body::Json("NewWorkspace"))
        .json(201, "Workspace"),
        op(
            "deleteWorkspace",
            Method::DELETE,
            "/v1/workspaces/{workspace}",
            "Delete a workspace of the person's own, with everything in it: final",
        )
        .mutating()
        .json(200, "DeletedWorkspace"),
        op(
            "listKeys",
            Method::GET,
            "/v1/keys",
            "This key and the keys it minted",
        )
        .json(200, "KeyList"),
        op(
            "mintKey",
            Method::POST,
            "/v1/keys",
            "Mint a key reaching one workspace, for an agent there",
        )
        .body(Body::Json("NewKey"))
        .json(201, "MintedKey"),
        op(
            "revokeKey",
            Method::DELETE,
            "/v1/keys/{grant}",
            "Revoke this key or one it minted, and every key it minted",
        )
        .mutating()
        .json(200, "RevokedKey"),
        op(
            "listResources",
            Method::GET,
            "/v1/workspaces/{workspace}/resources",
            "What is under a path: its rows and actions",
        )
        .query(PAGE)
        .json(200, "Listing"),
        op(
            "readResource",
            Method::GET,
            "/v1/workspaces/{workspace}/resources/{path}",
            "One resource: its fields and what it holds",
        )
        .query(&[("cursor", "Where the previous page ended")])
        .json(200, "Resource"),
        op(
            "runAction",
            Method::POST,
            "/v1/workspaces/{workspace}/resources/{path}/actions/{action}",
            "Run an action on a resource, its arguments as the body",
        )
        .body(Body::Arguments)
        .answers(Answer::Action),
        op(
            "getRun",
            Method::GET,
            "/v1/workspaces/{workspace}/runs/{run}",
            "A run, held until it is no longer pending for `wait` seconds",
        )
        .query(&[("wait", "Seconds to hold while it is pending, at most 60")])
        .answers(Answer::Action),
        op(
            "cancelRun",
            Method::POST,
            "/v1/workspaces/{workspace}/runs/{run}/cancel",
            "Stop a run",
        )
        .mutating()
        .answers(Answer::Action),
        op(
            "undoRun",
            Method::POST,
            "/v1/workspaces/{workspace}/runs/{run}/undo",
            "Take back what a run changed",
        )
        .mutating()
        .json(200, "ActionAnswer"),
        op(
            "runOutput",
            Method::GET,
            "/v1/workspaces/{workspace}/runs/{run}/output",
            "A run's events, ending when it ends",
        )
        .answers(Answer::Events),
        op(
            "search",
            Method::GET,
            "/v1/workspaces/{workspace}/search",
            "Search the workspace by words, best first",
        )
        .query(&[
            ("q", "The words"),
            ("app", "One app's resources only"),
            ("limit", "How many hits at most"),
        ])
        .json(200, "SearchHits"),
        op(
            "getLink",
            Method::GET,
            "/v1/workspaces/{workspace}/links",
            "The link a person opens what a path names with",
        )
        .query(&[
            ("path", "The resource"),
            ("mode", "`app` (the default) or `desktop`"),
        ])
        .json(200, "Link"),
        op(
            "listMembers",
            Method::GET,
            "/v1/workspaces/{workspace}/members",
            "The people in the workspace, and the invitations still open",
        )
        .json(200, "Members"),
        op(
            "invitePerson",
            Method::POST,
            "/v1/workspaces/{workspace}/members/invitations",
            "A link that brings one more person in",
        )
        .body(Body::Json("NewInvitation"))
        .json(201, "Invitation"),
        op(
            "runProgram",
            Method::POST,
            "/v1/workspaces/{workspace}/programs",
            "Run a tilolang program as one step",
        )
        .body(Body::Json("Program"))
        .json(200, "ProgramAnswer"),
        op(
            "upload",
            Method::POST,
            "/v1/workspaces/{workspace}/uploads",
            "Put a file's bytes into the workspace, for an action to take",
        )
        .body(Body::Bytes)
        .json(201, "Uploaded"),
        op(
            "getContent",
            Method::GET,
            "/v1/workspaces/{workspace}/content/{content}",
            "The bytes an action made",
        )
        .answers(Answer::Bytes),
        op(
            "events",
            Method::GET,
            "/v1/workspaces/{workspace}/events",
            "What happens in the workspace, as it happens",
        )
        .query(&[("path", "One resource's changes, and what is under it")])
        .answers(Answer::Events),
        op(
            "listWebhooks",
            Method::GET,
            "/v1/workspaces/{workspace}/webhooks",
            "The workspace's webhooks, without their secrets",
        )
        .json(200, "WebhookList"),
        op(
            "createWebhook",
            Method::POST,
            "/v1/workspaces/{workspace}/webhooks",
            "Post the workspace's events to a URL, signed",
        )
        .body(Body::Json("NewWebhook"))
        .json(201, "Webhook"),
        op(
            "deleteWebhook",
            Method::DELETE,
            "/v1/workspaces/{workspace}/webhooks/{webhook}",
            "Stop posting to a webhook",
        )
        .mutating()
        .json(200, "DeletedWebhook"),
    ]
});

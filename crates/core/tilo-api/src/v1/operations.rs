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
    /// A content put: where an unfinished one stands (`200`), or what the
    /// bytes are once every one is there (`201`).
    Content,
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
            "listAccountResources",
            Method::GET,
            "/v1/resources",
            "What the key's account holds under a path (`account/workspaces`, `account/keys`): its rows and actions",
        )
        .query(PAGE)
        .json(200, "Listing"),
        op(
            "readAccountResource",
            Method::GET,
            "/v1/resources/{path}",
            "One resource of the key's account: `account/workspaces` or `account/usage`",
        )
        .query(&[("cursor", "Where the previous page ended")])
        .json(200, "Resource"),
        op(
            "runAccountAction",
            Method::POST,
            "/v1/resources/{path}/actions/{action}",
            "Run an action of the key's account (`account/workspaces#create`, `account/keys#mint`), its arguments as the body",
        )
        .body(Body::Arguments)
        .answers(Answer::Action),
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
            "viewResource",
            Method::GET,
            "/v1/workspaces/{workspace}/views",
            "What an app's window shows for a resource: its view as lines, and a picture with `picture`",
        )
        .query(&[
            ("path", "The resource whose view to read"),
            ("picture", "`true` to answer a picture too, kept as content"),
            ("scale", "Pixels per point of that picture, 0.5 to 2"),
        ])
        .json(200, "View"),
        op(
            "runAction",
            Method::POST,
            "/v1/workspaces/{workspace}/resources/{path}/actions/{action}",
            "Run an action on a resource, its arguments as the body",
        )
        .body(Body::Arguments)
        .answers(Answer::Action),
        op(
            "getInvocation",
            Method::GET,
            "/v1/workspaces/{workspace}/invocations/{invocation}",
            "An invocation, answered once it is no longer pending or `wait` seconds passed",
        )
        .query(&[("wait", "Seconds to wait while it is pending, at most 60")])
        .answers(Answer::Action),
        op(
            "cancelInvocation",
            Method::POST,
            "/v1/workspaces/{workspace}/invocations/{invocation}/cancel",
            "Stop an invocation",
        )
        .mutating()
        .answers(Answer::Action),
        op(
            "undoInvocation",
            Method::POST,
            "/v1/workspaces/{workspace}/invocations/{invocation}/undo",
            "Take back what an invocation changed",
        )
        .mutating()
        .json(200, "ActionAnswer"),
        op(
            "invocationOutput",
            Method::GET,
            "/v1/workspaces/{workspace}/invocations/{invocation}/output",
            "An invocation's events, ending when it ends",
        )
        .answers(Answer::Events),
        op(
            "askModel",
            Method::POST,
            "/v1/workspaces/{workspace}/model/ask",
            "Ask the person's own model one turn, its answer streamed as the model writes it",
        )
        .body(Body::Json("ModelAsk"))
        .answers(Answer::Events),
        op(
            "runProgram",
            Method::POST,
            "/v1/workspaces/{workspace}/programs",
            "Run a tilolang program as one step",
        )
        .body(Body::Json("Program"))
        .json(200, "ProgramAnswer"),
        op(
            "putContent",
            Method::PUT,
            "/v1/workspaces/{workspace}/contents/{hash}",
            "Put a file's bytes into the workspace, addressed by their hash; `Content-Range` continues one too large for one request",
        )
        .body(Body::Bytes)
        .answers(Answer::Content),
        op(
            "getContent",
            Method::GET,
            "/v1/workspaces/{workspace}/contents/{hash}",
            "The bytes at a content address, with `Range`",
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
    ]
});

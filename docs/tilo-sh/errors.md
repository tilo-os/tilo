# Errors

Every call either answers or raises the API's one refusal, `Refused`. Both SDKs raise it with the same
fields, and the command exits non-zero and prints the same words.

## What a call can raise

| What happened | Python | TypeScript | Carries |
| :--- | :--- | :--- | :--- |
| The API refused the call | `Refused` | `Refused` | `code`, `error` (the whole body), `status` |
| The key was turned away | `Refused` | `Refused` | `code` is `key_revoked`, `key_expired` or `invalid_token`, and `status` is 401 |
| The app's surface moved past the generated types | `Refused` | `Refused` | `code` is `surface_changed`; run `tilo sdk gen` |
| The daemon or the server could not be reached | `ConnectionError` (`httpx.HTTPError` for a key) | the addon's `Error` (or `fetch`'s rejection) | the transport's own words |

An action that waits for a person is no error: the answer is `pending`, with the `invocation` it runs
under and the `ask` they answer. `run` follows it until they decide. [Approvals](approvals.md) is the
whole story.

## Refused

The API refused the call, and says why in the app's own words. `code` is stable and typed, what a program
branches on; `message` is what the app would tell a person; `next_steps` are the calls that would work
instead, each a sentence naming a path. `error` is the whole refusal body, so the fields a refusal of one
kind carries are read from it (`key_revoked`'s `revoked_at_ms`, a wait's `retry_after_ms`, the refused
action's row). `status` is the HTTP status it came with, 0 for a refusal of the daemon's own connection.

| `code` | Where it comes from |
| :--- | :--- |
| `invalid_arguments` | A parameter is missing or the wrong type; `next_steps` names the action's parameters |
| `invalid_request` | The request itself is not one the API takes |
| `unknown_action` | The path names no action of that resource, or one of an app the caller cannot list |
| `not_offered` | The app has the action, but it is not offered to this caller here; `message` names the action and the caller |
| `not_found` | No such workspace, resource or key, or one that is not yours |
| `beyond_scope` | The key reaches one workspace and this is another one |
| `not_permitted` | The key reaches this workspace but may not do this in it, or asks the account for more than the keys its workspace holds |
| `no_payment_method` | The account's plan does not include it yet; `next_steps` says what adds it |
| `unknown_app` | The path names an app the workspace does not have; `message` lists the ones it has |
| `action_failed` | The action ran and refused: `message` is what the app tells a person, and nothing else ran |
| `surface_changed` | The generated types were made for another surface of the app; `tilo sdk gen` writes them again |
| `unavailable` | The host could not run it just now, saying why: try again |
| `upload_failed` | A content put's bytes are not the ones its address names, or a piece did not land |
| `offset_mismatch` | A content put's piece came from where the put does not stand; `next_steps` says where it is |
| `stale_revision` | The resource changed since the revision the call named: list it again and invoke what is offered now |
| `key_revoked` | The key was revoked, with everything minted from it, or its workspace was deleted; `revoked_at_ms` says when, and `status` is 401 |
| `key_expired` | The key is past its expiry; `expired_at_ms` says when, and `status` is 401 |
| `invalid_token` | The host never issued this key: mistyped, or from another host; `status` is 401 |

Nothing ran when a refusal is raised, unless the app says otherwise in its steps.

The refusal's `message` is what a person would be told and never repeats the code. A tool answer's text
starts with the code (`unknown_action: …`), so a reader of the text can tell it too.

## Reading them

- **Python**: `error.code`, `error.message`, `error.next_steps`, `error.status`; `error.error` is the
  whole body. `str(error)` prints the code, the message and the steps on their own lines.
- **TypeScript**: `error.code`, `error.status`; `error.error.code`, `error.error.message`,
  `error.error.next_steps` and the rest of the body.
- **The command**: prints the host's words to standard error and exits 1. A key the host does not take
  says so in one line — it expired or was revoked — and names the command that asks for a new one.

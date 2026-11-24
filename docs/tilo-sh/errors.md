# Errors

Every call either answers or raises one of these. Both SDKs raise the same set, with the same fields, and
the command exits non-zero and prints the same words.

## What a call can raise

| What happened | Python | TypeScript | Carries |
| :--- | :--- | :--- | :--- |
| The host refused the action | `Refused` | `Refused` | `code`, `message`, `next_steps` (`nextSteps` in TypeScript) |
| The action waits for a person | `AskPending` | `AskPending` | `url`, `invocation_id` (`invocationId`), `question` |
| The key was turned away | `KeyRefused` | `KeyRefused` | `code`, `status` 401, `next_steps` |
| …because it was revoked | `KeyRevoked` | `KeyRevoked` | `revoked_at_ms` (`revokedAtMs`) |
| …because it expired | `KeyExpired` | `KeyExpired` | `expired_at_ms` (`expiredAtMs`) |
| The call itself was malformed, or the answer was not the typed one | `ProtocolError` | `ProtocolError` | `message`, `status`, `code` |
| The app's surface moved past the generated types | `SurfaceChanged` | `SurfaceChanged` | `code` is `surface_changed`; run `tilo sdk gen` |
| The daemon or the server could not be reached, or its answer was cut short | `Unreachable` | `Unreachable` | `message` |

`KeyRevoked` and `KeyExpired` are `KeyRefused`s, and `KeyRefused` is a
`ProtocolError` with HTTP status 401. So catching the parent catches its cases. `SurfaceChanged` is a
`Refused`; `Unreachable` is a `ProtocolError`.

## Refused

The host refused the action, and says why in the app's own words. `code` is the app's code, `message` is
what the app would tell a person, and `next_steps` are the calls that would work instead, each a sentence
naming a path.

| `code` | Where it comes from |
| :--- | :--- |
| `invalid_arguments` | A parameter is missing or the wrong type; `next_steps` names the action's parameters |
| `unknown_action` | The path names no action of that resource, or one of an app the caller cannot list |
| `not_offered` | The app has the action, but it is not offered to this caller here; `message` names the action and the caller |
| `not_found` | No such workspace, resource or key, or one that is not yours |
| `beyond_scope` | The key reaches one workspace and this is another one |
| `not_permitted` | The key reaches this workspace but may not do this in it, or asks the account for more than the keys its workspace holds |
| `no_payment_method` | The account's plan does not include it yet; `next_steps` says what adds it |
| `unknown_app` | The path names an app the workspace does not have; `message` lists the ones it has |
| `action_failed` | The action ran and refused: `message` is what the app tells a person, and nothing else ran |
| `unavailable` | The host could not run it just now, saying why: try again |
| `stale_revision` | The resource changed since the revision the call named: list it again and invoke what is offered now |

Nothing ran when a refusal is raised, unless the app says otherwise in its steps.

The refusal's `message` is what a person would be told and never repeats the code. A tool answer's text
starts with the code (`unknown_action: …`), so a reader of the text can tell it too.

## AskPending

The action asked the person first and has not run. `invocation_id` is the run the ask belongs to; `url` is
where the person answers it when the API says (a hosted workspace asks them on their own device instead);
`question` is what they are being asked. `ask.wait()` follows the run until they decide, and answers its
result. [Approvals](approvals.md) is the whole story.

## A key that is turned away

| Raised | `code` | Meaning |
| :--- | :--- | :--- |
| `KeyRevoked` | `key_revoked` | The key was revoked, with everything minted from it, or its workspace was deleted. `revoked_at_ms` says when |
| `KeyExpired` | `key_expired` | The key is past its expiry. `expired_at_ms` says when |
| `KeyRefused` | `invalid_token` | The host never issued this key: mistyped, or from another host |

A request with no key at all raises `ProtocolError` saying to set `TILO_API_KEY`.

## SurfaceChanged

A call made through generated types names the app surface those types were written for, and a workspace
whose app publishes another one refuses it rather than running it. `message` names both, and
`tilo sdk gen` writes the types again.

## ProtocolError

The call was malformed, or the answer was not what the API promises: a host that answered text instead of a
typed value, an event that is not JSON, an upload without a reference. It is the SDK's own error, not the
host's: if you see it, something on the way is not a Tilo host, or is not the version it says.

## Reading them

- **Python**: `error.code`, `error.message`, `error.next_steps`; `str(error)` prints the code, the message
  and the steps on their own lines.
- **TypeScript**: `error.code`, `error.message`, `error.nextSteps`, `error.status`.
- **The command**: prints the host's words to standard error and exits 1. A key the host does not take says
  so in one line — it expired or was revoked — and names the command that asks for a new one.

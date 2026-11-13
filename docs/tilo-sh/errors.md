# Errors

Every call either answers or raises one of these. Both SDKs raise the same set, with the same fields, and
the command exits non-zero and prints the same words.

## What a call can raise

| What happened | Python | TypeScript | Carries |
| :--- | :--- | :--- | :--- |
| The host refused the action | `Refused` | `Refused` | `code`, `message`, `next_steps` (`nextSteps` in TypeScript) |
| The host is busy with the person's changes | `RateLimited` | `RateLimited` | `code` is `rate_limited`, `retry_after_ms` (`retryAfterMs`) |
| The action waits for a person | `AskPending` | `AskPending` | `url`, `invocation_id` (`invocationId`), `question` |
| The key was turned away | `KeyRefused` | `KeyRefused` | `code`, `status` 401, `next_steps` |
| …because it was revoked | `KeyRevoked` | `KeyRevoked` | `revoked_at_ms` (`revokedAtMs`) |
| …because it expired | `KeyExpired` | `KeyExpired` | `expired_at_ms` (`expiredAtMs`) |
| The call itself was malformed, or the answer was not the typed one | `ProtocolError` | `ProtocolError` | `message`, `status`, `code` |
| The app's surface moved past the generated types | `SurfaceChanged` | `SurfaceChanged` | `code` is `surface_changed`; run `tilo sdk gen` |
| The server speaks another major version of the API | `UnsupportedVersion` | `ProtocolError` | `version`, and the one this SDK speaks (`api_version_unsupported`) |

`RateLimited` is a `Refused`; `KeyRevoked` and `KeyExpired` are `KeyRefused`s, and `KeyRefused` is a
`ProtocolError` with HTTP status 401. So catching the parent catches its cases. `SurfaceChanged` is a
`Refused` and `UnsupportedVersion` is a `ProtocolError`.

## Refused

The host refused the action, and says why in the app's own words. `code` is the app's code, `message` is
what the app would tell a person, and `next_steps` are the calls that would work instead, each a sentence
naming a path.

| `code` | Where it comes from |
| :--- | :--- |
| `invalid_arguments` | A parameter is missing or the wrong type; `next_steps` names the action's parameters |
| `unknown_action` | The path names no action of that resource |
| `not_found` | No such workspace, resource or key, or one that is not yours |
| `beyond_scope` | The key reaches one workspace and this is another one, or an account call |
| `not_permitted` | The key reaches this workspace but may not do this in it |
| `rate_limited` | The host is busy with the person's changes; nothing ran |
| `no_payment_method` | The account's plan does not include it yet; `next_steps` says what adds it |

Nothing ran when a refusal is raised, unless the app says otherwise in its steps.

## AskPending

The action asked the person first and has not run. `invocation_id` is the run the ask belongs to; `url` is
where the person answers it when the API says (a hosted workspace asks them on their own device instead);
`question` is what they are being asked. `ask.wait()` follows the run until they decide, and answers its
result. [Approvals](approvals.md) is the whole story.

## RateLimited

The host is running the person's own changes and ran nothing. The SDKs retry by themselves: they wait what
the host says, with backoff, until their retry deadline — 60 seconds by default,
`retry_deadline`/`retryDeadlineMs` on the client. Only when that passes is `RateLimited` raised, so
catching it means "still busy after a minute".

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

## An API of another version

Every answer names the API's major version (`Tilo-Api-Version`), and a server on another major than the SDK
speaks is refused before anything else is read: Python raises `UnsupportedVersion`, and TypeScript a
`ProtocolError` whose code is `api_version_unsupported`.

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

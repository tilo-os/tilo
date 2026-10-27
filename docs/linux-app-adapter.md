# Wrapping a Linux app's interfaces as typed actions

A Flatpak app, or a command-line tool, has no Tilo resource tree — so
Shortcuts, the palette, search and an agent cannot reach it at all. An
**adapter** declares that app's actions anyway: it names the app, the actions
it could perform, and, for each, the ways of performing it, ordered most
precise first. Everything in an adapter is inert data, serialised like any
other resource, so it can be written by the app's author, by Tilo, by a
person, or by a task that recorded what it did, and shared and reviewed like
the rest of a workspace.

`tilo-actions` carries the declaration (`tilo_actions::adapter`) and the step
from a declaration to work (`tilo_actions::adapter_run`). This page is about
the Linux half: what a D-Bus method and a command line become, and where the
sandbox draws the line.

## What a declaration holds

An adapter names its app (`org.gnome.Loupe`, `imagemagick`) and the platform
whose conventions its steps follow (`linux`). Each action declares:

* an `id` and the label a person reads;
* its typed parameters, in the same vocabulary an app declares its own;
* its `contract` — whether it reads, changes only a resource, or reaches
  outside it — which is what the grants deciding it are drawn from;
* what **Undo** means: `native` (the app takes it back), `restore` (Tilo
  snapshots the affected resources first, and Undo restores them), or `none`
  (nothing takes it back, so the action is marked irreversible and has to be
  approved before it runs);
* one or more **implementations**, and every implementation carries at least
  one **check** — what must be true once it has run.

The layers, most precise first: an interface the app already exposes
(`interface`), the app's own file formats understood as typed resources
(`files`), the accessibility tree (`accessibility`), and finally pixels.
Layers after `interface` belong to the UI-driving path; this page is the
`interface` layer on Linux.

```json
{
  "app": "org.gnome.Loupe",
  "app_version": "any",
  "platform": "linux",
  "actions": [
    {
      "id": "open-image",
      "label": "Open an image in Loupe",
      "params": [
        { "name": "path", "ty": "text", "required": true,
          "description": "The image to show" }
      ],
      "contract": { "kind": "Read" },
      "undo": "restore",
      "implementations": [
        {
          "layer": "interface",
          "steps": [{
            "step": "run",
            "program": "loupe",
            "args": ["--present", "{{param.path}}"]
          }],
          "checks": [{
            "kind": "command",
            "program": "loupe"
          }]
        }
      ]
    }
  ]
}
```

## What a step becomes

`AdapterAction::spec` turns each action into an ordinary `ActionSpec`, so it
joins a palette, a search provider and a Shortcut by the same door an app's
own actions do. `Plan::of` — or `Plan::lowering`, when the invocation is
already decoded — reads the declaration, puts the bound arguments in place,
plans the first layer reachable here, and hands back the work:

* a **`run`** step is a program and an argument vector. `{{param.NAME}}`
  substitutes one argument's *value*; nothing is split on whitespace and
  nothing is expanded by a shell, so a path `a; rm -rf ~/notes` is one
  argument.
* a **`call`** step is a bus name, an object path, an interface and a method,
  each checked against the grammar of the kind of name it claims to be,
  plus arguments. There is no spelling of a parameter that reaches a
  different object, and no method name that carries a flag.

A required parameter the caller left out refuses the plan before any work is
built, naming it, exactly as the bind of a typed action already does. A
declaration whose bus parts are not names is refused the same way rather than
handed to a bus client. Steps of the other layers — a resource edit, an
accessibility invoke, a click — are not performed by this lowering, and say
so.

## Where the sandbox draws the line

A Tilo app runs in a sandbox whose session bus is a filtered proxy: it reaches
the portals, notifications and media players its manifest declares, and
nothing else (`launcher::dbus_proxy`). The question "can this step run here"
is answered by that filter, not by this crate. So the action runs **inside
the app's sandbox scope**, and a step it cannot reach is refused and reported
— it is not retried somewhere with wider access. That refusal is the reason
the check exists: a layer that cannot say whether it worked cannot be trusted
ahead of the layer below it.

## Undo

`restore` is the answer for a legacy app: Tilo snapshots the resources the
action touches before it runs, and Undo restores them, so a change made
through the app appears in Tilo's history beside every other change. Where
nothing can be snapshotted, `none` marks the action irreversible and the
ordinary confirmation gate applies.

## Reaching it from a Shortcut

An adapter's action is a step like any other:

```json
{
  "id": "open",
  "name": "Open the shot just exported",
  "inputs": [],
  "steps": [
    {
      "id": "pick",
      "step": "invoke",
      "action": {
        "app": "org.freedesktop.portal.Desktop",
        "kind": "portal",
        "action": "pick-colour"
      },
      "resource": { "first_of_kind": "portal" },
      "args": { "handle": { "literal": "h-1" } }
    }
  ]
}
```

Running it goes through the same door every other action does: the arguments
bind against the declared parameters, the adapter plans its reachable layer,
and the work is performed as the person running the shortcut, within the
Shortcut's grant.

## Writing one

Introspectable D-Bus interfaces (`gdbus introspect --xml`) and a tool's
`--help` and man page are the inputs an adapter can be generated from; a
person reviews the result before it is shared. Check it before publishing
with `ActionAdapter::check`, which names what cannot work — an action with no
implementation, an implementation with no check, a contract that contradicts
its Undo, a parameter declaration nothing can bind — and the reader checks it
again on arrival.

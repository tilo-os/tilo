# Personal and workspace settings

Tilo does not ship two competing Settings applications. The ordinary Settings
app owns device and personal preferences. The shell owns contextual Workspace
Settings because it already knows the active workspace, authenticated local
identity, and role grants. Both surfaces call shared domain services.

Workspace Settings is presented as a docked shell lens, not another app. The
workspace stays visible beside it and the lens names both the workspace and the
person's effective role. Personal Settings and global search can request one of
four typed destinations (General, Members, Privacy & Security, Apps & Files),
but cannot supply an executable or arbitrary workspace. A stale link is refused
if its launch workspace is no longer active, preventing an invisible context
switch before a policy change.

## Default applications

`desktop-policy` is the only resolver. A resource is namespaced (`ext:pdf`,
`mime:application/pdf`, or `scheme:https`) and an application is selectable only
when an installed manifest claims that resource.

Resolution order is:

1. An explicit per-person choice in this workspace.
2. If that choice says “use my personal default”, the personal default.
3. Otherwise, an administrator's workspace application.
4. The personal default when workspace policy says “follow personal defaults”.
5. A stable alphabetical fallback among installed claiming applications.

The absent per-person workspace choice is “use workspace choice”. On a new
workspace the administrator choice is “follow personal defaults”, so both
layers naturally resolve to the user's personal selection without storing
redundant values.

Personal selections are atomically persisted under the user's XDG config root.
Workspace policy is part of the serialized workspace. Mutation is not an
untrusted settings action: `Workspace::set_default_app_policy_as` calls the same
owner/delegated-admin authority check as permission grants, and
`set_default_app_choice_as` permits a person to change only their own choice.
The launcher revalidates that the resolved package is installed and still
claims the resource immediately before launch. A removed app safely falls
through instead of breaking file opening.

The personal Settings pane therefore contains one ordinary chooser per file
type. Workspace Settings contains two explicitly labelled columns:
“Workspace choice” and “Your choice”. It does not repeat the personal picker.

## Collaboration and authority

Workspace Settings never constructs a shareable URL from a stable workspace
identifier. Its General pane opens the signed invite composer, where a
governing administrator chooses a recipient role and receives a one-hour token
binding the room, inviter address, role, expiry, and device identity. A guest
copy shows this control disabled because a locally generated token would not be
authoritative.
Each redemption is correlated with that exact signed token, so several pending
invitations retain their own recipient labels and roles regardless of arrival
order.

The same rule applies to every collaborative setting: the local tree must be
the room's pinned governance authority and the acting person must hold live
administration authority. Member role controls offer Viewer, Commenter, and
Editor only. They cannot create or transfer structural ownership, edit the
acting administrator's own membership, or remove the owner. A role change or
removal proves authority once and applies atomically, so revoking one of a
delegated administrator's capabilities cannot strand a half-applied edit.

## Storage

The first storage overview uses existing runtime boundaries rather than trying
to infer ownership from arbitrary home-directory paths:

- signed application payloads from the tapp payload store;
- app-owned data from the per-room runtime directories;
- application caches from the Tilo cache root;
- sync-replica bytes from the sync filesystem root.

Until the sync filesystem publishes hydrated/evictable state, synced bytes are
reported only as “Synced files”; they are never guessed to be reclaimable.
Likewise app data is not called cache merely because an app could recreate it.

Tree measurement uses `symlink_metadata` and never follows symbolic links.
Cache cleanup has a two-part API: preview exact direct children and sizes, then
apply selected preview indices. Apply canonicalizes the root and rechecks each
target's device, inode, mtime, type, and direct-parent relationship. A target
replaced between preview and confirmation is rejected. Installed packages, app
data, and synced files cannot enter this cleanup path.

Settings links to Disks and Backups with typed shell intents. Those intents can
name a known tool and route, but never an executable, package id, or argv. Disk
formatting, filesystem repair, backup retention, and restore therefore remain
in specialist workspaces without duplicating their implementation in Settings.

## Remaining integration work

The manifest reader currently exposes extension claims to the desktop. The
policy model already supports MIME types and URI schemes, but browser/mail
defaults will appear only after the canonical package manifest and installed-app
enumerator expose those claims. Settings already offers conservative “free
local space” actions through the namespace hydration authority. The remaining
storage work is to replace its daemon-lifetime, single-source provenance with
the record plane's existing signed exact-version replica receipts, durable
availability projection and crash-safe eviction queue, removing the two
parallel eviction models.

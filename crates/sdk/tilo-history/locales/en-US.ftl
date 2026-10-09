## Sentences for history entries an app has no message of its own for.
## Arguments: $item (the item's label at the time), $field (the field's
## label), $old, $new, $count, $action (the action's label), $what (a
## sentence for an undone or redone step), $name, $actor, $for.

history-item-unnamed = an item

history-edit-set = Changed { $field } of { $item } to { $new }
history-edit-set-unnamed = Changed { $field } to { $new }
history-edit-on = Turned on { $field } for { $item }
history-edit-on-unnamed = Turned on { $field }
history-edit-off = Turned off { $field } for { $item }
history-edit-off-unnamed = Turned off { $field }
history-edit-cleared = Cleared { $field } of { $item }
history-edit-cleared-unnamed = Cleared { $field }
history-edit-text = Edited { $field } of { $item }
history-edit-text-unnamed = Edited { $field }
history-edit-add = Changed { $field } of { $item } by { $new }
history-edit-add-unnamed = Changed { $field } by { $new }
history-edit-insert = Added to { $field } of { $item }
history-edit-insert-unnamed = Added to { $field }
history-edit-remove = Removed from { $field } of { $item }
history-edit-remove-unnamed = Removed from { $field }
history-edit-move = Reordered { $field } of { $item }
history-edit-move-unnamed = Reordered { $field }
history-edit-format = Formatted { $field } of { $item }
history-edit-format-unnamed = Formatted { $field }
history-edit-settle = Carried out { $item } from { $field }
history-edit-settle-unnamed = Carried out an entry of { $field }
history-edit-settle-failed = { $item } from { $field } failed: { $reason }
history-edit-settle-failed-unnamed = An entry of { $field } failed: { $reason }
history-edit-forget = Cleared { $item } from { $field }
history-edit-forget-unnamed = Cleared an entry of { $field }
history-edit = Changed { $field } of { $item }
history-edit-unnamed = Changed { $field }

history-verb = { $action }: { $item }
history-verb-unnamed = { $action }
history-verb-in = { $action } in { $item }

history-group = { $count ->
    [one] One change
   *[other] { $count } changes
}
history-group-to = { $count ->
    [one] One change to { $item }
   *[other] { $count } changes to { $item }
}
history-group-same = { $what } ({ $count }×)

history-undo = Undid “{ $what }”
history-undo-steps = { $count ->
    [one] Undid one step
   *[other] Undid { $count } steps
}
history-redo = Redid “{ $what }”
history-redo-steps = { $count ->
    [one] Redid one step
   *[other] Redid { $count } steps
}
history-undo-some = Undid earlier steps
history-redo-some = Redid earlier steps
history-restore = Restored an earlier version
history-migrate = Updated to storage version { $to }
history-checkpoint = Saved checkpoint “{ $name }”

history-changed = Changed { $item }
history-changed-unnamed = Changed the document

history-value-true = on
history-value-false = off
history-value-empty = nothing

## Who did it.
history-actor-for = { $actor }, for { $for }
history-actor-you = You
history-actor-you-for = You, for { $for }
history-actor-unknown = Someone
history-actor-agent = { $actor } · Agent
history-actor-shortcut = { $actor } · Shortcut
history-actor-app = { $actor } · App
history-actor-for-you = you

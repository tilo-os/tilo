## Palette rows for launchable applications.
palette-open-app = Open { $app }

## Second line for such a row: where the window will land, then what the
## publisher declared about the app.
palette-app-subtitle = { $hint } · { $detail }

## An app this machine does not have. The verb says what activating the
## row will actually do, because fetching it first is not the same act
## as opening one that is already here.
palette-fetch-and-open-app = Fetch and open { $app }

## Trailing chip on such a row. States the fact, not an instruction.
palette-not-on-this-machine = not on this machine

## Second line for a row whose app has to come over the network: the
## same fields an installed row shows, plus which registry offers it.
palette-app-subtitle-remote = { $hint } · { $detail } · from { $registry }

## The same, for an app whose publisher declared no category: there is
## nothing to put between the landing hint and the registry.
palette-app-subtitle-remote-plain = { $hint } · from { $registry }

## The standing indication raised while an app is being fetched and
## started. Retired when its window appears, or replaced by why it
## could not be.
notice-opening-app = Opening { $app }
notice-fetch-connecting = Contacting { $registry }…
notice-fetch-downloading = Downloading — { $done } of { $total }
notice-fetch-downloading-unsized = Downloading — { $done }
notice-fetch-unpacking = Checking and unpacking…
notice-fetch-starting = Starting…
notice-open-failed = Couldn't open { $app }

## The word beside a collaborator who is not a person. Lower-case: it
## is a tag next to a name, not a sentence, and it sits beside the
## role tag on the same rows.
roster-kind-agent = agent

## The one question first boot asks about the person rather than about
## the key. Wording lifted verbatim from the identity form in
## `ui::widget::identity_flow::create_identity`, which asked exactly this
## and never shipped — so the two surfaces say the same thing rather than
## inventing a second phrasing for one question.
onboarding-name-label = Display name
onboarding-name-placeholder = Your name…
## Why it is being asked, in the terms the rest of the screen uses:
## collaborators, and what they see. Not a warning, and not optional-ness
## theatre — the field opens filled in.
onboarding-name-hint = This is what collaborators see beside your changes. You can change it later.

## The workspace-invite card. The composer asks one question, and the
## field's own hint carries what the name is for — the card used to
## spell that out in a sentence underneath, which is one more line to
## read and says nothing the field cannot.
invite-composer-title = Invite someone
invite-composer-label = Who are you inviting?
invite-composer-placeholder = Their name in this room
invite-composer-cancel = Cancel
invite-composer-create = Create invite
invite-composer-empty = Say who this invite is for

# Shown when a remark cannot be delivered. Chat has no private
# conversation, so a remark addressed to one person alone has nowhere to
# go — and putting it in the channel would disclose what the author
# marked private. It is refused, and this is how they are told.
remark-not-sent-title = Not sent
remark-not-sent-body = A message to one person needs a private conversation, and this room has only channels. Nothing was sent.

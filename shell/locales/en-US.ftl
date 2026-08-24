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

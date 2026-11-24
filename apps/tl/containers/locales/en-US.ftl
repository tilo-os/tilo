# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-containers = Containers

d-so-tilo-containers-keywords = containers, podman, docker, images, volumes, distrobox, container, virtualisierung

d-u3plltps = Draft
    .doc = The new-container form: what the person typed, one text per field, edited together by everyone looking at it.
    .noun = { $count ->
        [one] draft
       *[other] drafts
    }

d-mnoqvwjm = Name

d-kc5y7sq2 = Image

d-gkz666im = Ports

d-7khtp7xl = Env

d-utqr7b5l = Volumes

d-3jkgyx2g = Command

d-uk5fnglc = Pull
    .doc = An image being pulled, and what the engine wrote while it did: the person's own, on their device, so it never reaches anyone else at the workspace.
    .noun = { $count ->
        [one] pull
       *[other] pulls
    }

d-shb65p47 = Image

d-gubc3b62 = Job
    .doc = The engine's job, empty before it starts.

d-hhqtcu4c = Lines
    .doc = The engine's output, newest last.

d-jgmjny63 = Done

d-wektgccg = Failed

d-uk5fnglc-filter-done = Done

d-et2qb7wr = Containers
    .doc = The app's own record: the new-container draft and the pulls started here.
    .noun = { $count ->
        [one] container
       *[other] containers
    }

d-svwiioos = Draft

d-ilelnqwj = Pulls

d-et2qb7wr-filter-draft = Draft

d-putmiceq = Pull ask
    .doc = What begins a pull says which image.
    .noun = { $count ->
        [one] pull ask
       *[other] pull asks
    }

d-tukmkidh = Image

d-xjgsomkl = Pull detail
    .doc = What the engine says of a pull as it runs: the image's output so far.
    .noun = { $count ->
        [one] pull detail
       *[other] pull details
    }

d-bldlgtsf = Lines

d-xcaqps73 = Clear create
    .doc = Clears the new-container form.

d-3zircjtu = Fill
    .doc = Fills one field of the new-container form, as an agent or a script would: `name`, `image`, `ports`, `env`, `volumes` or `command`.

d-3zircjtu-field = Field

d-3zircjtu-text = Text

d-ojpmjsas = Create container
    .doc = Runs an image as a new container from the draft, and clears the form when the engine took it.
    .w-an-image-is-needed-to-run = An image is needed to run a container

d-sv4joym2 = Make volume
    .doc = Makes a volume.
    .w-a-volume-needs-a-name = A volume needs a name

d-sv4joym2-name = Name

d-w5cuwhcr = Tag image
    .doc = Gives an image another reference.
    .w-a-tag-needs-a-name = A tag needs a name

d-w5cuwhcr-image = Image

d-w5cuwhcr-tag = Tag

d-dq4m27xc = Begin pull
    .doc = Starts pulling an image, and keeps its output as it comes.
    .w-there-is-no-image-to-pull = There is no image to pull

d-dq4m27xc-image = Image

d-ickgbq77 = Pull moved
    .doc = The pull moved on: what the engine wrote so far.

d-ickgbq77-j = J

d-5sku455z = Pull ended
    .doc = The pull ended, finished or failed; it says how and stays.

d-5sku455z-j = J

d-muuf5usk = Cancel pull
    .doc = Ends a pull and the engine's process with it.

d-muuf5usk-p = P

d-rov5aa6o = Dismiss pull
    .doc = Takes a finished or failed pull out of the window.

d-rov5aa6o-p = P

d-2wpoozqo = Open shell
    .doc = Opens a shell in a running container: podman's, in the Tilo terminal.

d-2wpoozqo-container = Container

d-krroi7ts = Enter box
    .doc = Opens a shell in a distrobox.

d-krroi7ts-box = Box

d-odguvwgd = Prompt run
    .doc = Runs what the prompt asked, once the person pressed its verb.

d-odguvwgd-prompt = Prompt

d-odguvwgd-typed = Typed

d-jghrbzbr = Confirm run
    .doc = Does what the confirmation named.

d-jghrbzbr-confirm = Confirm

d-lvoxmxjw = Place label
    .w-containers = Containers
    .w-images = Images
    .w-volumes = Volumes
    .w-distrobox = Distrobox

d-nqlyjo45 = State words
    .w-running = Running
    .w-exited = Exited
    .w-created = Created
    .w-paused = Paused
    .w-restarting = Restarting
    .w-needs-attention = Needs attention

d-wkkxefqe = Size words
    .w-b = { $bytes } B
    .w-kb = { $value } KB
    .w-mb = { $value } MB
    .w-gb = { $value } GB

d-lexzqqtm = Used by
    .w-used-by-no-container = Used by no container
    .w-used-by-1-container = Used by 1 container
    .w-used-by-containers = Used by { $count } containers

d-l5th2rd2 = Prompt title
    .w-new-volume = New volume
    .w-pull-an-image = Pull an image
    .w-add-a-tag-to = Add a tag to { $slice }
    .w-remove = Remove { $slice }?

d-bcreuc7u = Prompt label
    .w-the-volume-s-name = The volume's name
    .w-the-image-to-pull = The image to pull
    .w-the-new-tag = The new tag

d-juqarauo = Prompt primary
    .w-create-volume = Create volume
    .w-pull-image = Pull image
    .w-add-tag = Add tag
    .w-remove = Remove
    .w-ok = OK

d-eqfidwbc = Confirm title
    .w-remove-this-container = Remove this container?
    .w-remove-this-image = Remove this image?
    .w-remove-this-volume = Remove this volume?

d-vqxvzopi = Create container~present

d-b6lko7ef = Make volume~present

d-ecmxyqle = Tag image~present

d-6sqyauxv = Begin pull~present

d-khvbvqhn = Window
    .w-containers = Containers
    .w-this-computer = This computer
    .w-images = Images
    .w-volumes = Volumes
    .w-distrobox = Distrobox
    .w-podman-isn-t-answering = Podman isn't answering
    .w-new-container = New container
    .w-read-the-engine-again = Read the engine again
    .w-no-containers = No containers
    .w-run-an-image-to-make-one = Run an image to make one.
    .w-remove = Remove
    .w-detail = Detail
    .w-logs = Logs
    .w-stats = Stats
    .w-inspect = Inspect
    .w-terminal = Terminal
    .w-no-container-selected = No container selected
    .w-choose-a-container-to-see-its = Choose a container to see its logs, usage and settings.
    .w-no-images = No images
    .w-pull-an-image-to-have-something = Pull an image to have something to run.
    .w-images-holding = { $len } images holding { $size_words }
    .w-pull-again = Pull again
    .w-add-a-tag = Add a tag
    .w-pull-an-image = Pull an image
    .w-no-volumes = No volumes
    .w-a-volume-keeps-a-container-s = A volume keeps a container's data across restarts.
    .w-new-volume = New volume
    .w-runs-an-image-as-a-container = Runs an image as a container on this computer.
    .w-create = Create
    .w-cancel = Cancel
    .w-image = Image
    .w-docker-io-library-nginx-alpine = docker.io/library/nginx:alpine
    .w-name = Name
    .w-empty-for-a-name-podman-picks = Empty for a name podman picks.
    .w-my-service = my-service
    .w-ports = Ports
    .w-host-and-container-port-separated-by = Host and container port, separated by commas.
    .w-environment = Environment
    .w-key-value-pairs-separated-by-commas = KEY=value pairs, separated by commas.
    .w-tz-utc-log-level-info = TZ=UTC, LOG_LEVEL=info
    .w-volume-and-path-separated-by-commas = Volume and path, separated by commas.
    .w-pgdata-var-lib-postgresql-data = pgdata:/var/lib/postgresql/data
    .w-command = Command
    .w-empty-to-run-the-image-s = Empty to run the image's default.
    .w-the-container-is-removed-it-is = The container is removed. It is gone for good.
    .w-the-image-is-removed-from-this = The image is removed from this computer. Containers using it keep running.
    .w-the-volume-and-everything-it-kept = The volume and everything it kept are removed.

d-c5zef5ks = Engine missing
    .w-podman-isn-t-available = Podman isn't available
    .w-this-computer-has-no-container-engine = This computer has no container engine to read, or podman did not answer. Install podman and try again.
    .w-try-again = Try again

d-omimf7ko = Lifecycle buttons
    .w-stop = Stop
    .w-restart = Restart
    .w-pause = Pause
    .w-resume = Resume
    .w-start = Start

d-7iv57xex = Logs pane
    .w-this-container-hasn-t-written-anything = This container hasn't written anything yet.
    .w-error = Error
    .w-warning = Warning

d-dyv52unk = Stats pane
    .w-start-the-container-to-see-what = Start the container to see what it uses.
    .w-podman-returned-no-sample-for-this = Podman returned no sample for this container.
    .w-usage = Usage
    .w-processor = Processor
    .w-of-one-processor = { $value }% of one processor
    .w-memory = Memory
    .w-of = { $size_words } of { $size_words2 }
    .w-processes = Processes
    .w-activity = Activity
    .w-received = Received
    .w-sent = Sent
    .w-read-from-disk = Read from disk
    .w-written-to-disk = Written to disk

d-hpakp53f = Inspect pane
    .w-details = Details
    .w-id = ID
    .w-image = Image
    .w-command = Command
    .w-created = Created
    .w-state = State
    .w-ports = Ports
    .w-mounts = Mounts
    .w-read-only = { $source } (read-only)
    .w-environment = Environment
    .w-labels = Labels

d-p3ynizc6 = Terminal pane
    .w-opens-bin-sh-inside-the-container = Opens /bin/sh inside the container, in a terminal window.
    .w-open-a-shell = Open a shell
    .w-start-the-container-to-open-a = Start the container to open a shell in it.
    .w-start = Start

d-72k7h2qu = Pull banner
    .w-pulling = Pulling { $image }
    .w-the-image-is-ready = The image is ready.
    .w-the-engine-is-fetching-the-image = The engine is fetching the image.
    .w-stop-this-pull = Stop this pull
    .w-close = Close

d-vrra64ga = Distrobox page
    .w-no-distroboxes = No distroboxes
    .w-distrobox-runs-another-distribution-s-tools = Distrobox runs another distribution's tools in a container.
    .w-enter = Enter

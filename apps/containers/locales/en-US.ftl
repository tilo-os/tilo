## History of the new-container draft

containers-create-draft-name = Named the new container
containers-create-draft-image = Chose the image of the new container
containers-create-draft-ports = Changed the ports of the new container
containers-create-draft-env = Changed the environment of the new container
containers-create-draft-volumes = Changed the volumes of the new container
containers-create-draft-command = Changed the command of the new container
containers-clear-create = Cleared the new container

## Accessibility
containers-a11y-window = Containers

## Window and accessibility copy
containers-pull-log = Every line Podman wrote, oldest first.
containers-image-ready = The image is ready to create containers from.
containers-pull-stopped = Stopped. What had been fetched stays in the image store.
containers-a11y-containers = Containers
containers-sample-data = Sample data
containers-a11y-sections = Sections

## Facts, property labels and section titles
containers-section-labels = Labels
containers-section-environment = Environment
containers-section-mounts = Mounts
containers-host-port = Host port { $port }
containers-section-ports = Ports
containers-state = State
containers-created = Created
containers-command = Command
containers-action-open = Open { $name }
containers-section-details = Details
containers-section-activity = Activity
containers-section-usage = Usage
containers-images-summary = { $count ->
    [one] One image uses { $size } of disk space.
   *[other] { $count } images use { $size } of disk space.
}
containers-volumes-in-use = { $in_use } of { $total } volumes are in use.

## What the window shows
containers-logs-unavailable = Log output is unavailable: { $detail }
containers-logs-empty = The container hasn't written any log output.
containers-usage-waiting = Usage appears while the container runs.
containers-usage-unavailable = Usage is unavailable. Podman did not return a telemetry sample.
containers-cpu = CPU
containers-memory = Memory (RSS)
containers-processes = Processes
containers-open-shell = Open shell
containers-start = Start
containers-stop-download = Stop the download
containers-new = New container
containers-close-details = Close details
containers-demo-unavailable = Distrobox actions are unavailable in demo mode

## Status pills, sections, tabs and actions
containers-status-running = Running
containers-status-exited = Exited
containers-status-paused = Paused
containers-status-restarting = Restarting
containers-status-dead = Dead
containers-nav-images = Images
containers-nav-volumes = Volumes
containers-nav-distrobox = Distrobox
containers-tab-logs = Logs
containers-tab-stats = Stats
containers-tab-inspect = Inspect
containers-tab-terminal = Terminal
containers-action-stop = Stop
containers-action-restart = Restart
containers-action-pause = Pause
containers-action-resume = Resume
containers-action-remove = Remove
containers-prompt-name = Name
containers-prompt-tag = New tag
containers-prompt-image = Image

## What a removal leaves behind and what a pull is fetching
containers-remove-container-body = The container and the changes made inside it are deleted. Its volumes stay.
containers-remove-image-body = The image is deleted from this computer. You can pull it again later.
containers-remove-volume-body = The volume and the data stored in it are deleted.
containers-used-by = { $count ->
    [0] Not used by any container
    [one] Used by 1 container
   *[other] Used by { $count } containers
}
containers-downloading = Downloading layers
containers-downloading-of = Downloading layers: { $done } of { $total }
containers-downloading-so-far = Downloading layers: { $done } so far
containers-empty-containers-hint = Create a container from an image to run it here.
containers-engine-missing-hint = Install Podman or start its user service to manage containers here.
containers-none-selected-hint = Choose a container to see its logs, usage and settings.
containers-empty-images-hint = Pull an image to create containers from it.
containers-empty-volumes-hint = A volume keeps a container's data when the container is removed.
containers-empty-distroboxes-hint = Create a box with distrobox to run it here.

## Why an engine action cannot run
containers-engine-offline-message = Container engine actions are unavailable in demo mode

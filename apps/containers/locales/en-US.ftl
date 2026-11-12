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

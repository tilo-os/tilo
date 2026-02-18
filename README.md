# QuickDesktop

A collaborative, spatial desktop environment built on a bootable P2P filesystem.

## Overview

QuickDesktop is a modern operating system experiment that integrates a P2P collaborative filesystem directly into the desktop experience. It is built on Fedora Bootc and uses a custom Wayland compositor.

## Key Features

-   **Collaborative Filesystem**: A FUSE-based P2P filesystem (`quick-fs-fuse`) that allows real-time collaboration.
-   **Spatial Desktop**: A custom Wayland compositor (`quick-desktop`) designed for intuitive window management.
-   **Native Apps**: Includes a terminal (`quick-terminal`), file explorer (`file-explorer`), and a modern browser (`quick-browser`).

## Getting Started

Please see [DEVELOPMENT.md](DEVELOPMENT.md) for instructions on how to set up your development environment and build the project.

## Documentation

Detailed documentation can be found in the `docs/` directory:

-   [Architecture](docs/architecture.md): Overview of the system design and components.
-   [Testing](docs/testing.md): Guide to building and testing the OS image in a VM.

## License

(Include license information here)

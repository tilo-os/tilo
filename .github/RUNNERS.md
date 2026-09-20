# GitHub Actions runner contract

Fast policy and selection jobs run on `ubuntu-24.04`. Native OS, kernel, image,
and packaging jobs use two provider-neutral larger-runner labels:

| Label | Architecture | Minimum size |
| --- | --- | --- |
| `tilo-build-x64` | x86-64 | 16 vCPU, 64 GiB RAM, 600 GB SSD |
| `tilo-build-arm64` | ARM64 | 16 vCPU, 64 GiB RAM, 600 GB SSD |

The labels are the stable workflow interface. They can be backed by GitHub
larger hosted runners or a managed GitHub Actions runner provider without
changing individual workflows. Do not attach persistent, manually maintained
runners to these labels.

The `tilo-os` organization does not currently have the GitHub larger-hosted-
runner API enabled. Jobs using these labels will remain queued until that
entitlement or a managed provider is connected. Standard GitHub-hosted quality,
Windows, and macOS jobs do not depend on those pools.

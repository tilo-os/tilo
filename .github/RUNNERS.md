# GitHub-hosted Actions runners

The migration baseline uses GitHub's standard native runner images:

| Label | Architecture |
| --- | --- |
| `ubuntu-24.04` | x86-64 |
| `ubuntu-24.04-arm` | ARM64 |

Jobs use these GitHub-hosted labels directly. Workload-specific sizing belongs
to the later managed-runner migration and must not leave placeholder labels in
the GitHub workflows.

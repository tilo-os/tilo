# Inference project and application boundary

Tilo owns the installable first-party Chip Design application and reusable
resource/action, UI, collaboration, project, and agent-runtime capabilities.
An app's engineering features must work for a user's own project; the app does
not embed this development project's models, benchmark tasks, or chip design.

Model selection, pinned checkpoint/configuration inputs, coding evaluation,
compiler experiments, RTL, numerical verification, physical design evidence,
and manufacturing studies live in the separate
[tilo/inference-hardware repository](https://code.tilo.so/tilo/inference-hardware).
Users open such projects in their own collaborative workspace through Tilo.
Research artifacts are project data, not installed app contents or OS sources.

The repository boundary test rejects the former research source directories.
Historical published research was migrated with source identities retained;
this change removes it from the current Tilo tree without rewriting history.

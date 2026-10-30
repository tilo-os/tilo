"""Neither the caller's repository nor its user's Git settings may leak into temporary test histories."""

import os


# Repository-local variables reported by `git rev-parse --local-env-vars`.
GIT_REPOSITORY_ENV = (
    "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_OBJECT_DIRECTORY", "GIT_DIR",
    "GIT_COMMON_DIR", "GIT_WORK_TREE", "GIT_IMPLICIT_WORK_TREE", "GIT_INDEX_FILE",
    "GIT_SHALLOW_FILE", "GIT_GRAFT_FILE", "GIT_PREFIX", "GIT_REPLACE_REF_BASE",
    "GIT_NO_REPLACE_OBJECTS", "GIT_CONFIG", "GIT_CONFIG_PARAMETERS", "GIT_CONFIG_COUNT",
)


def isolated_git_environment(**overrides):
    """Copy the caller environment without repository-local Git inputs, reading no global or
    system Git configuration: a developer's `tag.gpgSign` turns a fixture's lightweight tag
    into an annotated one that wants a message, `commit.gpgSign` wants their key, and so on.
    A fixture sets the identity and settings it needs itself."""
    environment = {key: value for key, value in os.environ.items()
                   if key not in GIT_REPOSITORY_ENV}
    environment["GIT_CONFIG_GLOBAL"] = os.devnull
    environment["GIT_CONFIG_NOSYSTEM"] = "1"
    # A fixture's commits must not start a detached `git maintenance`: it
    # would write into the repository (and hold its lock) while the test reads it.
    for index, (key, value) in enumerate((("maintenance.auto", "false"), ("gc.auto", "0"))):
        environment[f"GIT_CONFIG_KEY_{index}"] = key
        environment[f"GIT_CONFIG_VALUE_{index}"] = value
    environment["GIT_CONFIG_COUNT"] = "2"
    return dict(environment, **overrides)

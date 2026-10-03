//! The environment record's side: [`ToolBuilder`] for [`Builder`], and
//! moving an environment's tools to a new runtime.

use std::io::Write;

use tilo_app_package::PackageId;
use tilo_environment::{ToolBuildError, ToolBuilder, ToolRef};

use crate::builder::{BuildError, Builder, ToolStore};
use crate::index::ToolLookup;
use crate::resolve::ResolveError;

impl<S, C> ToolBuilder for Builder<S, C>
where
    S: ToolStore + Send + Sync,
    C: ToolLookup + Send + Sync,
{
    fn build(
        &self,
        runtime: &PackageId,
        packages: &[String],
        progress: &mut dyn Write,
    ) -> Result<PackageId, ToolBuildError> {
        self.build_reporting(runtime, packages, progress)
            .map(|built| built.tool)
            .map_err(to_tool_error)
    }
}

/// How a build failure reads to the shell that asked.
pub fn to_tool_error(error: BuildError) -> ToolBuildError {
    match error {
        BuildError::Resolve(ResolveError::NoMatch(names)) => {
            ToolBuildError::UnknownPackage(names.join(", "))
        }
        other => ToolBuildError::Failed(other.to_string()),
    }
}

/// Every tool of an environment rebuilt for `runtime`, in the same order,
/// for the record's `set-runtime` action. All or nothing: the first tool
/// that fails fails the switch, so the record keeps its old runtime and a
/// tool is never mounted on a runtime it was not built for.
///
/// # Errors
///
/// The failing tool's packages and why.
pub fn rebuild_tools(
    builder: &dyn ToolBuilder,
    runtime: &PackageId,
    tools: &[ToolRef],
    progress: &mut dyn Write,
) -> Result<Vec<ToolRef>, RebuildError> {
    tools
        .iter()
        .map(|tool| {
            let _ = writeln!(
                progress,
                "rebuilding {} for {runtime}",
                tool.packages.join(" ")
            );
            builder
                .build(runtime, &tool.packages, progress)
                .map(|id| ToolRef {
                    id: id.to_string(),
                    packages: tool.packages.clone(),
                })
                .map_err(|source| RebuildError {
                    packages: tool.packages.clone(),
                    source,
                })
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
#[error("rebuilding {packages:?} for the new runtime: {source}")]
pub struct RebuildError {
    pub packages: Vec<String>,
    #[source]
    pub source: ToolBuildError,
}

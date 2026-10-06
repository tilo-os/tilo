//! The environment record's side: [`ToolBuilder`] for [`Builder`], and
//! moving an environment's tools to a new runtime.

use std::collections::BTreeMap;
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

/// Every tool of an environment rebuilt for the new release's runtime image
/// on this node's architecture (`arch`, image `runtime`), in the same
/// order, for the record's `set-runtime` action. Each rebuilt tool records
/// only its `arch` image: images for other architectures were built for the
/// old release, and are built again on first use there. All or nothing: the
/// first tool that fails fails the switch, so the record keeps its old
/// runtime and a tool is never mounted on a runtime it was not built for.
///
/// # Errors
///
/// The failing tool's packages and why.
pub fn rebuild_tools(
    builder: &dyn ToolBuilder,
    arch: &str,
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
                    packages: tool.packages.clone(),
                    images: BTreeMap::from([(arch.to_owned(), id.to_string())]),
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

/// The builder for a device that can't build tools, saying why (no device
/// key readable, no cache directory).
#[derive(Debug, Clone)]
pub struct Unavailable(pub String);

impl ToolBuilder for Unavailable {
    fn build(
        &self,
        _runtime: &PackageId,
        _packages: &[String],
        _progress: &mut dyn Write,
    ) -> Result<PackageId, ToolBuildError> {
        Err(ToolBuildError::Unavailable(self.0.clone()))
    }
}

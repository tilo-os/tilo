//! What the engine holds, published beneath the resource as rows of declared
//! types: a container and its logs, an image, a volume, a distrobox, and the
//! pull in flight. Each row is an item of its type, so search names what it is
//! and the effects it offers are the type's, bound to it.

use ui::framework::LiveRow;
use ui::tilo_resource;

use crate::model::{Container, DistroBox, Image, Volume};
use crate::resource::ContainersEffect;

/// One container the engine holds.
#[derive(tilo_resource::Row)]
#[row(
    kind = "containers.container",
    label = "Container",
    effects = ContainersEffect
)]
pub(crate) struct ContainerRow;

/// The recent logs of one container.
#[derive(tilo_resource::Row)]
#[row(
    kind = "containers.logs",
    label = "Container logs",
    effects = ContainersEffect
)]
pub(crate) struct ContainerLogs;

/// One image in the engine's store.
#[derive(tilo_resource::Row)]
#[row(kind = "containers.image", label = "Image", effects = ContainersEffect)]
pub(crate) struct ImageRow;

/// One volume the engine holds.
#[derive(tilo_resource::Row)]
#[row(kind = "containers.volume", label = "Volume", effects = ContainersEffect)]
pub(crate) struct VolumeRow;

/// One distrobox on this machine.
#[derive(tilo_resource::Row)]
#[row(
    kind = "containers.distrobox",
    label = "Distrobox",
    effects = ContainersEffect
)]
pub(crate) struct DistroboxRow;

/// An image being pulled.
#[derive(tilo_resource::Row)]
#[row(kind = "containers.job", label = "Pull")]
pub(crate) struct PullJob;

pub(crate) fn container_row(container: &Container) -> LiveRow {
    LiveRow::typed::<ContainerRow>(
        format!("container:{}", container.id),
        container.name.clone(),
    )
    // The row's open names the container it opens.
    .labelled(
        ContainersEffect::SELECT_CONTAINER,
        crate::resource::CATALOG.t_args(
            "containers-action-open",
            &[("name", ui::i18n::ArgValue::Text(container.name.clone()))],
        ),
    )
    .row(logs_row(container))
}

pub(crate) fn logs_row(container: &Container) -> LiveRow {
    LiveRow::typed::<ContainerLogs>(
        format!("container:{}:logs", container.id),
        format!("{} logs", container.name),
    )
}

pub(crate) fn image_row(image: &Image) -> LiveRow {
    let reference = image.reference();
    LiveRow::typed::<ImageRow>(format!("image:{reference}"), reference)
}

pub(crate) fn volume_row(volume: &Volume) -> LiveRow {
    LiveRow::typed::<VolumeRow>(format!("volume:{}", volume.name), volume.name.clone())
}

pub(crate) fn distrobox_row(distrobox: &DistroBox) -> LiveRow {
    LiveRow::typed::<DistroboxRow>(
        format!("distrobox:{}", distrobox.name),
        distrobox.name.clone(),
    )
}

pub(crate) fn pull_row(reference: &str) -> LiveRow {
    LiveRow::typed::<PullJob>("containers.pull-job", format!("Pulling {reference}"))
}

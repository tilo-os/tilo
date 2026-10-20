//! The panes people rarely open' part of Settings: the areas, panes, effect
//! families, live areas and settings sections they bring ([`crate::area::Part`]).

use crate::categories::Category;
use crate::effects::AreaVocabulary as _;
use crate::pane_model::model_pane;

use crate::panes::{
    about, accounts, agent_models, device_mesh, disk_unlock, drives, printers, region, speakers,
    storage, time_date, updates, users,
};

/// The host client Device mesh carries pairing requests on, which the areas
/// that act through the host share.
fn mesh_remote(made: &crate::area::Areas) -> ui::RemoteActions {
    made.find::<device_mesh::DeviceMeshArea>()
        .map(device_mesh::DeviceMeshArea::remote)
        .unwrap_or_default()
}

/// Every area the window holds, in the order they are made: an area made
/// later may use one made before (Users and Storage act through Device
/// mesh's host client).
pub(crate) const AREAS: &[(u16, crate::area::MakeArea)] = &[
    (40, |context, _| {
        Box::new(time_date::TimeDateArea::new(context.cx))
    }),
    (70, |context, _| {
        Box::new(updates::UpdatesArea::new(context.cx))
    }),
    (80, |context, _| {
        Box::new(disk_unlock::DiskUnlockArea::new(context.cx))
    }),
    (90, |context, _| {
        Box::new(drives::DrivesArea::new(context.cx))
    }),
    (110, |context, _| {
        Box::new(printers::PrintersArea::new(context.cx))
    }),
    (130, |context, _| {
        Box::new(speakers::SpeakersArea::new(context.cx))
    }),
    (140, |context, _| {
        Box::new(agent_models::AgentModelsArea::new(context.cx))
    }),
    (150, |context, _| {
        Box::new(device_mesh::DeviceMeshArea::new(context.cx))
    }),
    (160, |context, made| {
        Box::new(users::UsersArea::new(context.cx, mesh_remote(made)))
    }),
    (170, |context, made| {
        Box::new(storage::StorageArea::new(context.cx, mesh_remote(made)))
    }),
    (190, |context, _| {
        Box::new(
            accounts::AccountsArea::new(context.cx).with_account_root(context.account_root.clone()),
        )
    }),
];

/// Every area's vocabulary, in the order the union declares them.
pub(crate) const FAMILIES: &[(u16, &crate::effects::EffectFamily)] = &[
    (50, time_date::TimeDateEffect::FAMILY),
    (90, updates::UpdatesEffect::FAMILY),
    (100, disk_unlock::DiskUnlockEffect::FAMILY),
    (110, drives::DrivesEffect::FAMILY),
    (120, storage::StorageEffect::FAMILY),
    (130, about::AboutEffect::FAMILY),
    (150, printers::PrinterEffect::FAMILY),
    (160, users::UsersEffect::FAMILY),
    (180, speakers::SpeakerEffect::FAMILY),
    (190, speakers::SpeakerFlow::FAMILY),
    (200, agent_models::AgentModelsEffect::FAMILY),
    (210, device_mesh::DeviceMeshEffect::FAMILY),
    (230, accounts::AccountsEffect::FAMILY),
];

/// Every area with live rows, in the order their rows are published.
pub(crate) const LIVE_AREAS: &[(u16, crate::effects::LiveArea)] = &[
    // Printers read on a thread of their own.
    (
        70,
        crate::effects::LiveArea {
            name: "settings.rows.printers",
            pane: Category::Printers,
            is_open: |app| printers::area(app).is_some_and(printers::PrintersArea::is_open),
            rows: |app| {
                printers::area(app)
                    .map(printers::PrintersArea::rows)
                    .unwrap_or_default()
            },
            background: None,
        },
    ),
    (
        80,
        crate::effects::LiveArea {
            name: "settings.rows.users",
            pane: Category::Users,
            is_open: |app| users::area(app).is_some_and(users::UsersArea::is_open),
            rows: |app| {
                users::area(app)
                    .map(users::UsersArea::rows)
                    .unwrap_or_default()
            },
            background: Some(crate::effects::BackgroundRead {
                start: |app| users::area(app).is_some_and(users::UsersArea::start_read),
                finished: |app| users::area(app).is_none_or(users::UsersArea::read_finished),
            }),
        },
    ),
    // Speakers ask the broker on a thread of their own.
    (
        110,
        crate::effects::LiveArea {
            name: "settings.rows.speakers",
            pane: Category::Speakers,
            is_open: |app| speakers::area(app).is_some_and(speakers::SpeakersArea::is_open),
            rows: |app| {
                speakers::area(app)
                    .map(speakers::SpeakersArea::rows)
                    .unwrap_or_default()
            },
            background: None,
        },
    ),
    (
        120,
        crate::effects::LiveArea {
            name: "settings.rows.agent_models",
            pane: Category::AgentModels,
            is_open: |app| {
                agent_models::area(app).is_some_and(agent_models::AgentModelsArea::is_open)
            },
            rows: |app| {
                agent_models::area(app)
                    .map(agent_models::AgentModelsArea::rows)
                    .unwrap_or_default()
            },
            background: Some(crate::effects::BackgroundRead {
                start: |app| {
                    agent_models::area(app).is_some_and(agent_models::AgentModelsArea::start_read)
                },
                finished: |app| {
                    agent_models::area(app).is_none_or(agent_models::AgentModelsArea::read_finished)
                },
            }),
        },
    ),
    (
        130,
        crate::effects::LiveArea {
            name: "settings.rows.device_mesh",
            pane: Category::DeviceMesh,
            is_open: |app| device_mesh::area(app).is_some_and(device_mesh::DeviceMeshArea::is_open),
            rows: |app| {
                device_mesh::area(app)
                    .map(device_mesh::DeviceMeshArea::rows)
                    .unwrap_or_default()
            },
            background: Some(crate::effects::BackgroundRead {
                start: |app| {
                    device_mesh::area(app).is_some_and(device_mesh::DeviceMeshArea::start_read)
                },
                finished: |app| {
                    device_mesh::area(app).is_none_or(device_mesh::DeviceMeshArea::read_finished)
                },
            }),
        },
    ),
    (
        150,
        crate::effects::LiveArea {
            name: "settings.rows.accounts",
            pane: Category::ExternalAccounts,
            is_open: |app| accounts::area(app).is_some_and(accounts::AccountsArea::is_open),
            rows: |app| {
                accounts::area(app)
                    .map(accounts::AccountsArea::rows)
                    .unwrap_or_default()
            },
            background: Some(crate::effects::BackgroundRead {
                start: |app| accounts::area(app).is_some_and(accounts::AccountsArea::start_read),
                finished: |app| {
                    accounts::area(app).is_none_or(accounts::AccountsArea::read_finished)
                },
            }),
        },
    ),
    // Disk unlock's rows show only what its pane has already read.
    (
        220,
        crate::effects::LiveArea {
            name: "settings.rows.disk_unlock",
            pane: Category::Permissions,
            is_open: |_| true,
            rows: |app| {
                disk_unlock::area(app)
                    .map(disk_unlock::DiskUnlockArea::rows)
                    .unwrap_or_default()
            },
            background: None,
        },
    ),
    // Remembered drives' rows show only what the Storage pane has read.
    (
        240,
        crate::effects::LiveArea {
            name: "settings.rows.drives",
            pane: Category::Storage,
            is_open: |_| true,
            rows: |app| {
                drives::area(app)
                    .map(drives::DrivesArea::rows)
                    .unwrap_or_default()
            },
            background: None,
        },
    ),
];

/// The sections of the person's settings the panes own.
pub(crate) const SECTIONS: &[(u16, crate::area::SectionEntry)] = &[(
    20,
    crate::area::section_entry!(agent_models::AgentProviders),
)];

/// Every pane, by the categories it shows.
pub(crate) const PANES: &[crate::area::PaneEntry] = &[
    crate::area::PaneEntry {
        categories: &[Category::TimeDate],
        build: |app, _| {
            time_date::area(app).map_or_else(
                || crate::widgets::page(Vec::new()),
                |area| {
                    Box::new(time_date::TimeDatePane::new(app.host().clone(), area))
                        as Box<dyn ui::widget::Widget>
                },
            )
        },
    },
    crate::area::PaneEntry {
        categories: &[Category::DeviceMesh],
        build: |app, _| model_pane(device_mesh::area(app).map(device_mesh::DeviceMeshArea::model)),
    },
    crate::area::PaneEntry {
        categories: &[Category::Speakers],
        build: |app, _| model_pane(speakers::area(app).map(speakers::SpeakersArea::model)),
    },
    crate::area::PaneEntry {
        categories: &[Category::Users],
        build: |app, _| model_pane(users::area(app).map(users::UsersArea::model)),
    },
    crate::area::PaneEntry {
        categories: &[Category::Printers],
        build: |app, _| model_pane(printers::area(app).map(printers::PrintersArea::model)),
    },
    crate::area::PaneEntry {
        categories: &[Category::Storage],
        build: |app, _| {
            storage::build_with_drives(
                model_pane(storage::area(app).map(storage::StorageArea::model)),
                model_pane(drives::area(app).map(drives::DrivesArea::model)),
            )
        },
    },
    crate::area::PaneEntry {
        categories: &[Category::ExternalAccounts],
        build: |app, _| model_pane(accounts::area(app).map(accounts::AccountsArea::model)),
    },
    crate::area::PaneEntry {
        categories: &[Category::AgentModels],
        build: |app, _| {
            model_pane(agent_models::area(app).map(agent_models::AgentModelsArea::model))
        },
    },
    crate::area::PaneEntry {
        categories: &[Category::RegionFormats],
        build: |app, _| Box::new(region::RegionPane::new(app.host().clone())),
    },
    crate::area::PaneEntry {
        categories: &[Category::Updates],
        build: |app, _| model_pane(updates::area(app).map(updates::UpdatesArea::model)),
    },
    crate::area::PaneEntry {
        categories: &[Category::About],
        build: |app, _| about::build(app.host()),
    },
];

/// The part.
pub const PART: crate::area::Part = crate::area::Part {
    areas: AREAS,
    panes: PANES,
    families: FAMILIES,
    live_areas: LIVE_AREAS,
    sections: SECTIONS,
};

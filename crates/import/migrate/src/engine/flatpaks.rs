//! The Flatpak apps installed on this computer, as a migration source: each is installed again
//! through the Store's own action (`flatpak-install`), from Flathub, on the device the person
//! uses, with signature checks as any install has. What the apps keep (`~/.var/app`) comes with
//! the home's settings ([`super::settings`]).

use anyhow::Result;
use serde_json::json;

use super::registry::Row;
use super::state::{Item, Review};
use super::{Cx, Here, Landed, Lander, Source};

/// The remote the Store installs from.
const FLATHUB: &str = "flathub";

/// One installed app.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Installed {
    id: String,
    name: String,
    origin: String,
}

/// The Flatpak apps on this computer.
pub struct Flatpaks;

impl Source for Flatpaks {
    fn here(&self, _: &Cx<'_>, row: &'static Row) -> Here {
        match installed() {
            Ok(apps) if apps.is_empty() => Here {
                unavailable: Some("this computer has no Flatpak apps".to_owned()),
                ..Here::default()
            },
            Ok(apps) => Here {
                here: Some(json!(format!("{} app(s)", apps.len()))),
                next: Some(format!("tilo migrate review {}", row.key)),
                ..Here::default()
            },
            Err(why) => Here {
                unavailable: Some(why),
                ..Here::default()
            },
        }
    }

    fn review(&self, cx: &Cx<'_>, row: &'static Row, _: Option<&str>) -> Result<Review> {
        let (apps, unreadable) = match installed() {
            Ok(apps) => (apps, Vec::new()),
            Err(why) => (Vec::new(), vec![why]),
        };
        let items = apps
            .into_iter()
            .map(|app| Item {
                note: (app.origin != FLATHUB).then(|| {
                    format!(
                        "Installed from {}, not Flathub: add that repository in the Store first",
                        app.origin
                    )
                }),
                key: app.id,
                kind: "app".to_owned(),
                title: app.name,
                count: 1,
                counts: "apps".to_owned(),
                within: None,
                chosen: false,
            })
            .collect();
        let mut review = Review {
            source: row.key.to_owned(),
            title: "Flatpak apps on this computer".to_owned(),
            handle: String::new(),
            items,
            unreadable,
        };
        super::keep_chosen(&mut review, cx.place.review(row.key)?.as_ref());
        Ok(review)
    }

    fn summary(&self, _: &'static Row, review: &Review) -> String {
        let chosen: Vec<&str> = review
            .chosen()
            .iter()
            .map(|item| item.title.as_str())
            .collect();
        format!(
            "Install {}: {}",
            super::counted(chosen.len(), "apps"),
            chosen.join(", ")
        )
    }

    fn land(
        &self,
        _: &Cx<'_>,
        _: &'static Row,
        review: &Review,
        _: Option<&super::files::Sink>,
        lander: &mut Lander<'_>,
    ) -> Result<()> {
        lander.open("store");
        for item in review.chosen() {
            let app = item.key.clone();
            lander.once(&app, "apps", |workspace, run| {
                workspace.act(
                    run,
                    &format!("store/app:{app}#flatpak-install"),
                    json!({"app": app}),
                )?;
                Ok(Landed::One)
            })?;
        }
        Ok(())
    }
}

/// The apps `flatpak` lists, in every installation of this computer.
#[cfg(target_os = "linux")]
fn installed() -> Result<Vec<Installed>, String> {
    let mut command = std::process::Command::new("flatpak");
    command.args(["list", "--app", "--columns=application,name,origin"]);
    let Ok(output) = bounded_command::run_prepared(
        command,
        None,
        1024 * 1024,
        std::time::Duration::from_secs(20),
    ) else {
        // No Flatpak here is no app.
        return parse("");
    };
    if !output.status.success() {
        return Err("Flatpak didn't list its apps".to_owned());
    }
    parse(&String::from_utf8_lossy(&output.stdout))
}

/// Flatpak is Linux's.
#[cfg(not(target_os = "linux"))]
fn installed() -> Result<Vec<Installed>, String> {
    parse("")
}

/// `flatpak list`'s rows: id, name and origin, tab-separated, each app once.
fn parse(listed: &str) -> Result<Vec<Installed>, String> {
    let mut apps: Vec<Installed> = Vec::new();
    for line in listed.lines().filter(|line| !line.trim().is_empty()) {
        let mut columns = line.split('\t').map(str::trim);
        let (Some(id), Some(name), Some(origin)) = (columns.next(), columns.next(), columns.next())
        else {
            return Err("Flatpak listed an app Tilo can't read".to_owned());
        };
        let plain = |value: &str| {
            !value.is_empty()
                && value.len() <= 255
                && !value.starts_with(['-', '.'])
                && value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        };
        if !plain(id) || !plain(origin) {
            return Err(format!("Flatpak listed an app Tilo can't read: {id}"));
        }
        if !apps.iter().any(|app| app.id == id) {
            apps.push(Installed {
                id: id.to_owned(),
                name: if name.is_empty() { id } else { name }.to_owned(),
                origin: origin.to_owned(),
            });
        }
    }
    Ok(apps)
}

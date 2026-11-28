//! The folders Windows keeps a person's things in outside their home's familiar folders, as a
//! migration source: redirected Documents, Pictures, Music, Videos and Desktop, each `OneDrive`
//! account's folder, Outlook's archives and local stores, and Steam's game saves. Windows names
//! them in the registry, which is read for their paths only; the folders are copied by the files
//! engine ([`super::files`]) as any folder here is.
//!
//! A Windows home read from its disk ([`crate::old_system`]) has no registry running: its folders
//! are found where Windows puts them unless told otherwise ([`on_disk`]).

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use super::files::{self, Local, Tree as _};
use super::registry::Row;
use super::state::{Item, Journal, Review};
use super::{Cx, Here, Lander, Source};

/// The folders Windows names.
pub struct Windows;

/// What a folder Windows names is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Known,
    OneDrive,
    Outlook,
    Steam,
}

/// One folder Windows names.
#[derive(Clone, Debug)]
struct Named {
    path: PathBuf,
    role: Role,
    label: String,
}

impl Source for Windows {
    fn here(&self, _: &Cx<'_>, row: &'static Row) -> Here {
        if cfg!(windows) {
            Here {
                next: Some(format!("tilo migrate review {}", row.key)),
                ..Here::default()
            }
        } else {
            Here {
                unavailable: Some(
                    "Windows keeps these folders; this computer runs no Windows".into(),
                ),
                ..Here::default()
            }
        }
    }

    fn review(&self, cx: &Cx<'_>, row: &'static Row, _: Option<&str>) -> Result<Review> {
        let local = Local {
            home: cx.home.to_path_buf(),
        };
        let mut unreadable = Vec::new();
        let items = match named() {
            Ok(folders) => items(folders, &local),
            Err(why) => {
                unreadable.push(why);
                Vec::new()
            }
        };
        let mut review = Review {
            source: row.key.to_owned(),
            title: "Folders Windows keeps".to_owned(),
            handle: cx.home.display().to_string(),
            items,
            unreadable,
        };
        super::keep_chosen(&mut review, cx.place.review(row.key)?.as_ref());
        Ok(review)
    }

    fn summary(&self, row: &'static Row, review: &Review) -> String {
        files::summary(review, row.title)
    }

    fn files_under(&self, _: &'static Row, _: &super::state::Review) -> Option<&'static str> {
        Some("")
    }

    fn land(
        &self,
        cx: &Cx<'_>,
        _: &'static Row,
        review: &Review,
        sink: Option<&files::Sink>,
        lander: &mut Lander<'_>,
    ) -> Result<()> {
        let sink = sink.ok_or_else(|| anyhow!("files have nowhere to go"))?;
        files::land(
            &Local {
                home: cx.home.to_path_buf(),
            },
            review,
            sink,
            cx.place,
            lander,
        )
    }

    fn landed(&self, _: &Cx<'_>, _: &'static Row, journal: &Journal) -> Result<usize> {
        Ok(journal
            .brought
            .values()
            .sum::<usize>()
            .saturating_add(journal.already_there))
    }
}

/// Each of `folders` that is there, as a review lists it: by its path, counted.
fn items(folders: Vec<Named>, local: &Local) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    for folder in folders {
        if !folder.path.is_dir() {
            continue;
        }
        let key = folder.path.to_string_lossy().into_owned();
        if items.iter().any(|item| item.key == key) {
            continue;
        }
        let (count, bytes) = local.files(&key).map_or((0, 0), |(found, _)| {
            (found.len(), found.iter().map(|entry| entry.size).sum())
        });
        items.push(Item {
            key,
            kind: "folder".to_owned(),
            title: folder.label,
            count,
            counts: format!("files, {}", super::size(bytes)),
            within: None,
            chosen: false,
            note: (folder.role == Role::OneDrive).then(|| {
                "Files OneDrive kept only in the cloud are not on this disk; connecting the \
                 OneDrive account brings them from the cloud"
                    .to_owned()
            }),
        });
    }
    items
}

/// The folders Windows keeps a person's things in outside their familiar folders, in the Windows
/// home `home` read from its disk: each `OneDrive` folder of the home, Outlook's archives and
/// local stores, and Steam's game saves, where Windows puts them.
#[must_use]
pub fn on_disk(home: &Path) -> Vec<Item> {
    let mut named = Vec::new();
    if let Ok(entries) = std::fs::read_dir(home) {
        let mut onedrive: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("OneDrive"))
            .map(|entry| entry.path())
            .collect();
        onedrive.sort();
        for path in onedrive {
            let label = label_of(Role::OneDrive, "", &path);
            named.push(Named {
                path,
                role: Role::OneDrive,
                label,
            });
        }
    }
    named.push(Named {
        path: home.join("Documents/Outlook Files"),
        role: Role::Outlook,
        label: "Outlook archives".to_owned(),
    });
    named.push(Named {
        path: home.join("AppData/Local/Microsoft/Outlook"),
        role: Role::Outlook,
        label: "Outlook local stores and drafts".to_owned(),
    });
    if let Some(system) = home.parent().and_then(Path::parent) {
        named.push(Named {
            path: system.join("Program Files (x86)/Steam/userdata"),
            role: Role::Steam,
            label: "Steam game saves".to_owned(),
        });
    }
    items(
        named,
        &Local {
            home: home.to_path_buf(),
        },
    )
}

/// The folders the registry names.
#[cfg(windows)]
fn named() -> Result<Vec<Named>, String> {
    use std::{process::Command, time::Duration};
    let mut command = Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT]);
    let output = bounded_command::run_prepared(command, None, 256 * 1024, Duration::from_secs(10))
        .map_err(|error| format!("Windows' folders could not be read: {error}"))?;
    if !output.status.success() {
        return Err("Windows' folders could not be read".into());
    }
    parse(&output.stdout)
}

/// Off Windows there is no registry: it names no folder.
#[cfg(not(windows))]
fn named() -> Result<Vec<Named>, String> {
    parse(b"[]")
}

/// The registry reader's answer: each folder, what it is and what Windows calls it.
fn parse(bytes: &[u8]) -> Result<Vec<Named>, String> {
    #[derive(serde::Deserialize)]
    struct Record {
        kind: String,
        path: String,
        label: String,
    }
    if bytes.len() > 256 * 1024 {
        return Err("Windows named too many folders".into());
    }
    let records: Vec<Record> =
        serde_json::from_slice(bytes).map_err(|_| "Windows' folders could not be read")?;
    if records.len() > 256 {
        return Err("Windows named too many folders".into());
    }
    let mut named = Vec::new();
    for record in records {
        let role = match record.kind.as_str() {
            "known" => Role::Known,
            "onedrive" => Role::OneDrive,
            "outlook" => Role::Outlook,
            "steam" => Role::Steam,
            _ => return Err("Windows named a folder of a kind Tilo does not know".into()),
        };
        if record.path.is_empty() || record.path.contains('\0') || record.path.len() > 32768 {
            return Err("Windows named a folder by no usable path".into());
        }
        let path = PathBuf::from(&record.path);
        // Steam's own folder holds its programs; what is the person's are their games' saves.
        let (path, label) = if role == Role::Steam {
            (path.join("userdata"), "Steam game saves".to_owned())
        } else {
            (path, label_of(role, &record.label, Path::new(&record.path)))
        };
        named.push(Named { path, role, label });
    }
    Ok(named)
}

/// What a person calls a folder Windows names `label`.
fn label_of(role: Role, label: &str, path: &Path) -> String {
    let name = path.file_name().map_or_else(
        || label.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    match role {
        Role::Known => name,
        Role::OneDrive => format!("OneDrive ({name})"),
        Role::Outlook => label.to_owned(),
        Role::Steam => "Steam".to_owned(),
    }
}

// No interpolation, wildcard profile paths, credentials or recursive registry walks: the values
// name folders and nothing else.
#[cfg(windows)]
const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$rows = [System.Collections.Generic.List[object]]::new()
function Add-Root($kind, $path, $label) {
 if ($path -is [string] -and $path.Length -gt 0) {
  $rows.Add(@{ kind=$kind; path=[Environment]::ExpandEnvironmentVariables($path); label=$label })
 }
}
$folders = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders'
if (Test-Path -LiteralPath $folders) {
 $v = Get-ItemProperty -LiteralPath $folders
 foreach ($name in @('Personal','Desktop','My Pictures','My Music','My Video','{374DE290-123F-4565-9164-39C4925E467B}','{4C5C32FF-BB9D-43B0-B5B4-2D72E54EAAA4}')) {
  Add-Root 'known' $v.$name $name
 }
 if ($v.Personal) { Add-Root 'outlook' (Join-Path ([Environment]::ExpandEnvironmentVariables($v.Personal)) 'Outlook Files') 'Outlook archives' }
}
$accounts = 'HKCU:\Software\Microsoft\OneDrive\Accounts'
if (Test-Path -LiteralPath $accounts) {
 $keys = @(Get-ChildItem -LiteralPath $accounts)
 if ($keys.Count -gt 128) { throw 'Too many OneDrive accounts' }
 foreach ($key in $keys) {
  $v = Get-ItemProperty -LiteralPath $key.PSPath
  Add-Root 'onedrive' $v.UserFolder $key.PSChildName
 }
}
$steam = 'HKCU:\Software\Valve\Steam'
if (Test-Path -LiteralPath $steam) {
 $v = Get-ItemProperty -LiteralPath $steam
 Add-Root 'steam' $v.SteamPath 'Steam'
}
foreach ($version in @('16.0','15.0','14.0')) {
 $outlook = 'HKCU:\Software\Microsoft\Office\' + $version + '\Outlook'
 if (Test-Path -LiteralPath $outlook) {
  $v = Get-ItemProperty -LiteralPath $outlook
  Add-Root 'outlook' $v.ForcePSTPath 'Outlook archives'
  Add-Root 'outlook' $v.ForceOSTPath 'Outlook local stores'
 }
}
$local = [Environment]::GetFolderPath('LocalApplicationData')
if ($local) { Add-Root 'outlook' (Join-Path $local 'Microsoft\Outlook') 'Outlook local stores and drafts' }
[Console]::Out.Write((ConvertTo-Json -InputObject @($rows.ToArray()) -Depth 3 -Compress))
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_windows_disk_names_onedrive_outlook_and_steam_where_windows_puts_them() {
        let disk = tempfile::tempdir().unwrap();
        let home = disk.path().join("Users/ada");
        for folder in [
            "OneDrive - Contoso",
            "OneDrive",
            "Documents/Outlook Files",
            "AppData/Local/Microsoft/Outlook",
        ] {
            std::fs::create_dir_all(home.join(folder)).unwrap();
        }
        std::fs::write(home.join("Documents/Outlook Files/archive.pst"), b"pst").unwrap();
        let saves = disk.path().join("Program Files (x86)/Steam/userdata/42");
        std::fs::create_dir_all(&saves).unwrap();
        std::fs::write(saves.join("save.dat"), b"save").unwrap();
        let found: Vec<(String, usize)> = on_disk(&home)
            .into_iter()
            .map(|item| (item.title, item.count))
            .collect();
        assert_eq!(
            found,
            [
                ("OneDrive (OneDrive)".to_owned(), 0),
                ("OneDrive (OneDrive - Contoso)".to_owned(), 0),
                ("Outlook archives".to_owned(), 1),
                ("Outlook local stores and drafts".to_owned(), 0),
                ("Steam game saves".to_owned(), 1),
            ]
        );
    }

    #[test]
    fn the_registry_names_folders_and_nothing_else() {
        let named = parse(
            br#"[{"kind":"known","path":"D:\\Docs","label":"Personal"},{"kind":"onedrive","path":"C:\\Users\\you\\OneDrive - Work","label":"Business1"},{"kind":"steam","path":"C:\\Steam","label":"Steam"}]"#,
        )
        .unwrap();
        assert_eq!(named.len(), 3);
        assert_eq!(named[2].label, "Steam game saves");
        assert!(named[2].path.ends_with("userdata"));
        assert_eq!(named[1].role, Role::OneDrive);
        assert!(parse(br#"[{"kind":"registry","path":"x","label":"x"}]"#).is_err());
        assert!(parse(br#"[{"kind":"known","path":"","label":"x"}]"#).is_err());
    }
}

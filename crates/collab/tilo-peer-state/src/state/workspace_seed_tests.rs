//! A workspace brought onto the native owner keeps what it showed: the
//! windows in this device's namespace and the record index its apps
//! referenced, not an empty room that would silently drop either.

use super::*;
use tilo_desktop_model::ids::compose_id;
use tilo_desktop_model::projection::{contents_of, room_records_of};
use tilo_desktop_model::width::WindowWidth;
use tilo_desktop_model::{GroupId, Strip, StripId, Tab, TileContent, WindowGroup};

const SALT: u16 = 0x0042;

fn strip() -> Strip {
    let group_id: GroupId = compose_id(SALT, 2);
    let tab = Tab {
        id: compose_id(SALT, 3),
        title: "Notes".into(),
        tile: TileContent::LocalApp {
            app_id: "so.tilo.notes".into(),
            label: "Notes".into(),
            note: String::new(),
        },
        app_title: None,
    };
    let mut strip = Strip::new(StripId(compose_id(SALT, 1)), "Work");
    strip.window_groups.push(WindowGroup {
        id: group_id,
        tabs: vec![tab],
        active_tab: compose_id(SALT, 3),
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    strip
}

fn fixture() -> (SigningKey, DeviceCert, SystemTime) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let key = SigningKey::from_bytes(&[2; 32]);
    let mut authority = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Desktop".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut authority, &root).unwrap();
    let now = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(2000))
        .unwrap();
    (key, authority, now)
}

#[test]
fn a_seeded_workspace_keeps_its_windows_and_record_index() {
    let (key, authority, now) = fixture();
    let room = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let records = BTreeMap::from([("record-1".to_string(), "notes".to_string())]);
    let seed = workspace_seed_with_layout(
        room,
        &authority,
        std::slice::from_ref(&authority),
        "Work",
        SALT,
        &[strip()],
        &records,
    )
    .unwrap();
    let mut peer = StatePeer::new(
        room,
        authority.clone(),
        vec![authority],
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.install_seed(seed, &key, now).unwrap();
    let admitted = peer.admitted_room().expect("the seed is admitted");
    let strips = contents_of(admitted);
    assert_eq!(strips.len(), 1, "the workspace's strip is the room's");
    let groups = &strips[0].groups;
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].tabs.len(), 1);
    assert_eq!(groups[0].tabs[0].title, "Notes");
    assert_eq!(
        room_records_of(admitted),
        vec![("record-1".to_string(), "notes".to_string())],
        "an existing record index travels with the workspace"
    );
}

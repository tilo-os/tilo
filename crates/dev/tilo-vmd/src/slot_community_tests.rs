//! A dev slot joins no public site: its session takes its community from the
//! slot's own file, which names none, whatever the image's vendor file or the
//! host's /etc/tilo/community.toml say. Slots run throwaway test identities,
//! and they joined tilo.coffee's Lounge every ten minutes before this.

const UNIT: &str = include_str!("../data/tilo-vm-session@.service");
const COMMUNITY: &str = include_str!("../data/tilo-vm-community.toml");
const INSTALLED: &str = "/usr/share/tilo/tilo-vm-community.toml";

#[derive(serde::Deserialize)]
struct Community {
    site: String,
}

#[test]
fn a_slot_session_takes_its_community_from_the_slot_file() {
    assert!(
        UNIT.lines()
            .any(|line| line.trim() == format!("Environment=TILO_COMMUNITY_CONFIG={INSTALLED}")),
        "the slot unit points the shell at the slot's community file"
    );
}

#[test]
fn a_slot_names_no_public_site() {
    let community: Community = toml::from_str(COMMUNITY).unwrap();
    assert_eq!(community.site.trim(), "", "a slot's community is none");
    for text in [UNIT, COMMUNITY] {
        assert!(
            !text.contains("tilo.coffee"),
            "slot configuration names no public site (tilo.coffee or any of its hosts)"
        );
    }
}

//! A dev account joins no public site: its session takes its community from the
//! account's own file, which names none, whatever the image's vendor file or the
//! host's /etc/tilo/community.toml say. Accounts run throwaway test identities,
//! and they joined tilo.sh's Lounge every ten minutes before this.

const UNIT: &str = include_str!("../data/tilo-vm-session@.service");
const COMMUNITY: &str = include_str!("../data/tilo-vm-community.toml");
const INSTALLED: &str = "/usr/share/tilo/tilo-vm-community.toml";

#[derive(serde::Deserialize)]
struct Community {
    site: String,
}

tilo_test_env::scenario! {
    fn a_account_joins_no_public_site;

    phase a_account_session_takes_its_community_from_the_account_file;
    {
        assert!(
            UNIT.lines()
                .any(|line| line.trim() == format!("Environment=TILO_COMMUNITY_CONFIG={INSTALLED}")),
            "the account unit points the shell at the account's community file"
        );
    }

    phase a_account_names_no_public_site;
    {
        let community: Community = toml::from_str(COMMUNITY).unwrap();
        assert_eq!(community.site.trim(), "", "an account's community is none");
        for text in [UNIT, COMMUNITY] {
            assert!(
                !text.contains("tilo.sh"),
                "account configuration names no public site (tilo.sh or any of its hosts)"
            );
        }
    }
}

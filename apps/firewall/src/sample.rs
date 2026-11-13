//! The sample ruleset the captures and tests show; a live window reads the host instead.

use crate::model::{Action, FirewallConfig, Matcher, Proto, Rule, Target, Zone, ZoneKind};

fn rule(matcher: Matcher, action: Action, source: &str) -> Rule {
    Rule::new(matcher, action, source)
}

fn svc(name: &str) -> Matcher {
    Matcher::Service(name.to_string())
}

fn port(p: &str, proto: Proto) -> Matcher {
    Matcher::Port {
        port: p.to_string(),
        proto,
    }
}

/// Build the full sample configuration.
pub fn sample_config() -> FirewallConfig {
    let mut zones = Vec::new();

    zones.push(Zone::new(ZoneKind::Drop, Target::Drop));

    zones.push(Zone::new(ZoneKind::Block, Target::Reject));

    let mut public = Zone::new(ZoneKind::Public, Target::Default);
    public.interfaces = vec!["wlan0".into(), "eth0".into()];
    public.services = vec!["ssh".into(), "mdns".into(), "dhcpv6-client".into()];
    public.rules = vec![
        rule(svc("https"), Action::Allow, ""),
        rule(port("8080", Proto::Tcp), Action::Allow, "192.168.1.0/24"),
        rule(svc("cockpit"), Action::Allow, "10.8.0.0/24"),
        rule(port("51820", Proto::Udp), Action::Allow, ""),
        rule(port("23", Proto::Tcp), Action::Deny, ""),
        rule(port("3389", Proto::Tcp), Action::Deny, "0.0.0.0/0"),
        rule(svc("ftp"), Action::Deny, ""),
    ];
    zones.push(public);

    let mut external = Zone::new(ZoneKind::External, Target::Default);
    external.interfaces = vec!["ppp0".into()];
    external.services = vec!["ssh".into()];
    external.masquerade = true;
    external.rules = vec![
        rule(svc("https"), Action::Allow, ""),
        rule(svc("http"), Action::Allow, ""),
        rule(port("25565", Proto::Tcp), Action::Allow, ""),
    ];
    zones.push(external);

    let mut dmz = Zone::new(ZoneKind::Dmz, Target::Default);
    dmz.services = vec!["ssh".into()];
    dmz.rules = vec![rule(svc("https"), Action::Allow, "")];
    zones.push(dmz);

    let mut work = Zone::new(ZoneKind::Work, Target::Default);
    work.interfaces = vec!["eth1".into()];
    work.services = vec![
        "ssh".into(),
        "mdns".into(),
        "dhcpv6-client".into(),
        "cockpit".into(),
    ];
    work.rules = vec![
        rule(svc("samba-client"), Action::Allow, "192.168.0.0/16"),
        rule(svc("ipp"), Action::Allow, "192.168.0.0/16"),
    ];
    zones.push(work);

    let mut home = Zone::new(ZoneKind::Home, Target::Default);
    home.services = vec![
        "ssh".into(),
        "mdns".into(),
        "samba".into(),
        "dhcpv6-client".into(),
        "ipp".into(),
    ];
    home.rules = vec![
        rule(svc("nfs"), Action::Allow, "192.168.0.0/16"),
        rule(port("32400", Proto::Tcp), Action::Allow, "192.168.0.0/16"),
    ];
    zones.push(home);

    let mut internal = Zone::new(ZoneKind::Internal, Target::Default);
    internal.services = vec![
        "ssh".into(),
        "mdns".into(),
        "samba".into(),
        "dhcpv6-client".into(),
    ];
    zones.push(internal);

    zones.push(Zone::new(ZoneKind::Trusted, Target::Accept));

    FirewallConfig {
        enabled: true,
        panic_mode: false,
        default_zone: ZoneKind::Public,
        zones,
    }
}

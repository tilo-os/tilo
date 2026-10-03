//! The guest's one ordinary user, as `/etc/passwd` names it.

use std::path::PathBuf;

/// Apps run as this account. The guest image creates it.
pub const DEFAULT_USER: &str = "tilo";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuestUser {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
    pub shell: String,
}

impl GuestUser {
    /// Look `name` up in a passwd file's contents.
    #[must_use]
    pub fn from_passwd(passwd: &str, name: &str) -> Option<Self> {
        passwd.lines().find_map(|line| {
            let mut fields = line.split(':');
            let user = fields.next()?;
            if user != name {
                return None;
            }
            let _password = fields.next()?;
            let uid = fields.next()?.parse().ok()?;
            let gid = fields.next()?.parse().ok()?;
            let _gecos = fields.next()?;
            let home = PathBuf::from(fields.next()?);
            let shell = fields.next().unwrap_or("/bin/sh").to_string();
            (uid != 0).then(|| Self {
                name: user.to_string(),
                uid,
                gid,
                home,
                shell,
            })
        })
    }

    /// `$XDG_RUNTIME_DIR` for this user; systemd-logind creates it for a
    /// lingering user, the agent otherwise.
    #[must_use]
    pub fn runtime_dir(&self) -> PathBuf {
        PathBuf::from(format!("/run/user/{}", self.uid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guest_user_is_read_from_passwd_and_never_root() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\ntilo:x:1000:1000::/home/tilo:/bin/bash\n";
        let user = GuestUser::from_passwd(passwd, "tilo").unwrap();
        assert_eq!((user.uid, user.gid), (1000, 1000));
        assert_eq!(user.home, PathBuf::from("/home/tilo"));
        assert_eq!(user.runtime_dir(), PathBuf::from("/run/user/1000"));
        assert_eq!(GuestUser::from_passwd(passwd, "root"), None);
        assert_eq!(GuestUser::from_passwd(passwd, "nobody"), None);
    }
}

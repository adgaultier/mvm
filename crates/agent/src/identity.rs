use std::os::unix::process::CommandExt;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GuestUser {
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) groups: Vec<u32>,
    pub(super) name: String,
    pub(super) home: String,
}

impl GuestUser {
    pub(super) fn root() -> Self {
        Self {
            uid: 0,
            gid: 0,
            groups: vec![0],
            name: "root".into(),
            home: "/root".into(),
        }
    }

    pub(super) fn is_root(&self) -> bool {
        self.uid == 0 && self.gid == 0
    }
}

/// Resolve a docker-style user spec against the guest rootfs.
pub(super) fn resolve_user(spec: &str) -> Result<GuestUser, String> {
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    let group = std::fs::read_to_string("/etc/group").unwrap_or_default();
    resolve_user_from(spec, &passwd, &group)
}

pub(super) fn resolve_user_from(
    spec: &str,
    passwd: &str,
    group_file: &str,
) -> Result<GuestUser, String> {
    let (user_part, group_part) = match spec.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (spec, None),
    };

    let entry = passwd.lines().find(|line| {
        let mut fields = line.split(':');
        match (fields.next(), fields.nth(1)) {
            (Some(name), Some(uid)) => name == user_part || uid == user_part,
            _ => false,
        }
    });
    let (name, uid, mut gid, home) = match entry {
        Some(line) => {
            let fields: Vec<&str> = line.split(':').collect();
            if fields.len() < 6 {
                return Err(format!("malformed /etc/passwd entry for '{user_part}'"));
            }
            let uid = fields[2]
                .parse()
                .map_err(|_| format!("bad uid in /etc/passwd for '{user_part}'"))?;
            let gid = fields[3]
                .parse()
                .map_err(|_| format!("bad gid in /etc/passwd for '{user_part}'"))?;
            (fields[0].to_string(), uid, gid, fields[5].to_string())
        }
        None => match user_part.parse::<u32>() {
            Ok(uid) => (user_part.to_string(), uid, 0, "/".to_string()),
            Err(_) => {
                return Err(format!(
                    "unable to find user '{user_part}': no matching entry in /etc/passwd"
                ));
            }
        },
    };

    let gid_of = |want: &str| -> Option<u32> {
        group_file.lines().find_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            if fields.len() >= 3 && (fields[0] == want || fields[2] == want) {
                fields[2].parse().ok()
            } else {
                None
            }
        })
    };
    if let Some(group_part) = group_part {
        gid = match gid_of(group_part) {
            Some(gid) => gid,
            None => group_part.parse::<u32>().map_err(|_| {
                format!("unable to find group '{group_part}': no matching entry in /etc/group")
            })?,
        };
    }

    let mut groups = vec![gid];
    for line in group_file.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 4 {
            continue;
        }
        let Ok(group) = fields[2].parse::<u32>() else {
            continue;
        };
        if group != gid && fields[3].split(',').any(|member| !member.is_empty() && member == name) {
            groups.push(group);
        }
    }

    Ok(GuestUser {
        uid,
        gid,
        groups,
        name,
        home,
    })
}

/// Apply the resolved guest identity between fork and exec.
pub(super) fn apply_user(cmd: &mut Command, user: &GuestUser) {
    cmd.env("HOME", &user.home)
        .env("USER", &user.name)
        .env("LOGNAME", &user.name);
    if user.is_root() {
        return;
    }
    let (uid, gid, groups) = (user.uid, user.gid, user.groups.clone());
    unsafe {
        cmd.pre_exec(move || {
            if libc::setgroups(groups.len() as _, groups.as_ptr()) != 0
                || libc::setgid(gid) != 0
                || libc::setuid(uid) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_user_from, GuestUser};

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/ash\n\
                          bin:x:1:1:bin:/bin:/sbin/nologin\n\
                          agent:x:1000:1000:agent:/home/agent:/bin/sh\n\
                          nobody:x:65534:65534:nobody:/:/sbin/nologin\n";
    const GROUP: &str = "root:x:0:root\n\
                         bin:x:1:root,bin,daemon\n\
                         agent:x:1000:\n\
                         docker:x:998:agent\n\
                         wheel:x:10:agent,bin\n\
                         nobody:x:65534:\n";

    fn resolve(spec: &str) -> GuestUser {
        resolve_user_from(spec, PASSWD, GROUP).expect("resolves")
    }

    #[test]
    fn resolves_by_name_with_home_and_groups() {
        let user = resolve("agent");
        assert_eq!((user.uid, user.gid), (1000, 1000));
        assert_eq!(user.name, "agent");
        assert_eq!(user.home, "/home/agent");
        assert_eq!(user.groups, vec![1000, 998, 10]);
        assert!(!user.is_root());
    }

    #[test]
    fn resolves_by_uid_and_by_explicit_group() {
        assert_eq!(resolve("1000").name, "agent");
        assert_eq!(resolve("65534").home, "/");

        let user = resolve("agent:bin");
        assert_eq!((user.uid, user.gid), (1000, 1));
        assert_eq!(user.groups, vec![1, 998, 10]);

        let user = resolve("agent:4242");
        assert_eq!(user.gid, 4242);
        assert_eq!(user.groups[0], 4242);
    }

    #[test]
    fn root_is_recognised_as_root() {
        let user = resolve("root");
        assert!(user.is_root());
        assert_eq!(user.home, "/root");
        assert!(GuestUser::root().is_root());
    }

    #[test]
    fn unknown_numeric_id_is_allowed_unknown_name_is_not() {
        let user = resolve("4242");
        assert_eq!((user.uid, user.gid), (4242, 0));
        assert_eq!(user.name, "4242");
        assert_eq!(user.home, "/");
        assert_eq!(user.groups, vec![0]);

        let err = resolve_user_from("nosuchuser", PASSWD, GROUP).unwrap_err();
        assert!(err.contains("unable to find user 'nosuchuser'"), "{err}");
        let err = resolve_user_from("agent:nosuchgroup", PASSWD, GROUP).unwrap_err();
        assert!(err.contains("unable to find group 'nosuchgroup'"), "{err}");
    }

    #[test]
    fn survives_an_image_without_passwd() {
        let user = resolve_user_from("0", "", "").unwrap();
        assert!(user.is_root());
        assert!(resolve_user_from("agent", "", "").is_err());
    }
}

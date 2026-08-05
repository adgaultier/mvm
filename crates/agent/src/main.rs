//! mvm-agent entry point.
//!
//! The real agent only runs inside Linux guests (see linux.rs); the musl
//! cross-builds compile it directly. On non-Linux hosts this stub keeps
//! `cargo build --workspace` green and explains the mistake if the host
//! binary is ever executed.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
fn main() {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "mvm-agent only runs inside Linux guests; build it for a musl target \
         (e.g. cargo zigbuild --release -p mvm-agent --target aarch64-unknown-linux-musl)"
    );
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::{resolve_user_from, GuestUser};

    // Trimmed from a real alpine image, plus a docker-style app user.
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
        let u = resolve("agent");
        assert_eq!((u.uid, u.gid), (1000, 1000));
        assert_eq!(u.name, "agent");
        assert_eq!(u.home, "/home/agent");
        // Primary gid first, then every group listing the user as a member.
        assert_eq!(u.groups, vec![1000, 998, 10]);
        assert!(!u.is_root());
    }

    #[test]
    fn resolves_by_uid_and_by_explicit_group() {
        assert_eq!(resolve("1000").name, "agent");
        assert_eq!(resolve("65534").home, "/");

        let u = resolve("agent:bin");
        assert_eq!((u.uid, u.gid), (1000, 1));
        // wheel/docker list "agent", bin is now primary, so it is not repeated.
        assert_eq!(u.groups, vec![1, 998, 10]);

        let u = resolve("agent:4242");
        assert_eq!(u.gid, 4242);
        assert_eq!(u.groups[0], 4242);
    }

    #[test]
    fn root_is_recognised_as_root() {
        let u = resolve("root");
        assert!(u.is_root());
        assert_eq!(u.home, "/root");
        assert!(GuestUser::root().is_root());
    }

    #[test]
    fn unknown_numeric_id_is_allowed_unknown_name_is_not() {
        // docker: a bare uid needs no passwd entry (gid 0, home /).
        let u = resolve("4242");
        assert_eq!((u.uid, u.gid), (4242, 0));
        assert_eq!(u.name, "4242");
        assert_eq!(u.home, "/");
        assert_eq!(u.groups, vec![0]);

        let err = resolve_user_from("nosuchuser", PASSWD, GROUP).unwrap_err();
        assert!(err.contains("unable to find user 'nosuchuser'"), "{err}");
        let err = resolve_user_from("agent:nosuchgroup", PASSWD, GROUP).unwrap_err();
        assert!(err.contains("unable to find group 'nosuchgroup'"), "{err}");
    }

    #[test]
    fn survives_an_image_without_passwd() {
        // Scratch-style rootfs: numeric ids still work, names cannot.
        let u = resolve_user_from("0", "", "").unwrap();
        assert!(u.is_root());
        assert!(resolve_user_from("agent", "", "").is_err());
    }
}

//! What to tell an operator when the data directory cannot be written.
//!
//! The Docker image runs as the unprivileged `nonroot` user, uid/gid
//! [`IMAGE_UID`]. A named volume inherits the image's `/data` and its owner,
//! but a Linux bind mount keeps the host directory's owner, so a directory
//! created by root (or by `docker run` itself) refuses every write. REL-09:
//! the error then names the directory, who owns it, who Vyrtel runs as, and
//! the documented fix, instead of only "Permission denied".

use std::path::Path;

/// uid and gid of the `nonroot` user the Docker image runs as.
pub const IMAGE_UID: u32 = 65532;

/// Whether an error message is the operating system refusing to let Vyrtel
/// create or change a file.
pub fn is_access_error(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("permission denied") || m.contains("access is denied") || m.contains("read-only file system")
}

/// Owner and permission bits of a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Owner {
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
}

/// The hint printed after `error`, an access error on `dir`, from what this
/// process can find out about itself and the directory.
pub fn hint(dir: &Path, error: &str) -> String {
    let (shown, owner) = owner_of(dir);
    let read_only = error.to_ascii_lowercase().contains("read-only file system");
    hint_text(&shown, process_ids(), owner, read_only)
}

/// The hint for `dir` (or its nearest existing ancestor), owned by `owner`,
/// when Vyrtel runs as `process` (uid, gid). Either is `None` when the
/// platform does not say. On a `read_only` file system ownership is not the
/// problem, so the fix is to mount it read-write instead.
pub fn hint_text(dir: &Path, process: Option<(u32, u32)>, owner: Option<Owner>, read_only: bool) -> String {
    let dir = dir.display();
    let mut text = format!("hint: the data directory {dir} must be writable by the user running Vyrtel.\n");
    match (process, owner) {
        (Some((uid, gid)), Some(o)) => text.push_str(&format!(
            "      Vyrtel runs as uid {uid} (gid {gid}); {dir} is owned by uid {} (gid {}) with mode {:03o}.\n",
            o.uid,
            o.gid,
            o.mode & 0o777
        )),
        (Some((uid, gid)), None) => text.push_str(&format!("      Vyrtel runs as uid {uid} (gid {gid}).\n")),
        (None, Some(o)) => text.push_str(&format!(
            "      {dir} is owned by uid {} (gid {}) with mode {:03o}.\n",
            o.uid,
            o.gid,
            o.mode & 0o777
        )),
        (None, None) => {}
    }
    if read_only {
        text.push_str(&format!(
            "      {dir} is on a read-only file system or mount: mount it read-write (no `:ro`).\n      \
             See docs/configuration.md#docker-and-bind-mounts.\n"
        ));
        return text;
    }
    text.push_str(&format!(
        "      The Docker image runs as uid/gid {IMAGE_UID}. Named volumes get that owner automatically;\n      \
         for a bind mount, give the host directory to that user first, e.g.\n        \
         sudo chown -R {IMAGE_UID}:{IMAGE_UID} ./vyrtel-data\n      \
         See docs/configuration.md#docker-and-bind-mounts.\n"
    ));
    text
}

/// `dir`, or its nearest existing ancestor when it does not exist yet (the
/// one Vyrtel failed to create it in), with that directory's owner.
fn owner_of(dir: &Path) -> (std::path::PathBuf, Option<Owner>) {
    let mut at = Some(dir);
    while let Some(d) = at {
        if let Ok(meta) = std::fs::metadata(d) {
            return (d.to_path_buf(), owner_from(&meta));
        }
        at = d.parent().filter(|p| !p.as_os_str().is_empty());
    }
    (dir.to_path_buf(), None)
}

#[cfg(unix)]
fn owner_from(meta: &std::fs::Metadata) -> Option<Owner> {
    use std::os::unix::fs::MetadataExt;
    Some(Owner { uid: meta.uid(), gid: meta.gid(), mode: meta.mode() })
}

#[cfg(not(unix))]
fn owner_from(_: &std::fs::Metadata) -> Option<Owner> {
    None
}

/// Effective uid and gid of this process, from `/proc` (Linux only, which is
/// where the image runs; no libc binding needed).
fn process_ids() -> Option<(u32, u32)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    parse_ids(&status)
}

/// The effective ids (second column) of the `Uid:` and `Gid:` lines of
/// `/proc/<pid>/status`.
fn parse_ids(status: &str) -> Option<(u32, u32)> {
    let field = |name: &str| {
        let line = status.lines().find(|l| l.starts_with(name))?;
        line[name.len()..].split_whitespace().nth(1)?.parse().ok()
    };
    Some((field("Uid:")?, field("Gid:")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_errors_are_recognised_on_every_platform() {
        assert!(is_access_error("creating /data/wal: Permission denied (os error 13)"));
        assert!(is_access_error("Access is denied. (os error 5)"));
        assert!(is_access_error("opening /data/.lock: Read-only file system (os error 30)"));
        assert!(!is_access_error("data directory /data is incomplete: metadata/metadata.db is missing"));
        assert!(!is_access_error("cannot listen on 0.0.0.0:8080: Address in use"));
    }

    #[test]
    fn the_hint_names_the_directory_both_owners_and_the_fix() {
        let owner = Owner { uid: 0, gid: 0, mode: 0o40755 };
        let text = hint_text(Path::new("/data"), Some((65532, 65532)), Some(owner), false);
        assert!(text.starts_with("hint: the data directory /data must be writable"), "{text}");
        assert!(
            text.contains("Vyrtel runs as uid 65532 (gid 65532); /data is owned by uid 0 (gid 0) with mode 755."),
            "{text}"
        );
        assert!(text.contains("The Docker image runs as uid/gid 65532."), "{text}");
        assert!(text.contains("sudo chown -R 65532:65532 ./vyrtel-data"), "{text}");
        assert!(text.contains("docs/configuration.md#docker-and-bind-mounts"), "{text}");
        // Continuation lines are indented under "hint:"; no runs of spaces mid-sentence.
        assert!(text.lines().skip(1).all(|l| l.starts_with("      ")), "{text}");
        assert!(text.lines().all(|l| !l.trim_start().contains("   ")), "{text}");
    }

    #[test]
    fn unknown_ids_are_left_out_not_guessed() {
        let text = hint_text(Path::new("C:\\vyrtel"), None, None, false);
        assert!(!text.contains("Vyrtel runs as"), "{text}");
        assert!(!text.contains("owned by"), "{text}");
        assert!(text.contains("uid/gid 65532"), "{text}");
        let only_owner = hint_text(Path::new("/data"), None, Some(Owner { uid: 1000, gid: 1000, mode: 0o700 }), false);
        assert!(only_owner.contains("/data is owned by uid 1000 (gid 1000) with mode 700."), "{only_owner}");
        let only_process = hint_text(Path::new("/data"), Some((1001, 118)), None, false);
        assert!(only_process.contains("Vyrtel runs as uid 1001 (gid 118)."), "{only_process}");
    }

    #[test]
    fn a_read_only_mount_asks_for_read_write_not_chown() {
        let owner = Owner { uid: 65532, gid: 65532, mode: 0o40755 };
        let text = hint_text(Path::new("/data"), Some((65532, 65532)), Some(owner), true);
        assert!(
            text.contains("/data is on a read-only file system or mount: mount it read-write (no `:ro`)."),
            "{text}"
        );
        assert!(!text.contains("chown"), "{text}");
        assert!(text.contains("docs/configuration.md#docker-and-bind-mounts"), "{text}");
    }

    #[test]
    fn effective_ids_come_from_the_second_column() {
        let status = "Name:\tvyrtel\nUid:\t1000\t65532\t65532\t65532\nGid:\t1000\t65533\t65533\t65533\n";
        assert_eq!(parse_ids(status), Some((65532, 65533)));
        assert_eq!(parse_ids("Name:\tvyrtel\n"), None);
    }

    #[test]
    fn a_missing_directory_is_described_by_its_nearest_existing_parent() {
        let parent = tempfile::tempdir().unwrap();
        let missing = parent.path().join("not").join("yet");
        let (shown, owner) = owner_of(&missing);
        assert_eq!(shown, parent.path());
        assert_eq!(owner.is_some(), cfg!(unix));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn this_process_ids_are_read_on_linux() {
        assert!(process_ids().is_some());
    }
}

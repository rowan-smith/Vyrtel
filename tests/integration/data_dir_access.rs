//! REL-09: a data directory Vyrtel cannot write, such as a Linux bind mount
//! still owned by root, stops the server with an explanation of the
//! documented uid requirement instead of a bare "Permission denied". The
//! Docker image itself is checked the same way by
//! `scripts/release/release.mjs verify-image` (docs/releasing.md).

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

/// Runs the real binary on `dir` and returns once it exits.
fn serve(dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vyrtel"))
        .args(["--data-dir", dir.to_str().unwrap(), "--bind", "127.0.0.1:0", "--log-level", "warn"])
        .output()
        .unwrap()
}

/// Whether this process can create files in `dir` despite its mode (root can).
fn writable(dir: &Path) -> bool {
    let probe = dir.join(".probe");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(probe);
    ok
}

fn assert_hint(stderr: &str, shown: &Path) {
    let shown = shown.display();
    assert!(stderr.to_ascii_lowercase().contains("permission denied"), "{stderr}");
    assert!(stderr.contains(&format!("hint: the data directory {shown} must be writable")), "{stderr}");
    assert!(stderr.contains("The Docker image runs as uid/gid 65532."), "{stderr}");
    assert!(stderr.contains("sudo chown -R 65532:65532"), "{stderr}");
    assert!(stderr.contains("docs/configuration.md#docker-and-bind-mounts"), "{stderr}");
    #[cfg(target_os = "linux")]
    assert!(stderr.contains("Vyrtel runs as uid "), "{stderr}");
    assert!(stderr.contains(&format!("{shown} is owned by uid ")), "{stderr}");
}

#[test]
fn an_unwritable_data_directory_explains_the_uid_requirement() {
    let parent = tempfile::tempdir().unwrap();
    let dir = parent.path().join("data");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    if writable(&dir) {
        eprintln!("skipped: running as root, which can write a read-only directory");
        return;
    }
    let out = serve(&dir);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_hint(&stderr, &dir);
    assert!(stderr.contains("with mode 555."), "{stderr}");
    // Nothing was created in the directory it could not write.
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

/// A data directory that does not exist yet, under a parent Vyrtel cannot
/// write: the hint describes the parent, where the directory was refused.
#[test]
fn an_unwritable_parent_is_named_when_the_directory_is_missing() {
    let parent = tempfile::tempdir().unwrap();
    std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    if writable(parent.path()) {
        eprintln!("skipped: running as root, which can write a read-only directory");
        return;
    }
    let out = serve(&parent.path().join("data"));
    std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_hint(&String::from_utf8_lossy(&out.stderr), parent.path());
}

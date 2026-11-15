//! Password checks give their Argon2id work area back (crates/server/src/password.rs).
//!
//! With the work area from glibc's malloc, checks on different threads each
//! left a 19 MiB block resident: 0.1.2-rc.4 held about 111 MiB after a burst
//! of concurrent sign-ins on an empty server. Alone in this test binary, so
//! no other test moves the process's resident memory while it is measured.
#![cfg(target_os = "linux")]

use std::thread;

fn rss_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .expect("VmRSS in /proc/self/status")
}

#[test]
fn concurrent_checks_on_many_threads_retain_no_work_area() {
    let hash = server::password::hash_password("correct horse").unwrap();
    // Settle the allocator and runtime with one check first.
    assert!(server::password::verify_password("correct horse", &hash));
    let before = rss_kib();

    // 16 threads at once, 4 rounds, right and wrong passwords: as many
    // threads (and so malloc arenas) as a burst of sign-ins can touch.
    for _ in 0..4 {
        let handles: Vec<_> = (0..16)
            .map(|i| {
                let hash = hash.clone();
                thread::spawn(move || {
                    let pw = if i % 2 == 0 { "correct horse" } else { "wrong" };
                    assert_eq!(server::password::verify_password(pw, &hash), i % 2 == 0);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }

    let grown = rss_kib().saturating_sub(before);
    // One retained work area alone would be 19 MiB; allow thread stacks and
    // allocator bookkeeping, nothing like a work area.
    assert!(grown < 8 * 1024, "resident memory grew {grown} KiB after 64 checks");
}

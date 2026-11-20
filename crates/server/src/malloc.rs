//! The number of glibc malloc arenas.
//!
//! glibc gives each thread that allocates its own arena until there are
//! eight per CPU it can see (the host's CPUs, not a container's quota), and
//! an arena keeps the memory its thread freed for that thread's later
//! allocations rather than returning it. Tokio's blocking pool grows by one
//! thread whenever every existing one is busy, which under steady load
//! happens at an unpredictable moment, and each new pool thread's arena
//! then settles at the working set of the queries and ingest batches it
//! runs: the 0.1.2 30 minute transport runs saw RSS step up 11–12 MiB
//! within 30 s of the 15th thread starting, and the step alone decided
//! whether the run met its RSS growth budget.
//!
//! So at startup, before any other thread exists, the server caps the
//! arenas at two per CPU it may use (`available_parallelism`, which follows
//! a cgroup CPU quota). Threads beyond that share the existing arenas, and
//! memory one thread frees can be reused by the others, so RSS no longer
//! depends on how many blocking threads the pool happened to reach. Small
//! allocations mostly come from glibc's per-thread cache and take no arena
//! lock; with at least two arenas per CPU, threads that run at the same
//! time rarely wait for one.
//!
//! `MALLOC_ARENA_MAX` in the environment still wins: glibc has applied it
//! already, and the server leaves it alone. Other allocators and platforms
//! are unaffected.

/// Arenas allowed per CPU.
pub const ARENAS_PER_CPU: usize = 2;

/// What [`cap_arenas`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arenas {
    /// Capped at this many.
    Capped(usize),
    /// `MALLOC_ARENA_MAX` was set, so glibc's own setting stands.
    FromEnvironment,
    /// `mallopt` refused the value.
    Refused(usize),
}

/// The cap for `cpus` CPUs: two per CPU, at least two.
pub fn cap_for(cpus: usize) -> usize {
    (cpus * ARENAS_PER_CPU).max(2)
}

/// Cap glibc's malloc arenas (see the module documentation). Call it first
/// thing in `main`, before any thread is started: arenas that exist already
/// stay. `None` where the allocator is not glibc's.
pub fn cap_arenas() -> Option<Arenas> {
    imp::cap_arenas()
}

/// Log what [`cap_arenas`] did, once logging is set up.
pub fn report(result: Option<Arenas>) {
    match result {
        None => {}
        Some(Arenas::Capped(n)) => tracing::debug!(arenas = n, "capped malloc arenas"),
        Some(Arenas::FromEnvironment) => tracing::debug!("malloc arenas set by MALLOC_ARENA_MAX"),
        Some(Arenas::Refused(n)) => tracing::warn!(arenas = n, "could not cap malloc arenas"),
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod imp {
    use super::{Arenas, cap_for};

    pub fn cap_arenas() -> Option<Arenas> {
        if std::env::var_os("MALLOC_ARENA_MAX").is_some() {
            return Some(Arenas::FromEnvironment);
        }
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        let n = cap_for(cpus);
        let value = libc::c_int::try_from(n).unwrap_or(libc::c_int::MAX);
        // SAFETY: mallopt takes two integers and changes only allocator
        // settings; M_ARENA_MAX takes effect for threads that have not
        // allocated yet.
        if unsafe { libc::mallopt(libc::M_ARENA_MAX, value) } == 1 {
            Some(Arenas::Capped(n))
        } else {
            Some(Arenas::Refused(n))
        }
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
mod imp {
    pub fn cap_arenas() -> Option<super::Arenas> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_arenas_per_cpu_and_never_fewer_than_two() {
        assert_eq!(cap_for(0), 2);
        assert_eq!(cap_for(1), 2);
        assert_eq!(cap_for(2), 4);
        assert_eq!(cap_for(8), 16);
    }
}

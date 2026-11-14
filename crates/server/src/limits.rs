//! The process's open-file limit.
//!
//! Vyrtel needs a file descriptor per connection (`max_connections` plus 64
//! waiting places), about 25 for the listener, runtime, WAL and metadata
//! database, and at most [`storage::segment::MAX_OPEN_SEGMENT_FILES`] for
//! segment files. Many systems start processes with a soft limit of 1024
//! and a much higher hard limit, so at startup the soft limit is raised to
//! the hard one, as servers usually do. If even the hard limit is below what
//! the configuration needs, a warning says so; running out shows as refused
//! connections (`acceptErrors`), not as lost data.

/// Descriptors the configuration needs, besides the margin for clients of
/// the operating system itself.
pub fn needed(max_connections: usize) -> u64 {
    (max_connections + 64 + 25 + storage::segment::MAX_OPEN_SEGMENT_FILES) as u64
}

/// The open-file limit after [`raise`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFiles {
    pub soft: u64,
    pub hard: u64,
    /// The soft limit before it was raised, when it was.
    pub raised_from: Option<u64>,
}

/// Raise the soft open-file limit to the hard limit (at most 1,048,576).
/// `None` where the platform has no such limit (Windows).
pub fn raise() -> Option<std::io::Result<OpenFiles>> {
    imp::raise()
}

/// Log the limit, and warn when it is below what the configuration needs.
pub fn report(result: Option<std::io::Result<OpenFiles>>, max_connections: usize) {
    let need = needed(max_connections);
    match result {
        None => {}
        Some(Err(e)) => tracing::warn!(error = %e, "could not read or raise the open-file limit"),
        Some(Ok(l)) => {
            match l.raised_from {
                Some(from) => tracing::info!(from, to = l.soft, hard = l.hard, "raised the open-file limit"),
                None => tracing::debug!(soft = l.soft, hard = l.hard, "open-file limit"),
            }
            if l.soft < need {
                tracing::warn!(
                    limit = l.soft,
                    needed = need,
                    max_connections,
                    "the open-file limit is below what this configuration needs; connections beyond it are \
                     refused. Raise it (systemd LimitNOFILE=, Docker --ulimit nofile=, ulimit -n) or lower \
                     server.max_connections"
                );
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    use super::OpenFiles;
    use std::io;

    /// Highest soft limit asked for, even when the hard limit is unlimited.
    const CEILING: u64 = 1 << 20;

    fn get() -> io::Result<libc::rlimit> {
        let mut l = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        // SAFETY: getrlimit only writes the struct it is given.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut l) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(l)
    }

    fn set(soft: libc::rlim_t, hard: libc::rlim_t) -> io::Result<()> {
        let l = libc::rlimit { rlim_cur: soft, rlim_max: hard };
        // SAFETY: setrlimit only reads the struct it is given.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &l) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn raise() -> Option<io::Result<OpenFiles>> {
        Some((|| {
            let l = get()?;
            let (soft, hard) = (l.rlim_cur as u64, l.rlim_max as u64);
            let target = hard.min(CEILING);
            if soft >= target {
                return Ok(OpenFiles { soft, hard, raised_from: None });
            }
            // macOS refuses a soft limit above kern.maxfilesperproc even when
            // the hard limit allows it; OPEN_MAX (10240) is accepted there.
            let tried = set(target as libc::rlim_t, l.rlim_max).map(|()| target).or_else(|e| {
                if target > 10_240 && soft < 10_240 { set(10_240, l.rlim_max).map(|()| 10_240) } else { Err(e) }
            })?;
            Ok(OpenFiles { soft: tried, hard, raised_from: Some(soft) })
        })())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_soft_limit_ends_at_the_hard_limit_or_the_ceiling() {
            let before = get().unwrap();
            let l = raise().unwrap().unwrap();
            let after = get().unwrap();
            assert_eq!(after.rlim_cur as u64, l.soft);
            assert!(l.soft >= before.rlim_cur as u64, "never lowered");
            assert!(l.soft <= l.hard.min(CEILING) || l.raised_from.is_none());
            // A second call has nothing left to raise.
            assert_eq!(raise().unwrap().unwrap().raised_from, None);
        }
    }
}

#[cfg(not(unix))]
mod imp {
    pub fn raise() -> Option<std::io::Result<super::OpenFiles>> {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_default_configuration_needs_about_410() {
        assert_eq!(super::needed(256), 256 + 64 + 25 + 64);
    }
}

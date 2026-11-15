//! Argon2id password hashing whose working memory is returned to the
//! operating system after every check.
//!
//! An Argon2id check fills its whole working memory (19 MiB with the default
//! parameters). Through the `argon2` crate's own API that memory comes from
//! the global allocator, and glibc keeps freed blocks of that size: after the
//! first one is freed it raises its mmap threshold, so later work areas come
//! from malloc arenas and stay resident, one per arena that ran a check.
//! Sign-ins spread over the blocking pool's threads, so concurrent attempts
//! (right or wrong, which needs no account) left over 95 MiB resident in an
//! otherwise empty server and never gave it back.
//!
//! So the work area is mapped for each check and unmapped when it ends
//! ([`Workspace`]); everything else keeps the process's allocator. Hashing
//! and verification are otherwise what `argon2` and `password-hash` do:
//! the same PHC string format and parameters, and verification recomputes
//! the hash with the stored algorithm, version, parameters and salt and
//! compares outputs in constant time ([`Output`]'s `PartialEq`).

use argon2::password_hash::phc::{Output, ParamsString, PasswordHash, Salt};
use argon2::{Algorithm, Argon2, Block, Params, Version};

/// Hash `password` with Argon2id and the default parameters, as a PHC string.
pub fn hash_password(password: &str) -> Result<String, String> {
    let params = Params::default();
    let salt = argon2::password_hash::generate_salt();
    let salt = Salt::new(&salt).map_err(|e| e.to_string())?;
    let mut out = [0u8; Params::DEFAULT_OUTPUT_LEN];
    compute(Algorithm::Argon2id, Version::default(), params.clone(), password.as_bytes(), &salt, &mut out)?;
    let hash = PasswordHash {
        algorithm: Algorithm::Argon2id.ident(),
        version: Some(Version::default().into()),
        params: ParamsString::try_from(&params).map_err(|e| e.to_string())?,
        salt: Some(salt),
        hash: Some(Output::new(&out).map_err(|e| e.to_string())?),
    };
    Ok(hash.to_string())
}

/// Whether `password` matches the PHC string `hash`. False for any hash
/// that does not parse or is not Argon2.
pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(hash) = PasswordHash::new(hash) else { return false };
    let (Some(salt), Some(expected)) = (&hash.salt, &hash.hash) else { return false };
    let Ok(algorithm) = Algorithm::try_from(hash.algorithm.as_str()) else { return false };
    let version = match hash.version.map(Version::try_from).transpose() {
        Ok(v) => v.unwrap_or_default(),
        Err(_) => return false,
    };
    // Includes the output length, from the stored hash.
    let Ok(params) = Params::try_from(&hash) else { return false };
    let mut buf = [0u8; Output::MAX_LENGTH];
    let Some(out) = buf.get_mut(..expected.len()) else { return false };
    if compute(algorithm, version, params, password.as_bytes(), salt, out).is_err() {
        return false;
    }
    // Constant time: see `Output`.
    Output::new(out).is_ok_and(|computed| computed == *expected)
}

fn compute(
    algorithm: Algorithm,
    version: Version,
    params: Params,
    password: &[u8],
    salt: &[u8],
    out: &mut [u8],
) -> Result<(), String> {
    let mut work = Workspace::new(params.block_count()).map_err(|e| format!("password work area: {e}"))?;
    Argon2::new(algorithm, version, params)
        .hash_password_into_with_memory(password, salt, out, work.blocks())
        .map_err(|e| e.to_string())
}

/// An Argon2 work area: anonymous memory mapped for one check and unmapped
/// on drop (Unix), or from the global allocator elsewhere, where large
/// allocations are not kept after they are freed in the same way.
struct Workspace {
    #[cfg(unix)]
    map: std::ptr::NonNull<Block>,
    #[cfg(not(unix))]
    heap: Vec<Block>,
    len: usize,
}

// The mapping is owned by the workspace alone, like a `Vec<Block>`.
unsafe impl Send for Workspace {}

impl Workspace {
    #[cfg(unix)]
    fn new(len: usize) -> std::io::Result<Self> {
        let bytes = len
            .checked_mul(Block::SIZE)
            .filter(|&b| b > 0)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad block count"))?;
        // SAFETY: a fresh private anonymous mapping; no existing memory is touched.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        let map = std::ptr::NonNull::new(p.cast::<Block>()).ok_or_else(std::io::Error::last_os_error)?;
        Ok(Self { map, len })
    }

    #[cfg(not(unix))]
    fn new(len: usize) -> std::io::Result<Self> {
        Ok(Self { heap: vec![Block::new(); len], len })
    }

    fn blocks(&mut self) -> &mut [Block] {
        #[cfg(unix)]
        // SAFETY: the mapping is `len` blocks long, page aligned (Block needs
        // 64), zero filled (a valid `[u64; 128]` per block) and exclusively
        // borrowed through `&mut self` until it is unmapped in `drop`.
        return unsafe { std::slice::from_raw_parts_mut(self.map.as_ptr(), self.len) };
        #[cfg(not(unix))]
        return &mut self.heap[..self.len];
    }
}

#[cfg(unix)]
impl Drop for Workspace {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the mapping made in `new`; no borrow of it
        // outlives `self`.
        unsafe { libc::munmap(self.map.as_ptr().cast(), self.len * Block::SIZE) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argon2::PasswordVerifier;
    use argon2::password_hash::PasswordHasher;

    #[test]
    fn hashes_are_default_argon2id() {
        let h = hash_password("hunter2").unwrap();
        let parsed = PasswordHash::new(&h).unwrap();
        assert_eq!(parsed.algorithm, Algorithm::Argon2id.ident());
        assert_eq!(parsed.version, Some(Version::V0x13.into()));
        assert_eq!(Params::try_from(&parsed).unwrap().m_cost(), Params::DEFAULT_M_COST);
        assert_eq!(Params::try_from(&parsed).unwrap().t_cost(), Params::DEFAULT_T_COST);
        assert_eq!(Params::try_from(&parsed).unwrap().p_cost(), Params::DEFAULT_P_COST);
        assert_eq!(parsed.hash.unwrap().len(), Params::DEFAULT_OUTPUT_LEN);
        assert!(verify_password("hunter2", &h));
        assert!(!verify_password("hunter3", &h));
        assert!(!verify_password("", &h));
    }

    #[test]
    fn compatible_with_the_argon2_crate_both_ways() {
        // Hashes stored by 0.1.1 and 0.1.2-rc.1 to rc.4 came from `Argon2::default()`.
        let theirs = Argon2::default().hash_password(b"correct horse").unwrap().to_string();
        assert!(verify_password("correct horse", &theirs));
        assert!(!verify_password("correct horsf", &theirs));
        let ours = hash_password("correct horse").unwrap();
        assert!(Argon2::default().verify_password(b"correct horse", ours.as_str()).is_ok());
        assert!(Argon2::default().verify_password(b"wrong", ours.as_str()).is_err());
    }

    #[test]
    fn stored_parameters_are_honoured() {
        // Not the defaults: the verifier must use what the hash records.
        for (alg, version) in [
            (Algorithm::Argon2id, Version::V0x13),
            (Algorithm::Argon2i, Version::V0x13),
            (Algorithm::Argon2d, Version::V0x10),
        ] {
            let params = Params::new(2048, 3, 2, Some(24)).unwrap();
            let h = Argon2::new(alg, version, params).hash_password(b"pw").unwrap().to_string();
            assert!(verify_password("pw", &h), "{h}");
            assert!(!verify_password("pX", &h), "{h}");
        }
    }

    #[test]
    fn known_vector() {
        // RFC 9106-style fixed input, computed by the argon2 crate's own path:
        // the mapped work area must give the same output.
        let params = Params::new(256, 2, 1, Some(32)).unwrap();
        let salt = Salt::new(b"somesaltsomesalt").unwrap();
        let mut ours = [0u8; 32];
        compute(Algorithm::Argon2id, Version::V0x13, params.clone(), b"password", &salt, &mut ours).unwrap();
        let mut theirs = [0u8; 32];
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(b"password", b"somesaltsomesalt", &mut theirs)
            .unwrap();
        assert_eq!(ours, theirs);
    }

    #[test]
    fn malformed_or_foreign_hashes_never_match() {
        let good = hash_password("pw").unwrap();
        for bad in [
            "",
            "not a hash",
            "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ",    // no output
            "$scrypt$ln=16,r=8,p=1$c2FsdHNhbHQ$aGFzaGhhc2g", // another algorithm
            "$argon2id$v=7$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g", // unknown version
            "$argon2id$v=19$m=1,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g", // invalid memory cost
        ] {
            assert!(!verify_password("pw", bad), "{bad}");
        }
        // A truncated or altered output does not match either.
        let truncated = &good[..good.len() - 4];
        assert!(!verify_password("pw", truncated));
    }
}

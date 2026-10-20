//! The identity of the on-disk layout this build reads and writes.
//!
//! Every file Vyrtel persists in a data directory carries it: the manifest
//! (`vyrtel.json`), each WAL and segment header and the metadata database.
//! A file with any other identity, or none, is refused before anything in
//! the directory changes. Format version numbers say nothing on their own:
//! before 1.0 each release that changes a layout resets them to 1, so only
//! the identity tells two layouts both labelled version 1 apart.
//!
//! The identity names the release that introduced the layout. A release
//! that changes no layout keeps it; one that changes any layout sets it to
//! its own version (see docs/releasing.md and docs/storage-format.md).
//! It is an exact match, never an ordering: no build reads another layout.

/// The layout identity, as written in `vyrtel.json` and the metadata
/// database.
pub const LAYOUT: &str = "0.1.2";

/// [`LAYOUT`] as stored in binary file headers: its ASCII bytes, padded
/// with zeros to 8 bytes.
pub const LAYOUT_TAG: [u8; 8] = tag(LAYOUT);

/// Bytes a binary header uses for the identity.
pub const LAYOUT_TAG_LEN: usize = 8;

const fn tag(s: &str) -> [u8; 8] {
    let b = s.as_bytes();
    assert!(!b.is_empty() && b.len() <= 8, "a layout identity is 1 to 8 ASCII bytes");
    let mut out = [0u8; 8];
    let mut i = 0;
    while i < b.len() {
        assert!(b[i].is_ascii_graphic(), "a layout identity is printable ASCII");
        out[i] = b[i];
        i += 1;
    }
    out
}

/// How an identity found in a binary header reads in an error message:
/// `layout 0.1.3`, `no layout identity` (all zeros, or too short to hold
/// one), or its bytes in hex.
pub fn describe_tag(found: &[u8]) -> String {
    if found.len() < LAYOUT_TAG_LEN || found.iter().all(|&b| b == 0) {
        return "no layout identity".into();
    }
    let found = &found[..LAYOUT_TAG_LEN];
    let end = found.iter().position(|&b| b == 0).unwrap_or(LAYOUT_TAG_LEN);
    let (text, pad) = found.split_at(end);
    if text.iter().all(u8::is_ascii_graphic) && pad.iter().all(|&b| b == 0) {
        format!("layout {}", String::from_utf8_lossy(text))
    } else {
        format!("no layout identity (bytes {})", found.iter().map(|b| format!("{b:02x}")).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tag_is_the_identity_padded() {
        assert_eq!(&LAYOUT_TAG[..LAYOUT.len()], LAYOUT.as_bytes());
        assert!(LAYOUT_TAG[LAYOUT.len()..].iter().all(|&b| b == 0));
        assert_eq!(describe_tag(&LAYOUT_TAG), format!("layout {LAYOUT}"));
    }

    #[test]
    fn foreign_tags_are_described() {
        assert_eq!(describe_tag(&[0; 8]), "no layout identity");
        assert_eq!(describe_tag(b"0.1"), "no layout identity");
        assert_eq!(describe_tag(b"0.1.3\0\0\0"), "layout 0.1.3");
        // A 0.1.1 WAL header: format 1, signal 1, then reserved zeros.
        assert_eq!(describe_tag(&[1, 0, 1, 0, 0, 0, 0, 0]), "no layout identity (bytes 0100010000000000)");
    }
}

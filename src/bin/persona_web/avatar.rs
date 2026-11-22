//! Profile pictures: `avatar.*` in a persona folder, `me.*` in the store root. The type comes from
//! the file's magic bytes, never from what the client claims.

use persona_core::{Error, Result};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const MAX_BYTES: usize = 8 << 20;
const TYPES: [(&str, &str); 4] = [("png", "image/png"), ("jpg", "image/jpeg"), ("webp", "image/webp"), ("gif", "image/gif")];

fn sniff(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if b.starts_with(b"GIF8") {
        Some("gif")
    } else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// (path, content type, version): the version is the mtime in ms, for cache-busting URLs.
pub fn find(dir: &Path, stem: &str) -> Option<(PathBuf, &'static str, u64)> {
    TYPES.iter().find_map(|(ext, ctype)| {
        let p = dir.join(format!("{stem}.{ext}"));
        let ver = std::fs::metadata(&p).ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64;
        Some((p, *ctype, ver))
    })
}

pub fn version(dir: &Path, stem: &str) -> Option<u64> {
    find(dir, stem).map(|a| a.2)
}

pub fn delete(dir: &Path, stem: &str) {
    for (ext, _) in TYPES {
        let _ = std::fs::remove_file(dir.join(format!("{stem}.{ext}")));
    }
}

pub fn save(dir: &Path, stem: &str, bytes: &[u8]) -> Result<u64> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::Invalid("image larger than 8 MB".into()));
    }
    let ext = sniff(bytes).ok_or_else(|| Error::Invalid("not a PNG, JPEG, GIF or WebP image".into()))?;
    delete(dir, stem);
    std::fs::create_dir_all(dir).map_err(|e| Error::Io(e.to_string()))?;
    std::fs::write(dir.join(format!("{stem}.{ext}")), bytes).map_err(|e| Error::Io(e.to_string()))?;
    Ok(version(dir, stem).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffing() {
        assert_eq!(sniff(b"\x89PNG\r\n"), Some("png"));
        assert_eq!(sniff(b"\xFF\xD8\xFF\xE0"), Some("jpg"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(sniff(b"<script>"), None);
    }
}

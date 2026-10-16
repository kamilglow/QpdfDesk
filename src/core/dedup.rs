//! Duplicate detection by full path.
//!
//! Each path is normalized with `std::fs::canonicalize` before comparison, so
//! the same file reached via different spellings is counted once. Dedup is
//! applied AFTER the PDF filter, otherwise skipped files would be miscounted
//! as duplicates.
//!
//! Canonicalizing costs a file-system call per path, so for a big folder the
//! keys are worked out on a background thread ([`canonical_key`]) and only the
//! set lookups happen on the UI thread (`FileList`). The key is kept with the
//! list entry and used again to forget it: by then the file may be gone, and
//! canonicalizing again would give a different answer.

use std::path::{Path, PathBuf};

/// Canonical comparison key for a path. Falls back to the path as-is when it
/// cannot be canonicalized (e.g. it no longer exists) rather than failing.
pub fn canonical_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_spellings_of_one_file_are_one_key() {
        let dir = std::env::temp_dir();
        let file = dir.join(format!("qd_dedup_{}.pdf", std::process::id()));
        std::fs::write(&file, b"x").expect("write");
        let other_spelling = dir.join(".").join(file.file_name().expect("name"));
        assert_eq!(canonical_key(&file), canonical_key(&other_spelling));
        assert_ne!(canonical_key(&file), canonical_key(&dir.join("other.pdf")));
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn a_missing_file_keeps_its_spelling() {
        let path = Path::new(r"C:\no\such\file.pdf");
        assert_eq!(canonical_key(path), path);
    }
}

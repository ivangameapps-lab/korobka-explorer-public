//! Windows long-path helpers.
//!
//! Win32 has a 260-character limit for normal paths. Prefixing an absolute
//! path with `\\?\` (or `\\?\UNC\` for network paths) lifts that limit.
//! The UI keeps showing normal paths; only OS calls receive the extended form.

use std::path::{Path, PathBuf};

#[cfg(windows)]
pub(crate) fn extended(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text.starts_with(r"\\?\") {
        return path.to_path_buf();
    }
    if let Some(rest) = text.strip_prefix(r"\\") {
        return PathBuf::from(format!(r"\\?\UNC\{rest}"));
    }
    if path.is_absolute() {
        PathBuf::from(format!(r"\\?\{text}"))
    } else {
        path.to_path_buf()
    }
}

#[cfg(not(windows))]
pub(crate) fn extended(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// Human-readable path without the `\\?\` prefix.
pub(crate) fn display(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        text.into_owned()
    }
}

/// Normalizes a path returned by an extended Win32 call for UI/PathBuf use.
pub(crate) fn normalized(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn extends_drive_and_unc_paths() {
        assert_eq!(extended(Path::new(r"C:\folder\file.txt")), PathBuf::from(r"\\?\C:\folder\file.txt"));
        assert_eq!(
            extended(Path::new(r"\\server\share\file.txt")),
            PathBuf::from(r"\\?\UNC\server\share\file.txt")
        );
        assert_eq!(
            extended(Path::new(r"\\?\C:\already\long")),
            PathBuf::from(r"\\?\C:\already\long")
        );
        assert_eq!(display(Path::new(r"\\?\C:\folder")), r"C:\folder");
        assert_eq!(display(Path::new(r"\\?\UNC\server\share")), r"\\server\share");
    }
}

use std::{
    collections::{hash_map::DefaultHasher, BTreeMap},
    fs,
    hash::{Hash, Hasher},
    io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub inner: String,
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub index: Option<usize>,
}

pub fn is_zip(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
}

pub fn list(zip_path: &Path, inner: &str) -> Result<Vec<Entry>, String> {
    let file = fs::File::open(crate::path_util::extended(zip_path))
        .map_err(|error| error.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    let inner = normalize_inner(inner);
    let mut entries: BTreeMap<String, Entry> = BTreeMap::new();

    for index in 0..zip.len() {
        let item = zip.by_index_raw(index).map_err(|error| error.to_string())?;
        let path = item.name().replace('\\', "/");
        let path = path.trim_start_matches('/');
        let Some(rest) = path.strip_prefix(inner.as_str()) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let mut parts = rest.split('/');
        let Some(name) = parts.next().filter(|name| !name.is_empty()) else {
            continue;
        };
        let nested = parts.next().is_some();
        let is_dir = nested || item.is_dir();
        let entry_inner = format!("{inner}{name}");
        if is_dir {
            entries.insert(
                name.to_owned(),
                Entry {
                    inner: entry_inner,
                    name: name.to_owned(),
                    is_dir: true,
                    size: None,
                    index: None,
                },
            );
        } else {
            entries.entry(name.to_owned()).or_insert_with(|| Entry {
                inner: entry_inner,
                name: name.to_owned(),
                is_dir: false,
                size: Some(item.size()),
                index: Some(index),
            });
        }
    }

    let mut entries: Vec<Entry> = entries.into_values().collect();
    entries.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    Ok(entries)
}

pub fn extract_entry(zip_path: &Path, index: usize, destination: &Path) -> Result<PathBuf, String> {
    let file = fs::File::open(crate::path_util::extended(zip_path))
        .map_err(|error| error.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    let mut item = zip.by_index(index).map_err(|error| error.to_string())?;
    let relative = safe_relative(item.name()).ok_or_else(|| {
        if crate::i18n::english() {
            format!("Unsafe path in archive: {}", item.name())
        } else {
            format!("Небезопасный путь в архиве: {}", item.name())
        }
    })?;
    let target = destination.join(relative);
    if item.is_dir() {
        fs::create_dir_all(crate::path_util::extended(&target))
            .map_err(|error| error.to_string())?;
        return Ok(target);
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(crate::path_util::extended(parent))
            .map_err(|error| error.to_string())?;
    }
    let mut output = fs::File::create(crate::path_util::extended(&target))
        .map_err(|error| error.to_string())?;
    io::copy(&mut item, &mut output).map_err(|error| error.to_string())?;
    Ok(target)
}

pub fn extract_all(zip_path: &Path, destination: &Path) -> Result<usize, String> {
    let file = fs::File::open(crate::path_util::extended(zip_path))
        .map_err(|error| error.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    fs::create_dir_all(crate::path_util::extended(destination))
        .map_err(|error| error.to_string())?;
    let mut count = 0;
    for index in 0..zip.len() {
        let mut item = zip.by_index(index).map_err(|error| error.to_string())?;
        let Some(relative) = safe_relative(item.name()) else {
            return Err(if crate::i18n::english() {
                format!("Unsafe path in archive: {}", item.name())
            } else {
                format!("Небезопасный путь в архиве: {}", item.name())
            });
        };
        let target = destination.join(relative);
        if item.is_dir() {
            fs::create_dir_all(crate::path_util::extended(&target))
                .map_err(|error| error.to_string())?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(crate::path_util::extended(parent))
                .map_err(|error| error.to_string())?;
        }
        let mut output = fs::File::create(crate::path_util::extended(&target))
            .map_err(|error| error.to_string())?;
        io::copy(&mut item, &mut output).map_err(|error| error.to_string())?;
        count += 1;
    }
    Ok(count)
}

pub fn unique_extraction_dir(zip_path: &Path) -> PathBuf {
    let parent = zip_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = zip_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| crate::i18n::tr("Архив").to_owned());
    let base = parent.join(&stem);
    if !crate::path_util::extended(&base).exists() {
        return base;
    }
    for suffix in 2_u32.. {
        let candidate = parent.join(format!("{stem} ({suffix})"));
        if !crate::path_util::extended(&candidate).exists() {
            return candidate;
        }
    }
    base
}

pub fn extract_to_temp(zip_path: &Path, index: usize) -> Result<PathBuf, String> {
    let mut hasher = DefaultHasher::new();
    zip_path.hash(&mut hasher);
    let stem = zip_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".to_owned());
    let root = std::env::temp_dir()
        .join("Korobka")
        .join("archive")
        .join(format!("{stem}-{:016x}", hasher.finish()));
    extract_entry(zip_path, index, &root)
}

fn normalize_inner(inner: &str) -> String {
    let inner = inner.replace('\\', "/");
    let inner = inner.trim_matches('/');
    if inner.is_empty() {
        String::new()
    } else {
        format!("{inner}/")
    }
}

fn safe_relative(name: &str) -> Option<PathBuf> {
    let mut path = PathBuf::new();
    for part in name.replace('\\', "/").split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.contains(':') || part.contains('\0') {
            return None;
        }
        path.push(part);
    }
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "korobka-archive-test-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let zip_path = root.join("sample.zip");
        let file = fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.add_directory("sub/", options).unwrap();
        zip.start_file("root.txt", options).unwrap();
        zip.write_all(b"root").unwrap();
        zip.start_file("sub/nested.txt", options).unwrap();
        zip.write_all(b"nested").unwrap();
        zip.finish().unwrap();
        zip_path
    }

    #[test]
    fn lists_and_extracts_nested_entries() {
        let zip_path = fixture();
        let root = list(&zip_path, "").unwrap();
        assert_eq!(root.len(), 2);
        assert!(root[0].is_dir);
        assert_eq!(root[1].name, "root.txt");

        let nested = list(&zip_path, "sub").unwrap();
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0].inner, "sub/nested.txt");
        assert_eq!(nested[0].size, Some(6));

        let destination = unique_extraction_dir(&zip_path);
        let count = extract_all(&zip_path, &destination).unwrap();
        assert_eq!(count, 2);
        assert_eq!(
            fs::read_to_string(destination.join("sub").join("nested.txt")).unwrap(),
            "nested"
        );

        let single_root = unique_extraction_dir(&zip_path);
        fs::create_dir_all(&single_root).unwrap();
        let index = nested[0].index.unwrap();
        let extracted = extract_entry(&zip_path, index, &single_root).unwrap();
        assert_eq!(fs::read_to_string(extracted).unwrap(), "nested");

        fs::remove_dir_all(zip_path.parent().unwrap()).unwrap();
    }
}

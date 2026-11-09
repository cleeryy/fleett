//! Tar/gzip plumbing shared by instance archives (`fleett export/import`) and
//! rotating backups (`fleett backup`).
//!
//! Everything is written with the `tar` crate behind a `GzEncoder`, so both
//! archive kinds are plain `.tgz` files an operator can inspect with
//! `tar -tzf`. Extraction is deliberately paranoid: entries must stay inside
//! the destination (no absolute path, no `..`), symlinks must not escape, and
//! plaintext secret material is never written back.

use flate2::write::GzEncoder;
use flate2::Compression;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};

/// Largest metadata entry (`manifest.json`, `compose.yaml`) read into memory.
pub const MAX_METADATA_BYTES: u64 = 1024 * 1024;
/// Upper bound of the decompressed size of one archive (decompression bomb
/// guard). Well above any realistic Hermes home.
pub const MAX_EXTRACTED_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Fresh gzip encoder collecting the archive in memory.
pub fn gzip_writer() -> GzEncoder<Vec<u8>> {
    GzEncoder::new(Vec::new(), Compression::default())
}

/// Append one in-memory file.
pub fn append_bytes<W: Write>(
    builder: &mut tar::Builder<W>,
    path: &Path,
    bytes: &[u8],
    mode: u32,
) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(bytes.len() as u64);
    header.set_mode(mode & 0o777);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    builder.append_data(&mut header, path, bytes)?;
    Ok(())
}

/// Append one directory entry.
pub fn append_dir<W: Write>(
    builder: &mut tar::Builder<W>,
    path: &Path,
    mode: u32,
) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_size(0);
    header.set_mode(mode & 0o777);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    builder.append_data(&mut header, path, io::empty())?;
    Ok(())
}

/// Recursively append `root` under `prefix`. Returns the number of files and
/// symlinks written (empty directories are written but not counted).
///
/// `secrets.env` and `*.key` are skipped at every level, as are sockets, FIFOs
/// and device nodes (runtime artifacts, not data).
pub fn append_tree<W: Write>(
    builder: &mut tar::Builder<W>,
    root: &Path,
    prefix: &Path,
) -> io::Result<usize> {
    append_tree_excluding(builder, root, prefix, |_| false)
}

/// Same as [`append_tree`] with an extra name filter.
///
/// `skip` receives the file name of every entry (file or directory) and
/// returns `true` to leave it out. Snapshots use it to drop runtime caches that
/// are regenerated on the next start; backups (`fleett backup`) keep the whole
/// tree and call [`append_tree`].
pub fn append_tree_excluding<W: Write, F: Fn(&OsStr) -> bool>(
    builder: &mut tar::Builder<W>,
    root: &Path,
    prefix: &Path,
    skip: F,
) -> io::Result<usize> {
    let mut count = 0;
    append_children(builder, root, prefix, &mut count, &skip)?;
    Ok(count)
}

fn append_children<W: Write, F: Fn(&OsStr) -> bool>(
    builder: &mut tar::Builder<W>,
    root: &Path,
    prefix: &Path,
    count: &mut usize,
    skip: &F,
) -> io::Result<()> {
    let mut entries = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let name = entry.file_name();
        if is_secret_name(&name) || skip(&name) {
            continue;
        }
        let path = entry.path();
        let relative = prefix.join(&name);
        let metadata = fs::symlink_metadata(&path)?;
        let file_type = metadata.file_type();

        if file_type.is_dir() {
            append_dir(builder, &relative, 0o700)?;
            append_children(builder, &path, &relative, count, skip)?;
            continue;
        }
        if file_type.is_symlink() {
            let target = fs::read_link(&path)?;
            if !safe_link_target(&target) {
                continue;
            }
            append_symlink(builder, &relative, &target)?;
            *count += 1;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        append_file(
            builder,
            &relative,
            &path,
            metadata.len(),
            metadata.permissions().mode(),
        )?;
        *count += 1;
    }
    Ok(())
}

fn append_symlink<W: Write>(
    builder: &mut tar::Builder<W>,
    path: &Path,
    target: &Path,
) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_mode(0o777);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    builder.append_link(&mut header, path, target)?;
    Ok(())
}

fn append_file<W: Write>(
    builder: &mut tar::Builder<W>,
    path: &Path,
    source: &Path,
    len: u64,
    mode: u32,
) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(len);
    header.set_mode(mode & 0o777);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    let mut file = fs::File::open(source)?;
    builder.append_data(&mut header, path, &mut file)?;
    Ok(())
}

/// True for a file name that must never leave the host: plaintext secrets and
/// private keys.
pub fn is_secret_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    lower == "secrets.env" || lower.ends_with(".key")
}

/// Reject an archive entry that could escape the extraction directory.
pub fn safe_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err("empty path".to_string());
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!("path {} contains \"..\"", path.display()));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("path {} is absolute", path.display()));
            }
        }
    }
    Ok(())
}

/// True when a symlink target stays inside the archive.
pub fn safe_link_target(target: &Path) -> bool {
    !target.as_os_str().is_empty() && safe_path(target).is_ok()
}

/// Write one extracted entry below `destination`.
///
/// The entry path is not interpreted here: callers validate it with
/// [`safe_path`] and join it themselves.
pub fn write_entry<R: Read>(mut entry: tar::Entry<'_, R>, destination: &Path) -> io::Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let entry_type = entry.header().entry_type();

    if entry_type.is_dir() {
        fs::create_dir_all(destination)?;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o700))?;
        return Ok(());
    }
    if entry_type.is_symlink() {
        let target = entry.link_name()?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "symlink entry without a target")
        })?;
        if !safe_link_target(&target) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "symlink {} points outside the archive",
                    destination.display()
                ),
            ));
        }
        let _ = fs::remove_file(destination);
        std::os::unix::fs::symlink(&target, destination)?;
        return Ok(());
    }
    if !(entry_type.is_file()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported entry type {entry_type:?}"),
        ));
    }

    // Strip setuid/setgid/sticky bits: an archive must never grant them.
    let mode = entry.header().mode().unwrap_or(0o600) & 0o777;
    let mode = if mode == 0 { 0o600 } else { mode };
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(destination)?;
    io::copy(&mut entry, &mut file)?;
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))?;
    Ok(())
}

/// Read a small metadata entry with a hard size cap.
pub fn read_metadata<R: Read>(entry: &mut tar::Entry<'_, R>) -> io::Result<Vec<u8>> {
    let size = entry.size();
    if size > MAX_METADATA_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("metadata entry is {size} bytes, above the {MAX_METADATA_BYTES} limit"),
        ));
    }
    let mut bytes = Vec::with_capacity(size as usize);
    entry.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Tar extraction rejects the same names the writer refuses to store.
pub fn skip_extracted_entry(name: &OsStr) -> bool {
    is_secret_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn secret_names_are_recognised() {
        for name in ["secrets.env", "SECRETS.ENV", "tls.key", "private.KEY"] {
            assert!(is_secret_name(OsStr::new(name)), "{name}");
        }
        for name in ["config.yaml", "keyring.json", "secrets.env.example"] {
            assert!(!is_secret_name(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn unsafe_paths_are_rejected() {
        assert!(safe_path(Path::new("data/config.yaml")).is_ok());
        assert!(safe_path(Path::new("./data/./x")).is_ok());
        assert!(safe_path(Path::new("../outside")).is_err());
        assert!(safe_path(Path::new("data/../../outside")).is_err());
        assert!(safe_path(Path::new("/etc/passwd")).is_err());
        assert!(safe_path(Path::new("")).is_err());
        assert!(!safe_link_target(Path::new("/etc/passwd")));
        assert!(!safe_link_target(Path::new("../../outside")));
        assert!(safe_link_target(Path::new("MEMORY.md")));
    }

    #[test]
    fn a_tree_round_trips_without_secrets_or_absolute_links() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("data");
        fs::create_dir_all(root.join("sessions")).expect("tree");
        fs::write(root.join("config.yaml"), "model: {}\n").expect("config");
        fs::write(root.join("sessions/one.json"), "{}").expect("session");
        fs::write(root.join("secrets.env"), "TOKEN=1\n").expect("secret");
        fs::write(root.join("tls.key"), "KEY\n").expect("key");
        std::os::unix::fs::symlink("config.yaml", root.join("latest.yaml")).expect("symlink");
        std::os::unix::fs::symlink("/etc/passwd", root.join("escape")).expect("escaping link");

        let mut builder = tar::Builder::new(gzip_writer());
        append_dir(&mut builder, Path::new("data"), 0o700).expect("dir");
        let count = append_tree(&mut builder, &root, Path::new("data")).expect("tree");
        assert_eq!(count, 3, "config, session and the safe symlink");
        let encoder = builder.into_inner().expect("finish tar");
        let bytes = encoder.finish().expect("gzip");

        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(Cursor::new(bytes)));
        let mut names = Vec::new();
        for entry in archive.entries().expect("entries") {
            let entry = entry.expect("entry");
            names.push(entry.path().expect("path").display().to_string());
        }
        names.sort();
        assert_eq!(
            names,
            vec![
                "data",
                "data/config.yaml",
                "data/latest.yaml",
                "data/sessions",
                "data/sessions/one.json",
            ]
        );
    }

    #[test]
    fn a_name_filter_skips_files_and_whole_directories() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("data");
        fs::create_dir_all(root.join("cache")).expect("cache dir");
        fs::create_dir_all(root.join("skills")).expect("skills dir");
        fs::write(root.join("cache/blob.bin"), "cache").expect("cache file");
        fs::write(root.join("skills/note.md"), "note\n").expect("skill");
        fs::write(root.join("SOUL.md"), "soul\n").expect("soul");

        let mut builder = tar::Builder::new(gzip_writer());
        let count = append_tree_excluding(&mut builder, &root, Path::new("data"), |name| {
            name == "cache"
        })
        .expect("tree");
        assert_eq!(count, 2, "SOUL and the skill, no cache file");
        let bytes = builder.into_inner().expect("tar").finish().expect("gzip");

        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(Cursor::new(bytes)));
        let mut names = Vec::new();
        for entry in archive.entries().expect("entries") {
            names.push(
                entry
                    .expect("entry")
                    .path()
                    .expect("path")
                    .display()
                    .to_string(),
            );
        }
        assert!(names.contains(&"data/SOUL.md".to_string()), "{names:?}");
        assert!(
            names.contains(&"data/skills/note.md".to_string()),
            "{names:?}"
        );
        assert!(
            !names.iter().any(|name| name.contains("cache")),
            "{names:?}"
        );
    }
}

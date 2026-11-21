//! File-level validation of an explicitly selected local input.

use std::io::{self, Read};
use std::path::{Component, Path};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, Metadata, OpenOptions};
use serde::Serialize;
use thiserror::Error;

/// A nonempty regular file identified by its bytes at the time of validation.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename = "local")]
pub struct LocalSource {
    source_id: String,
    part_id: (),
}

impl LocalSource {
    pub fn source_id(&self) -> &str {
        &self.source_id
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LocalFileError {
    #[error("Expected an absolute authorized directory and an explicit file path within it")]
    InvalidPath,
    #[error("The specified file or directory does not exist")]
    NotFound,
    #[error("Links and reparse points are not supported below the authorized directory")]
    Link,
    #[error("The selected path is a directory")]
    Directory,
    #[error("The selected input is not a regular file")]
    NotRegular,
    #[error("The selected file is empty")]
    Empty,
    #[error("The selected file changed while being read")]
    Changed,
    #[error("File access failed ({0})")]
    Access(io::ErrorKind),
}

impl From<io::Error> for LocalFileError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::NotFound => Self::NotFound,
            kind => Self::Access(kind),
        }
    }
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use cap_std::fs::MetadataExt;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// Resolve only this path against a trusted absolute root. The current directory is never used.
pub fn validate_local_file(root: &Path, input: &Path) -> Result<LocalSource, LocalFileError> {
    let pending = prepare_file(root, input, |dir, name| dir.open_dir_nofollow(name))?;
    identify(pending.open()?)
}

/// Preflight classifies the input. Authorization is enforced again by the final open.
struct PendingFile<'a> {
    directory: Dir,
    path: &'a Path,
}

fn prepare_file<'a>(
    root: &Path,
    input: &'a Path,
    open_child: impl Fn(&Dir, &Path) -> io::Result<Dir>,
) -> Result<PendingFile<'a>, LocalFileError> {
    if !root.is_absolute() || input.as_os_str().is_empty() {
        return Err(LocalFileError::InvalidPath);
    }
    #[cfg(windows)]
    if !matches!(
        root.components().next(),
        Some(Component::Prefix(p)) if matches!(p.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_))
    ) {
        return Err(LocalFileError::InvalidPath);
    }
    let relative = if input.is_absolute() {
        input
            .strip_prefix(root)
            .map_err(|_| LocalFileError::InvalidPath)?
    } else {
        input
    };
    let mut parts = relative.components().filter(|c| *c != Component::CurDir);
    let invalid = parts.clone().any(|c| match c {
        Component::Normal(name) => cfg!(windows) && name.as_encoded_bytes().contains(&b':'),
        _ => true,
    });
    if invalid {
        return Err(LocalFileError::InvalidPath);
    }

    let root = Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
    let leaf = parts.next_back().ok_or(LocalFileError::Directory)?;
    #[cfg(not(any(windows, target_os = "linux")))]
    if parts.clone().next().is_some() {
        return Err(LocalFileError::Access(io::ErrorKind::Unsupported));
    }
    let mut parent = None;
    for component in parts {
        let dir = parent.as_ref().unwrap_or(&root);
        let name = Path::new(component.as_os_str());
        if is_link(&dir.symlink_metadata(name)?) {
            return Err(LocalFileError::Link);
        }
        let child = open_child(dir, name)?;
        #[cfg(windows)]
        if is_link(&child.dir_metadata()?) {
            return Err(LocalFileError::Link);
        }
        parent = Some(child);
    }
    let name = Path::new(leaf.as_os_str());
    let metadata = parent.as_ref().unwrap_or(&root).symlink_metadata(name)?;
    if is_link(&metadata) {
        return Err(LocalFileError::Link);
    }
    if metadata.is_dir() {
        return Err(LocalFileError::Directory);
    }
    if !metadata.is_file() {
        return Err(LocalFileError::NotRegular);
    }
    let last = input
        .as_os_str()
        .as_encoded_bytes()
        .rsplit(|&b| b == b'/' || (cfg!(windows) && b == b'\\'))
        .next();
    if matches!(last, Some(b"" | b".")) {
        return Err(LocalFileError::InvalidPath);
    }
    #[cfg(target_os = "linux")]
    if parent.is_some() {
        return Ok(PendingFile {
            directory: root,
            path: relative,
        });
    }
    #[cfg(windows)]
    let root = parent.unwrap_or(root);
    Ok(PendingFile {
        directory: root,
        path: name,
    })
}

impl PendingFile<'_> {
    fn open(self) -> Result<cap_std::fs::File, LocalFileError> {
        #[cfg(target_os = "linux")]
        if self.path.components().count() > 1 {
            use rustix::fs::{Mode, OFlags, ResolveFlags, openat2};
            use rustix::io::Errno;

            let fd = openat2(
                &self.directory,
                self.path,
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOCTTY,
                Mode::empty(),
                ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
            )
            .map_err(|error| match error {
                Errno::NOSYS | Errno::INVAL => LocalFileError::Access(io::ErrorKind::Unsupported),
                Errno::LOOP => LocalFileError::Link,
                Errno::XDEV => LocalFileError::InvalidPath,
                _ => LocalFileError::from(io::Error::from(error)),
            })?;
            return Ok(cap_std::fs::File::from_std(fd.into()));
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOCTTY);
        }
        Ok(self.directory.open_with(self.path, &options)?)
    }
}

fn identify(mut file: cap_std::fs::File) -> Result<LocalSource, LocalFileError> {
    let before = file.metadata()?;
    if is_link(&before) || !before.is_file() {
        return Err(LocalFileError::NotRegular);
    }
    let length = before.len();
    if length == 0 {
        return Err(LocalFileError::Empty);
    }
    let mut hash = blake3::Hasher::new();
    let copied = io::copy(&mut (&mut file).take(length), &mut hash)?;
    let after = file.metadata()?;
    if copied != length || after.len() != length || after.modified()? != before.modified()? {
        return Err(LocalFileError::Changed);
    }
    Ok(LocalSource {
        source_id: hash.finalize().to_hex().to_string(),
        part_id: (),
    })
}

#[cfg(test)]
mod tests;

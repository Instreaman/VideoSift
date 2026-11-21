use std::io;
use std::path::Path;

use tempfile::NamedTempFile;

#[cfg(unix)]
pub(super) fn publish(temp: NamedTempFile, destination: &Path) -> io::Result<()> {
    publish_and_sync(temp, destination, |directory| {
        std::fs::File::open(directory)?.sync_all()
    })
}

#[cfg(unix)]
pub(super) fn publish_and_sync(
    temp: NamedTempFile,
    destination: &Path,
    sync_directory: impl Fn(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let directory = destination.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let parent = directory.parent().ok_or(io::ErrorKind::InvalidInput)?;
    temp.persist_noclobber(destination)
        .map_err(|error| error.error)?;
    sync_directory(directory)?;
    // The destination directory itself may have been created during this attempt.
    sync_directory(parent)
}

#[cfg(windows)]
pub(super) fn publish(mut temp: NamedTempFile, destination: &Path) -> io::Result<()> {
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_NORMAL, MOVEFILE_WRITE_THROUGH, MoveFileExW, SetFileAttributesW,
    };
    use windows::core::HSTRING;

    let source = HSTRING::from(temp.path().as_os_str());
    let target = HSTRING::from(destination.as_os_str());
    // SAFETY: both absolute paths are owned, NUL-terminated and live through the calls.
    // Clear tempfile's temporary attribute. Do not allow replacement or cross-volume copy.
    unsafe {
        SetFileAttributesW(&source, FILE_ATTRIBUTE_NORMAL)?;
        MoveFileExW(&source, &target, MOVEFILE_WRITE_THROUGH)?;
    }
    temp.disable_cleanup(true);
    Ok(())
}

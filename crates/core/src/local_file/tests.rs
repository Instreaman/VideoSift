#[cfg(any(windows, target_os = "linux"))]
use super::*;

#[cfg(target_os = "linux")]
#[test]
fn final_open_rejects_directory_moved_out_of_root_before_read() {
    use std::fs;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("selected");
    fs::create_dir_all(root.join("clips")).unwrap();
    let relative = Path::new("clips/input");
    fs::write(root.join(relative), b"fixture").unwrap();

    let pending = prepare_file(&root, relative, |dir, name| dir.open_dir_nofollow(name)).unwrap();
    fs::rename(root.join("clips"), temp.path().join("relocated")).unwrap();
    // Opening itself must fail; no readable file can reach identify().
    assert!(matches!(pending.open(), Err(LocalFileError::NotFound)));
}

#[cfg(target_os = "linux")]
#[test]
fn final_open_rejects_an_intermediate_link_even_within_the_root() {
    use std::{fs, os::unix::fs::symlink};

    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("clips")).unwrap();
    let relative = Path::new("clips/input");
    fs::write(root.path().join(relative), b"fixture").unwrap();
    let pending = prepare_file(root.path(), relative, |dir, name| {
        dir.open_dir_nofollow(name)
    })
    .unwrap();
    fs::rename(root.path().join("clips"), root.path().join("renamed")).unwrap();
    symlink("renamed", root.path().join("clips")).unwrap();
    assert!(matches!(pending.open(), Err(LocalFileError::Link)));
}

#[cfg(windows)]
#[test]
fn rejects_reparse_attribute_on_returned_directory_handle() {
    use std::{fs, os::windows::fs::OpenOptionsExt, process::Command};
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("plain")).unwrap();
    let result = Command::new("cmd")
        .args(["/d", "/c", "mklink", "/J", "junction", "plain"])
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(result.status.success());

    // Preflight sees an ordinary directory. Supply a native handle to the
    // reparse point itself to exercise the post-open check deterministically.
    let result = prepare_file(root.path(), Path::new("plain/missing"), |_, _| {
        let file = fs::OpenOptions::new()
            .read(true)
            .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
            .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0)
            .open(root.path().join("junction"))?;
        Ok(Dir::from_std_file(file))
    });
    assert!(matches!(result, Err(LocalFileError::Link)));
}

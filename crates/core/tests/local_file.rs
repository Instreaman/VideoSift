use std::{fs, path::Path};

use serde_json::Value;
use tempfile::tempdir;
use videosift_core::VideoSource;
use videosift_core::local_file::{LocalFileError as Error, validate_local_file};

#[test]
fn serializes_the_shared_fixture_for_a_readable_non_media_file() {
    let root = tempdir().unwrap();
    let relative = Path::new("合成 目录").join("private clip.unknown");
    let absolute = root.path().join(&relative);
    fs::create_dir(absolute.parent().unwrap()).unwrap();
    fs::write(&absolute, b"abc").unwrap();

    let source = validate_local_file(root.path(), &relative).unwrap();
    assert_eq!(validate_local_file(root.path(), &absolute).unwrap(), source);
    let fixtures: Vec<Value> =
        serde_json::from_str(include_str!("../../../contracts/examples/valid.json")).unwrap();
    let fixture = fixtures
        .iter()
        .find(|f| f["name"] == "local_file_digest")
        .unwrap();
    assert_eq!(
        serde_json::to_value(VideoSource::Local(source)).unwrap(),
        fixture["instance"]["source"]
    );
}

#[test]
fn identity_follows_content_across_names_and_roots() {
    let first = tempdir().unwrap();
    let second = tempdir().unwrap();
    let name = Path::new("before.mov");
    let copy = Path::new("copy");
    let bytes = vec![0x31; 200_000];
    fs::write(first.path().join(name), &bytes).unwrap();
    let original = validate_local_file(first.path(), name).unwrap();
    fs::rename(first.path().join(name), first.path().join(copy)).unwrap();
    fs::write(second.path().join(copy), &bytes).unwrap();
    assert_eq!(validate_local_file(first.path(), copy).unwrap(), original);
    assert_eq!(validate_local_file(second.path(), copy).unwrap(), original);
    fs::write(second.path().join(copy), vec![0x32; bytes.len()]).unwrap();
    assert_ne!(validate_local_file(second.path(), copy).unwrap(), original);
}

#[test]
fn distinguishes_missing_directory_empty_and_invalid_inputs() {
    let root = tempdir().unwrap();
    let other = tempdir().unwrap();
    fs::create_dir(root.path().join("folder")).unwrap();
    fs::write(root.path().join("empty.mp4"), []).unwrap();
    for (input, error) in [
        (Path::new("missing.mp4"), Error::NotFound),
        (Path::new("folder"), Error::Directory),
        (Path::new("."), Error::Directory),
        (Path::new("empty.mp4"), Error::Empty),
        (Path::new("empty.mp4/"), Error::InvalidPath),
        (Path::new("empty.mp4/."), Error::InvalidPath),
        (Path::new(""), Error::InvalidPath),
        (Path::new("folder/../empty.mp4"), Error::InvalidPath),
        (other.path(), Error::InvalidPath),
    ] {
        let actual = validate_local_file(root.path(), input).unwrap_err();
        assert_eq!(actual, error);
        assert!(!format!("{actual} {actual:?}").contains(&*root.path().to_string_lossy()));
    }
    assert_eq!(
        validate_local_file(Path::new("."), Path::new("clip")),
        Err(Error::InvalidPath)
    );
}

#[cfg(unix)]
#[test]
fn rejects_links_in_any_component_and_special_files() {
    use std::os::unix::{fs::symlink, net::UnixListener};
    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    fs::write(root.path().join("regular"), b"synthetic").unwrap();
    fs::write(outside.path().join("regular"), b"outside").unwrap();
    symlink("regular", root.path().join("inside-link")).unwrap();
    symlink("absent", root.path().join("dangling")).unwrap();
    symlink(outside.path(), root.path().join("outside-link")).unwrap();
    for input in ["inside-link", "dangling", "outside-link/regular"] {
        assert_eq!(
            validate_local_file(root.path(), Path::new(input)),
            Err(Error::Link)
        );
    }
    let _socket = UnixListener::bind(root.path().join("socket")).unwrap();
    assert_eq!(
        validate_local_file(root.path(), Path::new("socket")),
        Err(Error::NotRegular)
    );
}

#[cfg(windows)]
#[test]
fn rejects_directory_junctions_and_nonlocal_namespaces() {
    use std::process::Command;
    let parent = tempdir().unwrap();
    let root = parent.path().join("authorized");
    fs::create_dir(&root).unwrap();
    fs::create_dir(parent.path().join("outside")).unwrap();
    fs::write(parent.path().join("outside/file"), b"synthetic").unwrap();
    let status = Command::new("cmd")
        .args(["/d", "/c", "mklink", "/J", "junction", r"..\outside"])
        .current_dir(&root)
        .output()
        .unwrap()
        .status;
    assert!(
        status.success(),
        "creating a synthetic directory junction failed"
    );
    assert_eq!(
        validate_local_file(&root, Path::new("junction/file")),
        Err(Error::Link)
    );
    for name in [
        r"x:drive-relative",
        r"\rooted",
        r"\\example.invalid\share\clip",
        "file:stream",
    ] {
        assert_eq!(
            validate_local_file(&root, Path::new(name)),
            Err(Error::InvalidPath)
        );
    }
}

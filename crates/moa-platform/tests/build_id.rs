use moa_platform::{build_id, build_id_of, canonical, fnv1a64};

fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("moa")
        .tempdir_in("/tmp")
        .unwrap()
}
fn is_id(s: &str) -> bool {
    s.len() == 16
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[test]
fn a_missing_path_is_canonicalized_through_its_existing_parent() {
    let t = tmp();
    let parent = std::fs::canonicalize(t.path()).unwrap();
    let missing = t.path().join("not").join("yet");
    assert_eq!(canonical(&missing), parent.join("not").join("yet"));
    // A relative path is returned unchanged.
    assert_eq!(
        canonical(std::path::Path::new("no/such/dir")),
        std::path::PathBuf::from("no/such/dir")
    );
}

#[test]
fn a_relative_path_that_exists_is_returned_unchanged() {
    // "." and ".." exist relative to whatever the cwd is; no cwd change needed.
    for rel in [".", "..", "./src"] {
        let p = std::path::Path::new(rel);
        assert_eq!(canonical(p), p);
    }
}

#[test]
fn fnv_known_vectors() {
    assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
}

#[test]
fn build_id_follows_path_length_and_mtime() {
    let t = tmp();
    let a = t.path().join("a");
    let b = t.path().join("b");
    std::fs::write(&a, b"hello").unwrap();
    std::fs::copy(&a, &b).unwrap();
    let ida = build_id_of(&a).unwrap();
    assert!(is_id(&ida));
    assert_eq!(build_id_of(&a).unwrap(), ida);
    assert_ne!(build_id_of(&b).unwrap(), ida); // path differs

    let mut f = std::fs::OpenOptions::new().append(true).open(&a).unwrap();
    std::io::Write::write_all(&mut f, b"!").unwrap();
    assert_ne!(build_id_of(&a).unwrap(), ida); // length (and mtime) differ

    // mtime alone: same length, different time
    let before = build_id_of(&b).unwrap();
    let f = std::fs::File::options().write(true).open(&b).unwrap();
    f.set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000))
        .unwrap();
    assert_ne!(build_id_of(&b).unwrap(), before);
}

#[test]
fn build_id_of_a_missing_file_is_an_error() {
    assert!(build_id_of(std::path::Path::new("/no/such/file")).is_err());
}

#[test]
fn the_process_build_id_is_stable_hex() {
    assert!(is_id(build_id()));
    assert_eq!(build_id(), build_id());
}

#[test]
fn canonical_resolves_or_passes_through() {
    let t = tmp();
    let real = t.path().join("r");
    std::fs::write(&real, b"x").unwrap();
    let link = t.path().join("l");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(canonical(&link), std::fs::canonicalize(&real).unwrap());
    assert_eq!(
        canonical(std::path::Path::new("/no/such")),
        std::path::Path::new("/no/such")
    );
}

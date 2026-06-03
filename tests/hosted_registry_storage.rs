use denia::registry::storage::RegistryStorage;

#[test]
fn blob_path_is_content_addressed() {
    let dir = tempfile::tempdir().unwrap();
    let storage = RegistryStorage::new(dir.path().to_path_buf());
    let path = storage.blob_path("sha256:abcdef").unwrap();
    assert!(path.ends_with("registry/blobs/sha256/abcdef"));
}

#[test]
fn reject_non_sha256_digest() {
    let dir = tempfile::tempdir().unwrap();
    let storage = RegistryStorage::new(dir.path().to_path_buf());
    assert!(storage.blob_path("md5:abcdef").is_err());
}

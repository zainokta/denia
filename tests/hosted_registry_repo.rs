use denia::registry::repo::HostedRegistryRepo;
use denia::repo::sqlite::{SqlitePool, run_migrations};
use uuid::Uuid;

#[test]
fn repository_and_tag_roundtrip() {
    let pool = SqlitePool::open_in_memory().unwrap();
    run_migrations(&pool).unwrap();
    let repo = HostedRegistryRepo::new(pool);
    let project_id = Uuid::now_v7();
    let service_id = Uuid::now_v7();
    let repository = repo
        .ensure_repository(project_id, service_id, "default/api")
        .unwrap();
    repo.put_manifest(
        repository.id,
        "sha256:abc",
        "application/vnd.oci.image.manifest.v1+json",
        100,
    )
    .unwrap();
    repo.put_tag(repository.id, "latest", "sha256:abc").unwrap();
    let tags = repo.tags(repository.id).unwrap();
    assert_eq!(tags[0].tag, "latest");
    assert_eq!(tags[0].manifest_digest, "sha256:abc");
}

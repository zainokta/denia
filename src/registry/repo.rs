//! SQLite-backed metadata repository for the hosted OCI registry.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use crate::registry::domain::{HostedManifest, HostedRepository, HostedTag, HostedUpload};
use crate::repo::error::RepoError;
use crate::repo::sqlite::SqlitePool;

#[derive(Clone)]
pub struct HostedRegistryRepo {
    pool: SqlitePool,
}

impl HostedRegistryRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Idempotent: returns the existing repository for (project_id, service_id) or inserts a new one.
    pub fn ensure_repository(
        &self,
        project_id: Uuid,
        service_id: Uuid,
        name: &str,
    ) -> Result<HostedRepository, RepoError> {
        let conn = self.pool.connection()?;
        let pid = project_id.to_string();
        let sid = service_id.to_string();

        let existing: Option<(String, String, String, String, String)> = conn
            .query_row(
                "SELECT id, project_id, service_id, name, created_at \
                 FROM hosted_repositories WHERE project_id=?1 AND service_id=?2",
                params![&pid, &sid],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;

        if let Some((id, p, s, n, ca)) = existing {
            return Ok(HostedRepository {
                id: Uuid::parse_str(&id)?,
                project_id: Uuid::parse_str(&p)?,
                service_id: Uuid::parse_str(&s)?,
                name: n,
                created_at: DateTime::parse_from_rfc3339(&ca)?.with_timezone(&Utc),
            });
        }

        let id = Uuid::now_v7();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        conn.execute(
            "INSERT INTO hosted_repositories (id, project_id, service_id, name, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id.to_string(), &pid, &sid, name, &now_str],
        )?;

        Ok(HostedRepository {
            id,
            project_id,
            service_id,
            name: name.to_string(),
            created_at: now,
        })
    }

    /// Upserts a manifest record for a repository.
    pub fn put_manifest(
        &self,
        repository_id: Uuid,
        digest: &str,
        media_type: &str,
        size: u64,
    ) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        let rid = repository_id.to_string();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO hosted_manifests (repository_id, digest, media_type, size, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(repository_id, digest) DO UPDATE SET \
               media_type=excluded.media_type, size=excluded.size",
            params![&rid, digest, media_type, size as i64, &now],
        )?;
        Ok(())
    }

    /// Upserts a tag, pointing it at a manifest digest.
    pub fn put_tag(
        &self,
        repository_id: Uuid,
        tag: &str,
        manifest_digest: &str,
    ) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        let rid = repository_id.to_string();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO hosted_tags (repository_id, tag, manifest_digest, updated_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(repository_id, tag) DO UPDATE SET \
               manifest_digest=excluded.manifest_digest, updated_at=excluded.updated_at",
            params![&rid, tag, manifest_digest, &now],
        )?;
        Ok(())
    }

    /// Returns all tags for a repository ordered by tag name.
    pub fn tags(&self, repository_id: Uuid) -> Result<Vec<HostedTag>, RepoError> {
        let conn = self.pool.connection()?;
        let rid = repository_id.to_string();
        let mut stmt = conn.prepare(
            "SELECT tag, manifest_digest, updated_at \
             FROM hosted_tags WHERE repository_id=?1 ORDER BY tag",
        )?;
        let rows = stmt.query_map(params![&rid], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        let mut tags = Vec::new();
        for row in rows {
            let (tag, manifest_digest, updated_at_str) = row?;
            let updated_at = DateTime::parse_from_rfc3339(&updated_at_str)
                .map_err(RepoError::Time)?
                .with_timezone(&Utc);
            tags.push(HostedTag {
                repository_id,
                tag,
                manifest_digest,
                updated_at,
            });
        }
        Ok(tags)
    }

    /// Returns the manifest digest a tag points at, if any.
    pub fn tag(&self, repository_id: Uuid, tag: &str) -> Result<Option<String>, RepoError> {
        let conn = self.pool.connection()?;
        let rid = repository_id.to_string();
        let digest: Option<String> = conn
            .query_row(
                "SELECT manifest_digest FROM hosted_tags WHERE repository_id=?1 AND tag=?2",
                params![&rid, tag],
                |row| row.get(0),
            )
            .optional()?;
        Ok(digest)
    }

    /// Returns a manifest by digest, if it exists.
    pub fn manifest(
        &self,
        repository_id: Uuid,
        digest: &str,
    ) -> Result<Option<HostedManifest>, RepoError> {
        let conn = self.pool.connection()?;
        let rid = repository_id.to_string();
        let row: Option<(String, String, i64, String)> = conn
            .query_row(
                "SELECT digest, media_type, size, created_at \
                 FROM hosted_manifests WHERE repository_id=?1 AND digest=?2",
                params![&rid, digest],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;

        match row {
            None => Ok(None),
            Some((d, mt, sz, ca)) => Ok(Some(HostedManifest {
                repository_id,
                digest: d,
                media_type: mt,
                size: sz as u64,
                created_at: DateTime::parse_from_rfc3339(&ca)?.with_timezone(&Utc),
            })),
        }
    }

    /// Creates a new upload session record using the provided ID.
    pub fn create_upload(
        &self,
        id: Uuid,
        repository_id: Uuid,
        path: &str,
    ) -> Result<HostedUpload, RepoError> {
        let conn = self.pool.connection()?;
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let rid = repository_id.to_string();
        conn.execute(
            "INSERT INTO hosted_uploads (id, repository_id, path, started_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id.to_string(), &rid, path, &now_str, &now_str],
        )?;
        Ok(HostedUpload {
            id,
            repository_id,
            path: PathBuf::from(path),
            started_at: now,
            updated_at: now,
        })
    }

    /// Returns an upload session by ID, if it exists.
    pub fn upload(&self, upload_id: Uuid) -> Result<Option<HostedUpload>, RepoError> {
        let conn = self.pool.connection()?;
        let uid = upload_id.to_string();
        let row: Option<(String, String, String, String, String)> = conn
            .query_row(
                "SELECT id, repository_id, path, started_at, updated_at \
                 FROM hosted_uploads WHERE id=?1",
                params![&uid],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;

        match row {
            None => Ok(None),
            Some((id_str, rid_str, path_str, started_str, updated_str)) => Ok(Some(HostedUpload {
                id: Uuid::parse_str(&id_str)?,
                repository_id: Uuid::parse_str(&rid_str)?,
                path: PathBuf::from(path_str),
                started_at: DateTime::parse_from_rfc3339(&started_str)?.with_timezone(&Utc),
                updated_at: DateTime::parse_from_rfc3339(&updated_str)?.with_timezone(&Utc),
            })),
        }
    }

    /// Deletes an upload session record.
    pub fn delete_upload(&self, upload_id: Uuid) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        conn.execute(
            "DELETE FROM hosted_uploads WHERE id=?1",
            params![upload_id.to_string()],
        )?;
        Ok(())
    }

    /// Upserts a blob record for a repository.
    pub fn put_blob(&self, repository_id: Uuid, digest: &str, size: u64) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        let rid = repository_id.to_string();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO hosted_blobs (repository_id, digest, size, created_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(repository_id, digest) DO UPDATE SET size=excluded.size",
            params![&rid, digest, size as i64, &now],
        )?;
        Ok(())
    }

    /// Returns true if the blob exists for the given repository.
    pub fn has_blob(&self, repository_id: Uuid, digest: &str) -> Result<bool, RepoError> {
        let conn = self.pool.connection()?;
        let rid = repository_id.to_string();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM hosted_blobs WHERE repository_id=?1 AND digest=?2",
            params![&rid, digest],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Returns the distinct set of manifest digests across all repositories.
    /// The garbage collector treats these — plus the config/layer digests
    /// parsed from each manifest body — as the live reference set.
    pub fn all_manifest_digests(&self) -> Result<Vec<String>, RepoError> {
        let conn = self.pool.connection()?;
        let mut stmt = conn.prepare("SELECT DISTINCT digest FROM hosted_manifests")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut digests = Vec::new();
        for row in rows {
            digests.push(row?);
        }
        Ok(digests)
    }

    /// Deletes every blob row for `digest` across all repositories. Called by
    /// the GC after the on-disk blob file has been removed.
    pub fn delete_blob_rows(&self, digest: &str) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        conn.execute("DELETE FROM hosted_blobs WHERE digest=?1", params![digest])?;
        Ok(())
    }

    /// Records a completed garbage-collection run for observability.
    pub fn record_gc_run(
        &self,
        scanned: u64,
        deleted: u64,
        deleted_bytes: u64,
    ) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO hosted_registry_gc_runs \
             (id, status, scanned_blobs, deleted_blobs, deleted_bytes, started_at, finished_at) \
             VALUES (?1, 'completed', ?2, ?3, ?4, ?5, ?5)",
            params![
                Uuid::now_v7().to_string(),
                scanned as i64,
                deleted as i64,
                deleted_bytes as i64,
                &now,
            ],
        )?;
        Ok(())
    }

    /// Total number of repositories.
    pub fn count_repositories(&self) -> Result<u64, RepoError> {
        let conn = self.pool.connection()?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM hosted_repositories", [], |row| {
            row.get(0)
        })?;
        Ok(count as u64)
    }

    /// Total number of blob rows.
    pub fn count_blobs(&self) -> Result<u64, RepoError> {
        let conn = self.pool.connection()?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM hosted_blobs", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    /// Sum of all recorded blob sizes (bytes).
    pub fn total_blob_bytes(&self) -> Result<u64, RepoError> {
        let conn = self.pool.connection()?;
        let total: i64 = conn.query_row(
            "SELECT COALESCE(SUM(size), 0) FROM hosted_blobs",
            [],
            |row| row.get(0),
        )?;
        Ok(total.max(0) as u64)
    }

    /// All repositories ordered by name.
    pub fn list_repositories(&self) -> Result<Vec<HostedRepository>, RepoError> {
        let conn = self.pool.connection()?;
        let mut stmt = conn.prepare(
            "SELECT id, project_id, service_id, name, created_at \
             FROM hosted_repositories ORDER BY name",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut repositories = Vec::new();
        for row in rows {
            let (id, pid, sid, name, created_at) = row?;
            repositories.push(HostedRepository {
                id: Uuid::parse_str(&id)?,
                project_id: Uuid::parse_str(&pid)?,
                service_id: Uuid::parse_str(&sid)?,
                name,
                created_at: DateTime::parse_from_rfc3339(&created_at)?.with_timezone(&Utc),
            });
        }
        Ok(repositories)
    }
}

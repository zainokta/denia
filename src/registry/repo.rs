//! SQLite-backed metadata repository for the hosted OCI registry.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use crate::registry::domain::{HostedManifest, HostedRepository, HostedTag};
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
}

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use thiserror::Error;

use crate::repo::error::RepoError;

#[derive(Debug, Error)]
pub enum StateError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("uuid error: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("time parse error: {0}")]
    Time(#[from] chrono::ParseError),
    #[error("state lock poisoned")]
    LockPoisoned,
    #[error("cannot delete project with existing services")]
    ProjectNotEmpty,
    #[error("project not found")]
    UnknownProject,
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("cannot delete the last super admin")]
    LastSuperAdmin,
    #[error("invalid status: {0}")]
    InvalidStatus(String),
    #[error("registry not found")]
    RegistryNotFound,
    #[error("registry is referenced by one or more services")]
    RegistryInUse,
}

impl From<RepoError> for StateError {
    fn from(err: RepoError) -> Self {
        match err {
            RepoError::Sqlite(e) => StateError::Sqlite(e),
            RepoError::Json(e) => StateError::Json(e),
            RepoError::Uuid(e) => StateError::Uuid(e),
            RepoError::Time(e) => StateError::Time(e),
            RepoError::LockPoisoned => StateError::LockPoisoned,
            RepoError::ProjectNotEmpty => StateError::ProjectNotEmpty,
            RepoError::UnknownProject => StateError::UnknownProject,
            RepoError::InvalidCredentials => StateError::InvalidCredentials,
            RepoError::LastSuperAdmin => StateError::LastSuperAdmin,
            RepoError::InvalidStatus(s) => StateError::InvalidStatus(s),
            RepoError::RegistryNotFound => StateError::RegistryNotFound,
            RepoError::RegistryInUse => StateError::RegistryInUse,
        }
    }
}

fn apply_sqlite_pragmas(connection: &Connection) -> Result<(), StateError> {
    connection.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;
        PRAGMA synchronous = NORMAL;
        PRAGMA busy_timeout = 5000;
        "#,
    )?;
    Ok(())
}

#[derive(Clone)]
pub struct SqliteStore {
    pub(crate) connection: Arc<Mutex<Connection>>,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, StateError> {
        let path = path.as_ref();
        let connection = Connection::open(path)?;
        apply_sqlite_pragmas(&connection)?;
        #[cfg(unix)]
        {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn open_in_memory() -> Result<Self, StateError> {
        let connection = Connection::open_in_memory()?;
        apply_sqlite_pragmas(&connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Delegates to `repo::sqlite::run_migrations` (Task 8). The body lives there
    /// so the repo layer owns schema setup; behavior is byte-identical.
    pub fn migrate(&self) -> Result<(), StateError> {
        let pool = crate::repo::sqlite::SqlitePool {
            inner: Arc::clone(&self.connection),
        };
        crate::repo::sqlite::run_migrations(&pool).map_err(StateError::from)
    }

    pub fn schema_version(&self) -> Result<i64, StateError> {
        let connection = self.connection()?;
        let v = connection
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap_or(0);
        Ok(v)
    }

    pub(crate) fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StateError> {
        self.connection.lock().map_err(|_| StateError::LockPoisoned)
    }

    /// Returns a `SqlitePool` sharing the same underlying connection as this
    /// store. Used by callers wiring per-aggregate `Sqlite*Repo` instances.
    pub fn pool(&self) -> crate::repo::sqlite::SqlitePool {
        crate::repo::sqlite::SqlitePool {
            inner: Arc::clone(&self.connection),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DomainStatus, Project, ServiceConfig, ServiceDomain, ServiceSource};
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn migrate_advances_to_version_6() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        assert_eq!(store.schema_version().unwrap(), 6);
    }

    #[test]
    fn registry_crud_roundtrip_and_unique_name() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        let project = store
            .put_project(crate::domain::Project::new("p", None).unwrap())
            .unwrap();
        let cred = crate::secrets::SecretRef::parse("ghcr-cred").unwrap();
        let reg = crate::domain::Registry::new(
            project.id,
            "ghcr",
            "ghcr.io",
            crate::domain::RegistryAuthKind::Basic,
            Some(cred),
        )
        .unwrap();

        store.create_registry(&reg).unwrap();
        assert_eq!(store.registry(reg.id).unwrap().unwrap().endpoint, "ghcr.io");
        assert_eq!(store.registries_for_project(project.id).unwrap().len(), 1);

        // duplicate (project_id, name) rejected
        let dup = crate::domain::Registry::new(
            project.id,
            "ghcr",
            "other.io",
            crate::domain::RegistryAuthKind::Anonymous,
            None,
        )
        .unwrap();
        assert!(store.create_registry(&dup).is_err());
    }

    #[test]
    fn delete_registry_blocked_when_referenced() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        let project = store
            .put_project(crate::domain::Project::new("p", None).unwrap())
            .unwrap();
        let reg = crate::domain::Registry::new(
            project.id,
            "ghcr",
            "ghcr.io",
            crate::domain::RegistryAuthKind::Anonymous,
            None,
        )
        .unwrap();
        store.create_registry(&reg).unwrap();

        // build a service in the project referencing reg.id
        use crate::domain::{ExternalImageSource, HealthCheck, ServiceConfig, ServiceSource};
        let service = ServiceConfig::new(
            project.id,
            "svc",
            vec!["example.com".into()],
            ServiceSource::ExternalImage(ExternalImageSource {
                image: String::new(),
                credential: None,
                registry_id: Some(reg.id),
                image_ref: Some("acme/web:1".into()),
            }),
            3000,
            HealthCheck::new("/health", 10),
            None,
            vec![],
        )
        .unwrap();
        store.put_service(service).unwrap();

        assert!(matches!(
            store.delete_registry(reg.id).unwrap_err(),
            crate::state::StateError::RegistryInUse
        ));
    }

    #[test]
    fn delete_registry_happy_path() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        let project = store
            .put_project(crate::domain::Project::new("p", None).unwrap())
            .unwrap();
        let reg = crate::domain::Registry::new(
            project.id,
            "ghcr",
            "ghcr.io",
            crate::domain::RegistryAuthKind::Anonymous,
            None,
        )
        .unwrap();
        store.create_registry(&reg).unwrap();

        store.delete_registry(reg.id).unwrap();
        assert!(store.registry(reg.id).unwrap().is_none());
    }

    #[test]
    fn update_registry_round_trip_and_not_found() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        let project = store
            .put_project(crate::domain::Project::new("p", None).unwrap())
            .unwrap();
        let mut reg = crate::domain::Registry::new(
            project.id,
            "ghcr",
            "ghcr.io",
            crate::domain::RegistryAuthKind::Anonymous,
            None,
        )
        .unwrap();
        store.create_registry(&reg).unwrap();

        // rename + update
        reg.name = "ghcr-renamed".into();
        store.update_registry(&reg).unwrap();
        let fetched = store.registry(reg.id).unwrap().unwrap();
        assert_eq!(fetched.name, "ghcr-renamed");

        // unknown id -> RegistryNotFound
        let mut unknown = crate::domain::Registry::new(
            project.id,
            "other",
            "other.io",
            crate::domain::RegistryAuthKind::Anonymous,
            None,
        )
        .unwrap();
        unknown.id = uuid::Uuid::now_v7(); // fresh id never inserted
        assert!(matches!(
            store.update_registry(&unknown).unwrap_err(),
            crate::state::StateError::RegistryNotFound
        ));
    }

    #[test]
    fn service_domains_crud_round_trip() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();

        let project = Project::new("p", None).unwrap();
        store.put_project(project.clone()).unwrap();
        let svc = ServiceConfig::new(
            project.id,
            "svc",
            vec!["example.com".into()],
            ServiceSource::ExternalImage(crate::domain::ExternalImageSource {
                image: "nginx".into(),
                credential: None,
                registry_id: None,
                image_ref: None,
            }),
            80,
            crate::domain::HealthCheck::new("/", 5),
            None,
            Vec::new(),
        )
        .unwrap();
        store.put_service(svc.clone()).unwrap();

        let d = ServiceDomain {
            id: Uuid::now_v7(),
            service_id: svc.id,
            hostname: "x.example.com".into(),
            status: DomainStatus::Pending,
            challenge_token: "tok".into(),
            verified_at: None,
            last_check_at: None,
            last_error: None,
            created_at: Utc::now(),
        };

        store.put_service_domain(&d).unwrap();
        let back = store.get_service_domain(d.id).unwrap().unwrap();
        assert_eq!(back.hostname, "x.example.com");
        assert_eq!(back.status, DomainStatus::Pending);

        let listed = store.list_service_domains_by_service(svc.id).unwrap();
        assert_eq!(listed.len(), 1);

        let by_token = store.get_service_domain_by_token("tok").unwrap().unwrap();
        assert_eq!(by_token.id, d.id);

        store
            .update_service_domain_status(d.id, DomainStatus::Verified, Some(Utc::now()), None)
            .unwrap();
        let v = store.get_service_domain(d.id).unwrap().unwrap();
        assert_eq!(v.status, DomainStatus::Verified);
        assert!(v.verified_at.is_some());

        store.delete_service_domain(d.id).unwrap();
        assert!(store.get_service_domain(d.id).unwrap().is_none());
    }

    #[test]
    fn service_domains_unique_hostname_rejected() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        let project = Project::new("p", None).unwrap();
        store.put_project(project.clone()).unwrap();
        let svc = ServiceConfig::new(
            project.id,
            "svc",
            vec!["x.example.com".into()],
            ServiceSource::ExternalImage(crate::domain::ExternalImageSource {
                image: "nginx".into(),
                credential: None,
                registry_id: None,
                image_ref: None,
            }),
            80,
            crate::domain::HealthCheck::new("/", 5),
            None,
            Vec::new(),
        )
        .unwrap();
        store.put_service(svc.clone()).unwrap();

        let d1 = ServiceDomain {
            id: Uuid::now_v7(),
            service_id: svc.id,
            hostname: "dup.example.com".into(),
            status: DomainStatus::Pending,
            challenge_token: "t1".into(),
            verified_at: None,
            last_check_at: None,
            last_error: None,
            created_at: Utc::now(),
        };
        let d2 = ServiceDomain {
            challenge_token: "t2".into(),
            id: Uuid::now_v7(),
            ..d1.clone()
        };
        store.put_service_domain(&d1).unwrap();
        assert!(store.put_service_domain(&d2).is_err());
    }
}

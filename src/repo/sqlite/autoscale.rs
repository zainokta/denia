//! Autoscaler desired-replica persistence.
//!
//! SQL lives in `*_q` free functions taking `&Connection`. `SqliteStore`
//! facade methods open a connection and delegate.

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::repo::error::RepoError;
use crate::state::{SqliteStore, StateError};

pub(super) fn get_desired_q(conn: &Connection, service_id: Uuid) -> Result<Option<u32>, RepoError> {
    let value: Option<i64> = conn
        .query_row(
            "SELECT desired_replicas FROM autoscale_desired WHERE service_id = ?1",
            params![service_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.map(|v| v as u32))
}

pub(super) fn set_desired_q(
    conn: &Connection,
    service_id: Uuid,
    desired: u32,
) -> Result<(), RepoError> {
    conn.execute(
        r#"
        INSERT INTO autoscale_desired (service_id, desired_replicas)
        VALUES (?1, ?2)
        ON CONFLICT(service_id) DO UPDATE SET desired_replicas = excluded.desired_replicas
        "#,
        params![service_id.to_string(), desired as i64],
    )?;
    Ok(())
}

impl SqliteStore {
    pub fn get_desired_replicas(&self, service_id: Uuid) -> Result<Option<u32>, StateError> {
        let conn = self.connection()?;
        get_desired_q(&conn, service_id).map_err(StateError::from)
    }

    pub fn set_desired_replicas(&self, service_id: Uuid, desired: u32) -> Result<(), StateError> {
        let conn = self.connection()?;
        set_desired_q(&conn, service_id, desired).map_err(StateError::from)
    }
}

#[cfg(test)]
mod tests {
    use crate::state::SqliteStore;
    use uuid::Uuid;

    #[test]
    fn desired_replicas_round_trip() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        let svc = Uuid::now_v7();
        assert_eq!(store.get_desired_replicas(svc).unwrap(), None);
        store.set_desired_replicas(svc, 3).unwrap();
        assert_eq!(store.get_desired_replicas(svc).unwrap(), Some(3));
        store.set_desired_replicas(svc, 5).unwrap(); // upsert
        assert_eq!(store.get_desired_replicas(svc).unwrap(), Some(5));
    }
}

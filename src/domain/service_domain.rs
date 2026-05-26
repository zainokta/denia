use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainStatus {
    Pending,
    Verified,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceDomain {
    pub id: Uuid,
    pub service_id: Uuid,
    pub hostname: String,
    pub status: DomainStatus,
    pub challenge_token: String,
    pub verified_at: Option<DateTime<Utc>>,
    pub last_check_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_status_snake_case_serde() {
        let pending = serde_json::to_string(&DomainStatus::Pending).unwrap();
        assert_eq!(pending, "\"pending\"");
        let verified: DomainStatus = serde_json::from_str("\"verified\"").unwrap();
        assert_eq!(verified, DomainStatus::Verified);
        let failed: DomainStatus = serde_json::from_str("\"failed\"").unwrap();
        assert_eq!(failed, DomainStatus::Failed);
    }

    #[test]
    fn service_domain_round_trip() {
        let id = Uuid::now_v7();
        let service_id = Uuid::now_v7();
        let d = ServiceDomain {
            id,
            service_id,
            hostname: "app.example.com".into(),
            status: DomainStatus::Pending,
            challenge_token: "abc123".into(),
            verified_at: None,
            last_check_at: None,
            last_error: None,
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&d).unwrap();
        let back: ServiceDomain = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }
}

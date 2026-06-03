//! Domain and API types for the interactive service console. See ADR-033.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const DEFAULT_TERMINAL_COLS: u16 = 120;
pub const DEFAULT_TERMINAL_ROWS: u16 = 32;
pub const MAX_TERMINAL_COLS: u16 = 300;
pub const MAX_TERMINAL_ROWS: u16 = 120;

pub fn default_terminal_cols() -> u16 {
    DEFAULT_TERMINAL_COLS
}

pub fn default_terminal_rows() -> u16 {
    DEFAULT_TERMINAL_ROWS
}

/// A live replica of a service's promoted deployment that the console can attach
/// to. Returned by the replica-listing endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleReplicaView {
    pub service_id: Uuid,
    pub service_name: String,
    pub deployment_id: Uuid,
    pub replica_index: u32,
    pub state: String,
}

/// Request body for minting a short-lived single-use console ticket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateConsoleTicketRequest {
    pub replica_index: u32,
    #[serde(default = "default_terminal_cols")]
    pub cols: u16,
    #[serde(default = "default_terminal_rows")]
    pub rows: u16,
}

impl CreateConsoleTicketRequest {
    pub fn normalized(mut self) -> Self {
        self.cols = self.cols.clamp(1, MAX_TERMINAL_COLS);
        self.rows = self.rows.clamp(1, MAX_TERMINAL_ROWS);
        self
    }
}

/// Response carrying the minted ticket and the websocket path to upgrade with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleTicketResponse {
    pub ticket: String,
    pub expires_at: DateTime<Utc>,
    pub ws_path: String,
}

/// Text control frames exchanged over the console websocket. Binary frames carry
/// raw terminal input/output bytes and are not modeled here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConsoleControlFrame {
    Ready {
        session_id: Uuid,
        replica_index: u32,
        cols: u16,
        rows: u16,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Exit {
        code: Option<i32>,
    },
    Error {
        message: String,
    },
    Close,
}

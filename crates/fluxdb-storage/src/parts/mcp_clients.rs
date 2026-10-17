use std::collections::BTreeMap;

use crate::{FileStorage, Result, sqlite};
use serde::{Deserialize, Serialize};

/// 只记录本应用写入的非敏感启动参数，用于更新前核对条目归属。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpClientLaunch {
    pub command: String,
    pub args: Vec<String>,
}

impl FileStorage {
    pub fn load_mcp_client_registrations(&self) -> Result<BTreeMap<String, McpClientLaunch>> {
        let conn = self.open_sqlite()?;
        Ok(sqlite::get_json(&conn, "mcp.client_registrations.v1")?.unwrap_or_default())
    }

    pub fn save_mcp_client_registrations(
        &self,
        registrations: &BTreeMap<String, McpClientLaunch>,
    ) -> Result<()> {
        let conn = self.open_sqlite()?;
        sqlite::put_json(&conn, "mcp.client_registrations.v1", registrations)
    }
}

// 查询会话与作用域标识（SQL 工作台通用）。
//
// `QuerySessionId` 标识一个可复用的查询会话（连接 + database/schema + 会话内事务/状态）；
// `QueryScopeKey` 提供 database(+schema) 级稳定标识，供 tab 去重、树节点、缓存、历史过滤、
// 异步 request key 复用同一份作用域，避免各处 `split('.')` 恢复对象名。

/// 一个可复用查询会话的稳定标识（进程内 / 连接生命周期内唯一）。
///
/// 仅作身份（不携带连接内容）；连接重连/重建会话可得新 ID，不跨进程持久化。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionId(pub u64);

impl std::fmt::Display for QuerySessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "q{}", self.0)
    }
}

impl QuerySessionId {
    /// 宿主托管会话（AI/Agent、MCP 等）使用的 ID 段起点。
    ///
    /// 编辑器查询标签从 1 起自增；两套自增若共用同一数值段，会在同一连接上命中
    /// 同一条后端会话，让 AI 的临时表/事务与编辑器互相串。宿主托管会话改用高位段，
    /// 与标签 ID 天然不重叠。
    pub const HOST_MANAGED_BASE: u64 = 1 << 40;

    /// 构造宿主托管会话 ID（`seq` 从 0 起，由调用方按会话计数分配）。
    pub fn host_managed(seq: u64) -> Self {
        Self(Self::HOST_MANAGED_BASE + seq)
    }

    /// 是否为宿主托管会话（AI/Agent、MCP 等），而非编辑器查询标签会话。
    pub fn is_host_managed(self) -> bool {
        self.0 >= Self::HOST_MANAGED_BASE
    }
}

/// 查询作用域：连接 + 可选 database + 可选 schema。
///
/// - PG 两段对象名是 `schema.table`，作用域内的 database 是物理连接库、schema 是 namespace。
/// - MySQL/TiDB 只有 database（schema 恒为 `None`）。
/// - Redis / SQLite 单库场景走 `database`（或 `None`）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryScope {
    pub connection_id: ConnectionId,
    pub database: Option<String>,
    pub schema: Option<String>,
}

impl QueryScope {
    /// 构造查询作用域。
    pub fn new(
        connection_id: ConnectionId,
        database: Option<String>,
        schema: Option<String>,
    ) -> Self {
        Self {
            connection_id,
            database,
            schema,
        }
    }

    /// 稳定、长度编码的字符串标识，供缓存 / 树节点 / 历史过滤复用。
    ///
    /// 采用长度前缀段（`conn`/`db`/`schema`），不以 `split('.')` 恢复对象名，
    /// 对象名含点号、空格、Unicode、双引号也不会产生歧义。
    pub fn key(&self) -> String {
        fn seg(prefix: &str, value: &str) -> String {
            format!("{}{}:{}:", prefix, value.len(), value)
        }
        let mut key = String::new();
        key.push_str(&seg("c", &self.connection_id.0.to_string()));
        if let Some(db) = &self.database {
            key.push_str(&seg("d", db));
        }
        if let Some(sch) = &self.schema {
            key.push_str(&seg("s", sch));
        }
        key
    }
}


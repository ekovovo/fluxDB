/// 连接发现只保存公开属性；返回列表不会扩大数据库 resolver 的执行范围。
struct AiConnectionsListTool {
    descriptor: fluxdb_core::ToolDescriptor,
    snapshot: serde_json::Value,
}

impl AiConnectionsListTool {
    fn new(
        connections: &[fluxdb_core::ConnectionConfig],
        current_connection: Option<fluxdb_core::ConnectionId>,
    ) -> Self {
        let connections = connections
            .iter()
            .map(|config| {
                serde_json::json!({
                    "connection_id": config.id.0,
                    "connection_name": config.name,
                    "database_kind": config.kind,
                    "connection_read_only": connection_is_read_only(config),
                    "is_current": Some(config.id) == current_connection,
                })
            })
            .collect::<Vec<_>>();
        Self {
            descriptor: fluxdb_core::ToolDescriptor {
                id: "connections.list".into(),
                version: 1,
                description: "列出本轮已保存连接的 ID、名称、数据库类型、只读标记及当前目标，不返回地址、路径或凭据。发现不授予执行权限：只能操作当前绑定连接，其他连接需用户在界面选择后重新发送。".into(),
                risk: fluxdb_core::ToolRisk::Read,
                input_schema_json: r#"{"type":"object","properties":{},"additionalProperties":false}"#.into(),
            },
            snapshot: serde_json::json!({
                "connections": connections,
                "snapshot_scope": "turn",
                "execution_scope": "current_connection_only",
            }),
        }
    }
}

impl fluxdb_core::AiTool for AiConnectionsListTool {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        _arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        Box::pin(async move {
            let started_at_ms = now_ms();
            let cancelled = ctx.cancel.is_cancelled();
            Ok(fluxdb_core::ToolReceipt {
                call_id: ctx.call_id,
                tool_id: self.descriptor.id.clone(),
                status: if cancelled {
                    fluxdb_core::ToolReceiptStatus::Cancelled
                } else {
                    fluxdb_core::ToolReceiptStatus::Succeeded
                },
                result_json: if cancelled {
                    serde_json::json!({"error": "连接发现已取消", "retryable": false}).to_string()
                } else {
                    self.snapshot.to_string()
                },
                error_code: cancelled.then(|| "cancelled".into()),
                affected_rows: None,
                started_at_ms,
                finished_at_ms: now_ms(),
            })
        })
    }
}

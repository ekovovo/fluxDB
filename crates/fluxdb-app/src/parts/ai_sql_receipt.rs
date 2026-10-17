impl SqlExecutionOutcome {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "columns": self.columns,
            "rows": self.rows,
            "row_count": self.row_count,
            "truncated": self.truncated,
            "acquisition_truncated": self.truncated,
            "elapsed_ms": self.elapsed_ms,
            "query_session_id": self.query_session_id.0,
            "source": match self.source {
                fluxdb_core::CommandExecutionSource::Ai => "ai",
                fluxdb_core::CommandExecutionSource::Workbench => "workbench",
                fluxdb_core::CommandExecutionSource::HistoryRerun => "history_rerun",
                fluxdb_core::CommandExecutionSource::KeyShortcut => "key_shortcut",
            },
            "original_sql": self.original_sql,
            "executed_sql": self.executed_sql,
            "progress": self.progress,
            "affected_rows": self.affected_rows,
            "statement_kind": self.statement_kind,
        })
    }

    /// 执行回执保留本次实际取得的全部行；发送模型前由通用预算层生成有界投影。
    pub fn to_model_json(&self, result_ref: Option<&crate::ResultRef>) -> serde_json::Value {
        let mut value = self.to_json();
        if let Some(object) = value.as_object_mut() {
            object.insert("rows_omitted".into(), serde_json::json!(0));
            if let Some(result_ref) = result_ref {
                object.insert("result_ref".into(), result_reference_json(result_ref));
            }
        }
        value
    }
}

// 内置 AI 插件声明文案与 Jev 状态的展示翻译。

/// 内置插件声明的稳定中文文案映射；未命中的外置插件原文保持不变。
fn localize_ai_plugin_text(text: &str) -> String {
    if let Some(detail) = text.strip_prefix("Jev 不可用：") {
        return tr("app.ai.plugins.text.jev_unavailable").replace(
            "%{detail}",
            &localize_ai_plugin_text(detail),
        );
    }
    if let Some(reason) = text.strip_prefix("Jev 决策连续失败已熔断：") {
        return tr("app.ai.plugins.text.jev_circuit_reason")
            .replace("%{reason}", &localize_ai_plugin_text(reason));
    }
    if let Some(detail) = text
        .strip_prefix("所选模型用途不匹配（")
        .and_then(|value| value.strip_suffix("）"))
    {
        return tr("app.ai.plugins.text.jev_model_purpose_mismatch")
            .replace("%{detail}", &localize_ai_plugin_text(detail));
    }
    if let Some(count) = text
        .strip_prefix("有 ")
        .and_then(|value| value.strip_suffix(" 个合格决策模型，请在「Jev 决策优化 · 管理」中选择决策模型"))
    {
        return tr("app.ai.plugins.text.jev_choose_model").replace("%{count}", count);
    }
    if let Some(count) = text
        .strip_prefix("已配置的 ")
        .and_then(|value| value.strip_suffix(" 个决策模型不支持该插件声明的决策协议"))
    {
        return tr("app.ai.plugins.text.jev_protocol_unsupported").replace("%{count}", count);
    }
    if let Some(error) = text.strip_prefix("不是有效声明：") {
        return tr("app.ai.plugins.text.declaration_not_valid")
            .replace("%{error}", error);
    }
    if let Some(error) = text.strip_prefix("manifest 不是有效 JSON：") {
        return tr("app.ai.plugins.text.manifest_invalid_json")
            .replace("%{error}", error);
    }
    if let Some(protocol) = text
        .strip_prefix("所选模型不支持 ")
        .and_then(|value| value.strip_suffix(" 协议"))
    {
        return tr("app.ai.plugins.text.selected_model_protocol_unsupported")
            .replace("%{protocol}", protocol);
    }
    if let Some(reason) = text.strip_prefix("选择的模型不满足插件要求：") {
        return tr("app.ai.plugins.text.selected_model_does_not_meet_requirements")
            .replace("%{reason}", reason);
    }
    if let Some(rest) = text.strip_prefix("字段 `") {
        if let Some((key, reason)) = rest.split_once("` 取值无效：") {
            return tr("app.ai.plugins.text.field_value_invalid")
                .replace("%{key}", key)
                .replace("%{reason}", reason);
        }
        if let Some(key) = rest.strip_suffix("` 与另一个字段占用同一个模型槽位") {
            return tr("app.ai.plugins.text.field_slot_conflict").replace("%{key}", key);
        }
    }
    if let Some(value) = text
        .strip_prefix("取值 `")
        .and_then(|value| value.strip_suffix("` 不在该字段的候选项里"))
    {
        return tr("app.ai.plugins.text.value_not_in_candidates").replace("%{text}", value);
    }
    if let Some(usage) = text
        .strip_prefix("只列出已启用、允许出站、凭据可用的")
        .and_then(|value| value.strip_suffix("。"))
    {
        return tr("app.ai.plugins.text.model_list_help")
            .replace("%{usage}", &localize_ai_plugin_text(usage));
    }
    let key = match text {
        "" => return String::new(),
        "当前界面与 @ 上下文" => "app.ai.plugins.text.context_ui_name",
        "当前页面、@ 对象摘要和主动细节读取" => "app.ai.plugins.text.context_ui_summary",
        "文件与附件" => "app.ai.plugins.text.files_name",
        "附件、授权目录、有界文件读取" => "app.ai.plugins.text.files_summary",
        "模型路由" => "app.ai.plugins.text.router_name",
        "Jev 决策优化" => "app.ai.plugins.text.jev_name",
        "可选分类、路由与相关性建议" => "app.ai.plugins.text.jev_summary",
        "上下文压缩" => "app.ai.plugins.text.compaction_name",
        "保护条目、卸载、摘要与 checkpoint" => "app.ai.plugins.text.compaction_summary",
        "调用前安全审计" => "app.ai.plugins.text.security_name",
        "调用前确定性风险/授权检查与审计" => "app.ai.plugins.text.security_summary",
        "AI 用量统计" => "app.ai.plugins.text.budget_name",
        "会话 Token 用量统计看板（待实现）" => "app.ai.plugins.text.budget_summary",
        "BI 与报表输出" => "app.ai.plugins.text.bi_name",
        "提供内置图表、表格与报告生成；关闭不隐藏产物工具，已完成报告仍可查看和导出" => {
            "app.ai.plugins.text.bi_summary"
        }
        "计划与执行进度" => "app.ai.plugins.text.progress_name",
        "公开计划、工具进展与检查点说明" => "app.ai.plugins.text.progress_summary",
        "生命周期诊断" => "app.ai.plugins.text.telemetry_name",
        "脱敏的阶段耗时/错误/诊断汇总" => "app.ai.plugins.text.telemetry_summary",
        "数据范围" => "app.ai.plugins.text.group_data_scope",
        "写入与结构" => "app.ai.plugins.text.group_writes_schema",
        "禁止访问的表" => "app.ai.plugins.text.field_denied_tables",
        "读写都拒绝。一行一个：裸名匹配任意 schema，schema.表名 只匹配该 schema。" => {
            "app.ai.plugins.text.help_denied_tables"
        }
        "例如 audit_logs 或 public.secrets" => "app.ai.plugins.text.placeholder_denied_tables",
        "允许访问的表" => "app.ai.plugins.text.field_allowed_tables",
        "填了就变成白名单：不在名单里的表一律拒绝，语句无法解析出表名时也拒绝。留空表示不限制。" => {
            "app.ai.plugins.text.help_allowed_tables"
        }
        "例如 orders\norder_items" => "app.ai.plugins.text.placeholder_allowed_tables",
        "只读保护的表" => "app.ai.plugins.text.field_protected_tables",
        "允许查询，但任何写入都拒绝。一行一个表名。" => {
            "app.ai.plugins.text.help_protected_tables"
        }
        "例如 customers" => "app.ai.plugins.text.placeholder_protected_tables",
        "禁止访问的列" => "app.ai.plugins.text.field_denied_columns",
        "语句里出现这些列即拒绝。裸列名匹配任意表，表名.列名 只匹配该表。" => {
            "app.ai.plugins.text.help_denied_columns"
        }
        "例如 api_token 或 app_users.password_hash" => {
            "app.ai.plugins.text.placeholder_denied_columns"
        }
        "禁止结构变更" => "app.ai.plugins.text.field_forbid_ddl",
        "打开后拒绝 CREATE / ALTER / DROP / TRUNCATE / GRANT 等结构与管理语句。" => {
            "app.ai.plugins.text.help_forbid_ddl"
        }
        "写操作必须带条件" => "app.ai.plugins.text.field_write_requires_where",
        "打开后，没有 WHERE 的 UPDATE / DELETE 转为待审批，而不是直接拒绝。" => {
            "app.ai.plugins.text.help_write_requires_where"
        }
        "禁止的 Redis 命令" => "app.ai.plugins.text.field_denied_redis_commands",
        "一行一个命令名，不区分大小写。覆盖内核拒绝列表之外的额外命令。" => {
            "app.ai.plugins.text.help_denied_redis_commands"
        }
        "例如 KEYS\nFLUSHALL" => "app.ai.plugins.text.placeholder_denied_redis_commands",
        "禁止的 Redis Key 前缀" => "app.ai.plugins.text.field_denied_redis_prefixes",
        "命令触碰任一以此开头的 Key 即拒绝；前缀按字面匹配。" => {
            "app.ai.plugins.text.help_denied_redis_prefixes"
        }
        "例如 prod:session:" => "app.ai.plugins.text.placeholder_denied_redis_prefixes",
        "未选择时 Jev 不参与运行，基础 Agent 不受影响。" => {
            "app.ai.plugins.text.jev_model_help"
        }
        "未选择时按确定性切点压缩，压缩与摘要照常运行；选择后仅影响哪些历史进入摘要。" => {
            "app.ai.plugins.text.compaction_model_help"
        }
        "未知失败" => "app.ai.plugins.text.status_unknown_failure",
        "没有实现" => "app.ai.plugins.text.status_not_implemented",
        "内核强制" => "app.ai.plugins.text.status_kernel_enforced",
        "行还在、能力却在跑" => "app.ai.plugins.text.status_orphan_running",
        "主模型" => "app.ai.plugins.text.role_main",
        "压缩模型" => "app.ai.plugins.text.role_compaction",
        "决策模型" => "app.ai.plugins.text.role_decision",
        "压缩决策模型" => "app.ai.plugins.text.role_compaction_decision",
        "生成模型" => "app.ai.plugins.text.role_generation",
        "模型" => "app.ai.plugins.text.role_model",
        "所选模型已停用" => "app.ai.plugins.text.jev_model_disabled",
        "所选模型缺少可用凭据" => "app.ai.plugins.text.jev_model_no_credential",
        "所选模型已禁止数据出站（可在模型设置中重新允许）" => {
            "app.ai.plugins.text.jev_model_outbound_blocked"
        }
        "摘要模型不可用" => "app.ai.plugins.text.jev_summary_model_unavailable",
        "Jev 熔断中，跳过本轮建议" => "app.ai.plugins.text.jev_circuit_open",
        "压缩决策模型熔断中，跳过本轮建议" => "app.ai.plugins.text.jev_compaction_circuit_open",
        "压缩决策模型未配置或不可用，按确定性切点压缩" => {
            "app.ai.plugins.text.jev_compaction_fallback"
        }
        "候选不足，无需决策" => "app.ai.plugins.text.jev_no_candidates",
        "任务已取消，跳过决策建议" => "app.ai.plugins.text.jev_task_cancelled",
        "配置声明无效" => "app.ai.plugins.text.config_declaration_invalid",
        "所选模型已被删除或不可用" => "app.ai.plugins.text.selected_model_deleted",
        "所选模型当前不可用（已停用、用途不符、禁止出站或缺少凭据）" => {
            "app.ai.plugins.text.selected_model_unavailable"
        }
        "所选模型已不是该插件的候选项" => "app.ai.plugins.text.selected_model_not_candidate",
        "插件实例不存在，无法保存配置" => "app.ai.plugins.text.plugin_instance_missing",
        _ => return text.to_string(),
    };
    tr(key).to_string()
}

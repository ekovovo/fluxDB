// 连接器与存储层错误的消息映射；未命中的原始报文保持原样。

fn localize_backend_external_error_text(text: &str) -> Option<String> {
    let key = match text {
        "连接类型不匹配" => "app.backend_error.ext_conn_type_mismatch",
        "缺少数据库名称" => "app.backend_error.ext_missing_database_name",
        "查询不能为空" => "app.backend_error.ext_query_empty",
        "数据库名称不能为空" => "app.backend_error.ext_database_name_required",
        "连接不匹配" => "app.backend_error.ext_connection_mismatch",
        "Redis Key 不存在" => "app.backend_error.ext_redis_key_missing",
        "Redis HPTTL 返回格式异常" => "app.backend_error.ext_redis_hpttl_invalid",
        "权限/授权对象名不合法" => "app.backend_error.ext_grant_object_invalid",
        "缺少数据库上下文" => "app.backend_error.ext_missing_database_context",
        "Redis List 操作缺少 Key" => "app.backend_error.ext_redis_list_key_missing",
        "Redis Hash 操作缺少 Key" => "app.backend_error.ext_redis_hash_key_missing",
        "角色与成员名不能为空" => "app.backend_error.ext_role_member_required",
        "缺少行身份条件，已取消提交" => "app.backend_error.ext_row_identity_condition_missing",
        "连接类型不支持新建数据库" => "app.backend_error.ext_create_database_unsupported",
        "仅表和视图支持数据读取" => "app.backend_error.ext_data_read_tables_views_only",
        "仅表和视图支持导出预览" => "app.backend_error.ext_export_preview_tables_views_only",
        "仅表和视图支持二进制读取" => "app.backend_error.ext_binary_read_tables_views_only",
        "二进制单元格为 NULL" => "app.backend_error.ext_binary_cell_null",
        "参数化执行只接受单条语句；不支持多语句脚本" => "app.backend_error.ext_parameterized_single_statement",
        "Redis XREVRANGE 返回格式异常" => "app.backend_error.ext_redis_xrevrange_invalid",
        "Redis 扫描返回格式异常" => "app.backend_error.ext_redis_scan_reply_invalid",
        "排序规则名称不合法" => "app.backend_error.ext_collation_invalid",
        "连接类型不支持删除数据库" => "app.backend_error.ext_drop_database_unsupported",
        "SQLite 文件路径不能为空" => "app.backend_error.ext_sqlite_path_required",
        "i64 参数不是合法的十进制文本" => "app.backend_error.ext_i64_decimal_invalid",
        "f64 参数必须是有限数值" => "app.backend_error.ext_f64_finite_required",
        "Redis ZSet 操作缺少 Key" => "app.backend_error.ext_redis_zset_key_missing",
        "Redis Stream 操作缺少 Key" => "app.backend_error.ext_redis_stream_key_missing",
        "Redis Stream 查询缺少 Key" => "app.backend_error.ext_redis_stream_query_key_missing",
        "Redis 返回格式异常" => "app.backend_error.ext_redis_reply_invalid",
        "Redis 范围返回格式异常" => "app.backend_error.ext_redis_range_reply_invalid",
        "列表已变化，请刷新后重试" => "app.backend_error.ext_list_changed",
        "Redis LSET 返回格式异常" => "app.backend_error.ext_redis_lset_invalid",
        "Redis Key 名称不能为空" => "app.backend_error.ext_redis_key_name_required",
        "Redis TTL 必须大于 0 秒或留空" => "app.backend_error.ext_redis_ttl_positive",
        "Redis EXPIRE 返回格式异常" => "app.backend_error.ext_redis_expire_invalid",
        "Redis 批量查询返回数量不匹配" => "app.backend_error.ext_redis_batch_count_mismatch",
        "Redis SCAN 返回格式异常" => "app.backend_error.ext_redis_scan_invalid",
        "Redis Set 操作缺少 Key" => "app.backend_error.ext_redis_set_key_missing",
        "Redis SREM 返回格式异常" => "app.backend_error.ext_redis_srem_invalid",
        "Redis SADD 返回格式异常" => "app.backend_error.ext_redis_sadd_invalid",
        "Redis SSCAN 返回格式异常" => "app.backend_error.ext_redis_sscan_invalid",
        "SSH 隧道配置缺少认证参数" => "app.backend_error.ext_ssh_auth_missing",
        "请填写 SSH 跳板机主机" => "app.backend_error.ext_ssh_jump_host_required",
        "Redis 连接需要 TCP 主机和端口" => "app.backend_error.ext_redis_tcp_required",
        "Redis HSET 返回格式异常" => "app.backend_error.ext_redis_hset_invalid",
        "Redis 字段不存在" => "app.backend_error.ext_redis_field_missing",
        "Redis HPERSIST 返回格式异常" => "app.backend_error.ext_redis_hpersist_invalid",
        "Redis HDEL 返回格式异常" => "app.backend_error.ext_redis_hdel_invalid",
        "Redis HPEXPIRE 返回格式异常" => "app.backend_error.ext_redis_hpexpire_invalid",
        "不支持的命令执行目标" => "app.backend_error.ext_command_target_unsupported",
        "行身份引用了不存在的列" => "app.backend_error.ext_row_identity_column_missing",
        "缺少 schema 上下文" => "app.backend_error.ext_missing_schema_context",
        "角色名不合法" => "app.backend_error.ext_role_name_invalid",
        "角色/成员名不合法" => "app.backend_error.ext_role_member_invalid",
        "查询会话已失效，执行结果请先核实；关闭标签后重新连接" => "app.backend_error.ext_query_session_expired",
        "只读连接禁止修改" => "app.backend_error.ext_read_only_connection",
        "有界写入提交结果未知，禁止自动重试" => "app.backend_error.ext_bounded_commit_unknown",
        "新增行字段数量和当前表结构不一致" => "app.backend_error.ext_insert_column_count_mismatch",
        "SQLite 连接需要数据库文件路径" => "app.backend_error.ext_sqlite_file_path_required",
        "SQLite 只读连接要求文件已存在" => "app.backend_error.ext_sqlite_readonly_file_exists",
        "SSH 认证未通过" => "app.backend_error.ext_ssh_auth_failed",
        "Redis Key 已存在" => "app.backend_error.ext_redis_key_exists",
        "目标 Redis Key 已存在" => "app.backend_error.ext_redis_target_key_exists",
        "Redis TTL 必须是秒数" => "app.backend_error.ext_redis_ttl_seconds",
        "Redis Key 元信息查询没有返回结果" => "app.backend_error.ext_redis_key_meta_missing",
        "订阅至少需要一个通道或 pattern" => "app.backend_error.ext_subscription_required",
        "订阅通道名不能为空" => "app.backend_error.ext_subscription_channel_required",
        "Redis VALUE_TOO_LARGE：回包超出解码预算，请缩小读取范围" => "app.backend_error.ext_redis_value_too_large",
        "Redis 集群重定向次数过多，请检查集群拓扑" => "app.backend_error.ext_redis_cluster_redirects",
        "Redis 地址解析失败" => "app.backend_error.ext_redis_address_invalid",
        "TLS 客户端证书与私钥需成对配置" => "app.backend_error.ext_tls_pair_required",
        "该值是二进制数据，无法在表格中编辑；请改用支持二进制的客户端写入" => "app.backend_error.ext_binary_edit_unsupported",
        "该值因超过 1MB 被截断，无法在表格中编辑；请改用支持大值的客户端写入" => "app.backend_error.ext_large_value_truncated",
        "元数据服务已关闭" => "app.backend_error.ext_metadata_service_closed",
        "PostgreSQL 建连超时（含传输与 TLS 握手）" => "app.backend_error.ext_pg_connect_timeout",
        "PostgreSQL 查询会话已达上限，请关闭不使用的查询标签" => "app.backend_error.ext_pg_session_limit",
        "探活超时" => "app.backend_error.ext_health_check_timeout",
        "查询超时" => "app.backend_error.ext_query_timeout",
        "SQL Server 查询超时，连接已关闭；执行结果待核实" => {
            "app.backend_error.sqlserver_query_timeout"
        }
        "查询已取消，SQL Server 连接已关闭；执行结果待核实" => {
            "app.backend_error.sqlserver_query_cancelled"
        }
        "SQL Server 查询会话已达上限，请关闭不使用的查询标签" => {
            "app.backend_error.sqlserver_session_limit"
        }
        "未找到目标行" => "app.backend_error.ext_target_row_missing",
        "未找到该对象（可能已被删除或不在当前 schema）" => "app.backend_error.ext_object_not_found",
        "连接类型不支持角色管理" => "app.backend_error.ext_role_management_unsupported",
        "连接类型不支持对象权限" => "app.backend_error.ext_object_privileges_unsupported",
        "连接类型不支持角色有效权限" => "app.backend_error.ext_effective_role_privileges_unsupported",
        "schema 名称不能为空" => "app.backend_error.ext_schema_name_required",
        "schema 名称不合法" => "app.backend_error.ext_schema_name_invalid",
        "取消连接超时" => "app.backend_error.ext_cancel_connect_timeout",
        "已取消排队执行" => "app.backend_error.ext_queued_execution_cancelled",
        "取消请求未确认，会话已关闭；执行结果待核实，请勿直接重试写操作" => "app.backend_error.ext_cancel_unconfirmed",
        "取消收尾超时，会话已关闭；执行结果待核实" => "app.backend_error.ext_cancel_cleanup_timeout",
        "自动审批写入超限或已取消，事务已回滚" => "app.backend_error.ext_auto_approval_write_rolled_back",
        "有界写入回滚结果无法核实，禁止自动重试" => "app.backend_error.ext_bounded_rollback_unknown",
        "仅表对象支持数据提交" => "app.backend_error.ext_data_commit_tables_only",
        "仅表对象支持数据读取" => "app.backend_error.ext_data_read_table_only",
        "仅表和视图支持导出" => "app.backend_error.ext_export_tables_views_only",
        "产物已被其他写入者修改，请重新加载" => "app.backend_error.ext_artifact_modified",
        "模型不存在" => "app.backend_error.ext_model_missing",
        "配置已被其他窗口修改，请重新加载" => "app.backend_error.ext_config_modified",
        "结果元数据不是合法 JSON" => "app.backend_error.ext_result_meta_invalid_json",
        "非法结果状态" => "app.backend_error.ext_result_state_invalid",
        "保留结果的路径必须是受控根内的相对路径" => "app.backend_error.ext_retained_path_relative",
        "Agent 已被其他窗口修改，请重新加载" => "app.backend_error.ext_agent_modified",
        "Agent 不存在" => "app.backend_error.ext_agent_missing",
        "任务快照不是合法 JSON" => "app.backend_error.ext_task_snapshot_invalid_json",
        "插件配置已被其他窗口修改，请重新加载" => "app.backend_error.ext_plugin_config_modified",
        "会话不存在" => "app.backend_error.ext_session_missing",
        "模型绑定已变更，请重新加载" => "app.backend_error.ext_model_binding_changed",
        "工具 ID 不合法" => "app.backend_error.ext_tool_id_invalid",
        "无法解析系统应用数据目录" => "app.backend_error.ext_app_data_dir_unresolved",
        "连接文件路径包含非 UTF-8 字符，无法落盘" => "app.backend_error.ext_connection_path_non_utf8",
        "MCP session 不存在" => "app.backend_error.ext_mcp_session_missing",
        "MCP session 不存在或不属于该 Profile" => "app.backend_error.ext_mcp_session_wrong_profile",
        "MCP request 不存在" => "app.backend_error.ext_mcp_request_missing",
        "MCP request 状态已变化，无法进入运行中" => "app.backend_error.ext_mcp_request_state_changed",
        "MCP 回执不是合法 JSON" => "app.backend_error.ext_mcp_receipt_invalid_json",
        "MCP request 已结束或状态已变化" => "app.backend_error.ext_mcp_request_finished",
        "非法结果保留方式" => "app.backend_error.ext_retention_mode_invalid",
        "retained 结果必须有相对路径" => "app.backend_error.ext_retained_result_path",
        "Agent ID 与名称不能为空" => "app.backend_error.ext_agent_id_name_required",
        "内置 Agent 不允许停用" => "app.backend_error.ext_builtin_agent_disable_forbidden",
        "内置 Agent 不允许删除" => "app.backend_error.ext_builtin_agent_delete_forbidden",
        "Agent 有正在运行、待审批或结果未知的任务，不能删除" => "app.backend_error.ext_agent_has_active_tasks",
        "压缩触发比例必须在 10–95% 之间" => "app.backend_error.ext_compaction_ratio_invalid",
        "最大模型迭代次数必须不小于 1" => "app.backend_error.ext_max_model_iterations_invalid",
        "提示词超过 64 KiB 上限" => "app.backend_error.ext_prompt_too_large",
        "Agent 已存在" => "app.backend_error.ext_agent_exists",
        "非法连接状态" => "app.backend_error.ext_connection_state_invalid",
        "达梦暂不支持从备份恢复" => "app.backend_error.ext_dm_restore_unsupported",
        "达梦数据库名缺失" => "app.backend_error.ext_dm_database_name_missing",
        "达梦不支持 SQLite 快照备份" => {
            "app.backend_error.ext_dm_sqlite_snapshot_unsupported"
        }
        "达梦视图 DDL 返回类型异常" => "app.backend_error.ext_dm_view_ddl_type_invalid",
        "达梦触发器元数据需要连接配置上下文" => "app.backend_error.ext_dm_trigger_connection_missing",
        "达梦触发器元数据返回类型异常" => "app.backend_error.ext_dm_trigger_metadata_invalid",
        "达梦触发器元数据缺少名称" => "app.backend_error.ext_dm_trigger_name_missing",
        "未读取到达梦视图定义" => "app.backend_error.ext_dm_view_definition_missing",
        "数据库中没有可备份的用户表" => "app.backend_error.ext_dm_no_backup_tables",
        "数据库中没有可备份的表或视图" => "app.backend_error.ext_dm_no_backup_objects",
        "达梦当前只支持整 schema 恢复，不支持逐表选择" => {
            "app.backend_error.ext_dm_restore_per_table_unsupported"
        }
        "达梦备份包含多个 schema，当前只支持单 schema 恢复" => {
            "app.backend_error.ext_dm_restore_multiple_schemas"
        }
        "达梦恢复只支持整 schema 导入；目标 schema 必须为空，失败可能留下部分对象。" => {
            "app.backend_error.ext_dm_restore_schema_only"
        }
        "恢复不会切换数据库或自动删除目标 schema。" => {
            "app.backend_error.ext_dm_restore_no_switch"
        }
        "达梦恢复脚本包含当前不支持的语句；请人工检查" => {
            "app.backend_error.ext_dm_restore_stmt_unsupported"
        }
        "达梦恢复脚本包含身份或实例级语句，禁止自动恢复" => {
            "app.backend_error.ext_dm_restore_identity_forbidden"
        }
        "达梦连接档案缺失" => "app.backend_error.ext_dm_restore_profile_missing",
        "目标 schema 名称无效" => "app.backend_error.ext_dm_restore_schema_invalid",
        "达梦恢复完成，但目标对象核对失败" => {
            "app.backend_error.ext_dm_restore_verify_failed"
        }
        "恢复后未找到目标表" => "app.backend_error.ext_dm_restore_table_missing",
        "数据包含未加载的二进制值或非有限浮点数，请改用原生备份" => {
            "app.backend_error.ext_backup_unloaded_binary"
        }
        "当前连接类型没有可导入的表数据" => "app.backend_error.app_msg_import_unsupported",
        "仅表对象支持数据导入" => "app.backend_error.app_msg_import_tables_only",
        "导入源不是普通文件" => "app.backend_error.app_msg_import_source_not_file",
        "导入文件超过 64 MiB，请拆分后重试" => "app.backend_error.app_msg_import_file_too_large",
        "导入文件必须是 UTF-8 文本" => "app.backend_error.app_msg_import_utf8",
        "导入文件没有字段" => "app.backend_error.app_msg_import_no_columns",
        "导入字段名不能为空" => "app.backend_error.app_msg_import_column_empty",
        "目标表没有可导入字段" => "app.backend_error.app_msg_import_target_no_columns",
        "导入文件没有可写入字段" => "app.backend_error.app_msg_import_no_writable_columns",
        "导入行数或字段数超过单次上限，请拆分文件后重试" => {
            "app.backend_error.app_msg_import_too_many"
        }
        "导入行数超过单次上限，请拆分文件后重试" => {
            "app.backend_error.app_msg_import_too_many"
        }
        "导入数据包含非有限浮点数" => "app.backend_error.app_msg_import_finite_float",
        "导入整数值格式无效" => "app.backend_error.app_msg_import_invalid_integer",
        "导入浮点值格式无效" => "app.backend_error.app_msg_import_invalid_float",
        "导入布尔值格式无效" => "app.backend_error.app_msg_import_invalid_bool",
        "导入二进制值格式无效" => "app.backend_error.app_msg_import_invalid_binary",
        "导入不支持未完整加载的二进制字段" => {
            "app.backend_error.app_msg_import_binary_unsupported"
        }
        "导入已取消" => "app.backend_error.app_msg_import_cancelled",
        "导入执行器与连接类型不匹配" => "app.backend_error.app_msg_import_kind_mismatch",
        "JSON 导入文件顶层必须是数组" => "app.backend_error.app_msg_import_json_top_array",
        "JSON 导入文件必须包含对象数组" => {
            "app.backend_error.app_msg_import_json_object_rows"
        }
        "XML 导入文件没有 row 节点" => "app.backend_error.app_msg_import_xml_no_rows",
        "field 缺少 name 属性" => "app.backend_error.app_msg_import_xml_field_name",
        "字段数与首行不一致" => "app.backend_error.app_msg_import_xml_field_count",
        _ => return None,
    };
    Some(tr(key).to_string())
}

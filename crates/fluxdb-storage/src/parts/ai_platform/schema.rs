//! AI 管理与运行实体的建表语句（设计文档 §42 的唯一代码来源）。
//!
//! 首次打开数据库时由 [`crate::sqlite::create_schema`] 幂等创建；已知旧列与面板
//! 偏好约束由 `ensure_schema` 增量升级。`CREATE TABLE IF NOT EXISTS` 不会重建
//! 未知结构，由 [`verify_ai_schema`] 检出并报错。

/// 管理实体 DDL（模型 / Agent / 提示词 / 插件 / 绑定 / MCP / 预算 / 运行设置）。
pub const MANAGEMENT_SQL: &str = r##"CREATE TABLE IF NOT EXISTS ai_model_profiles (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    provider TEXT NOT NULL,
    api_protocol TEXT NOT NULL,
    model_kind TEXT NOT NULL CHECK (model_kind IN ('generation','decision')),
    model TEXT NOT NULL,
    endpoint TEXT,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    auth_mode TEXT NOT NULL CHECK (auth_mode IN ('none','api_key')),
    context_window_tokens INTEGER CHECK (context_window_tokens IS NULL OR context_window_tokens >= 1024),
    max_output_tokens INTEGER CHECK (max_output_tokens IS NULL OR max_output_tokens >= 256),
    capabilities_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(capabilities_json)),
    provider_options_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(provider_options_json)),
    input_price_micros_per_million INTEGER CHECK (input_price_micros_per_million >= 0),
    output_price_micros_per_million INTEGER CHECK (output_price_micros_per_million >= 0),
    cached_input_price_micros_per_million INTEGER CHECK (cached_input_price_micros_per_million >= 0),
    price_metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(price_metadata_json)),
    credential_ciphertext BLOB,
    credential_nonce BLOB,
    cipher_version INTEGER,
    key_id TEXT,
    secret_revision INTEGER NOT NULL DEFAULT 0 CHECK (secret_revision >= 0),
    connection_status TEXT NOT NULL DEFAULT 'unknown' CHECK (connection_status IN ('unknown','ok','failed')),
    connection_checked_at_ms INTEGER,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    CHECK (
      (credential_ciphertext IS NULL AND credential_nonce IS NULL
       AND cipher_version IS NULL AND key_id IS NULL)
      OR
      (credential_ciphertext IS NOT NULL AND typeof(credential_ciphertext)='blob'
       AND length(credential_ciphertext) >= 16
       AND credential_nonce IS NOT NULL AND typeof(credential_nonce)='blob'
       AND length(credential_nonce)=12 AND cipher_version IS NOT NULL
       AND cipher_version >= 1 AND key_id IS NOT NULL AND length(key_id)>0
       AND secret_revision >= 1)
    )
);
CREATE INDEX IF NOT EXISTS ai_models_kind_enabled ON ai_model_profiles(model_kind, enabled);

CREATE TABLE IF NOT EXISTS ai_budget_policies (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    session_limit_micros INTEGER CHECK (session_limit_micros >= 0),
    daily_limit_micros INTEGER CHECK (daily_limit_micros >= 0),
    request_limit_micros INTEGER CHECK (request_limit_micros >= 0),
    max_iterations INTEGER NOT NULL DEFAULT 40 CHECK (max_iterations > 0),
    max_wall_time_ms INTEGER NOT NULL DEFAULT 600000 CHECK (max_wall_time_ms > 0),
    max_model_concurrency INTEGER NOT NULL DEFAULT 2 CHECK (max_model_concurrency > 0),
    options_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(options_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);


CREATE TABLE IF NOT EXISTS ai_agents (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0,1)),
    default_model_profile_id TEXT REFERENCES ai_model_profiles(id) ON DELETE RESTRICT,
    system_prompt TEXT NOT NULL DEFAULT '',
    budget_policy_id TEXT REFERENCES ai_budget_policies(id) ON DELETE RESTRICT,
    allowed_tools_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(allowed_tools_json)),
    permissions_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(permissions_json)),
    options_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(options_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS ai_plugin_packages (
    id TEXT PRIMARY KEY,
    package_key TEXT NOT NULL,
    version TEXT NOT NULL,
    runtime_kind TEXT NOT NULL CHECK (runtime_kind IN ('builtin','wasm')),
    digest TEXT NOT NULL,
    host_api_min INTEGER NOT NULL CHECK (host_api_min >= 1),
    host_api_max INTEGER NOT NULL CHECK (host_api_max >= host_api_min),
    manifest_json TEXT NOT NULL CHECK (json_valid(manifest_json)),
    relative_path TEXT,
    status TEXT NOT NULL CHECK (status IN ('staged','active','disabled','failed')),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(package_key, version, digest),
    CHECK (runtime_kind <> 'wasm' OR relative_path IS NOT NULL)
);
CREATE TABLE IF NOT EXISTS ai_plugin_instances (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    package_id TEXT NOT NULL REFERENCES ai_plugin_packages(id) ON DELETE RESTRICT,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    config_schema_version INTEGER NOT NULL DEFAULT 1 CHECK (config_schema_version >= 1),
    config_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(config_json)),
    granted_permissions_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(granted_permissions_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS ai_agent_plugin_bindings (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL REFERENCES ai_agents(id) ON DELETE RESTRICT,
    plugin_instance_id TEXT NOT NULL REFERENCES ai_plugin_instances(id) ON DELETE RESTRICT,
    binding_source TEXT NOT NULL DEFAULT 'user' CHECK (binding_source IN ('user','system')),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    priority INTEGER NOT NULL DEFAULT 100,
    capabilities_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(capabilities_json)),
    failure_policy TEXT NOT NULL DEFAULT 'best_effort'
        CHECK (failure_policy IN ('best_effort','required')),
    options_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(options_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(agent_id, plugin_instance_id)
);
CREATE INDEX IF NOT EXISTS ai_bindings_order ON ai_agent_plugin_bindings(agent_id, enabled, priority, id);

CREATE TABLE IF NOT EXISTS ai_mcp_settings (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    transport TEXT NOT NULL CHECK (transport IN ('stdio','http')),
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0,1)),
    config_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(config_json)),
    tool_filter_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(tool_filter_json)),
    permissions_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(permissions_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS ai_mcp_server_profiles (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0,1)),
    transport TEXT NOT NULL DEFAULT 'stdio' CHECK (transport='stdio'),
    write_mode TEXT NOT NULL DEFAULT 'disabled'
        CHECK (write_mode IN ('disabled','scoped','desktop_approval')),
    allowed_tool_ids_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(allowed_tool_ids_json)),
    options_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(options_json)),
    max_inflight INTEGER NOT NULL DEFAULT 4 CHECK (max_inflight > 0),
    max_request_ms INTEGER NOT NULL DEFAULT 30000 CHECK (max_request_ms > 0),
    max_request_bytes INTEGER NOT NULL DEFAULT 262144 CHECK (max_request_bytes > 0),
    max_response_bytes INTEGER NOT NULL DEFAULT 131072 CHECK (max_response_bytes > 0),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS ai_mcp_server_resource_grants (
    id TEXT PRIMARY KEY,
    profile_id TEXT NOT NULL REFERENCES ai_mcp_server_profiles(id) ON DELETE RESTRICT,
    connection_id INTEGER NOT NULL,
    database_name TEXT NOT NULL,
    namespace TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    allow_read INTEGER NOT NULL DEFAULT 1 CHECK (allow_read IN (0,1)),
    allow_write INTEGER NOT NULL DEFAULT 0 CHECK (allow_write IN (0,1)),
    object_policy_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(object_policy_json)),
    write_policy_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(write_policy_json)),
    max_rows INTEGER NOT NULL DEFAULT 200 CHECK (max_rows BETWEEN 1 AND 1000),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(profile_id, connection_id, database_name, namespace),
    UNIQUE(profile_id, id)
);
CREATE INDEX IF NOT EXISTS ai_mcp_server_grants_profile
    ON ai_mcp_server_resource_grants(profile_id, enabled);

CREATE TABLE IF NOT EXISTS ai_agent_model_bindings (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL REFERENCES ai_agents(id) ON DELETE RESTRICT,
    model_profile_id TEXT NOT NULL REFERENCES ai_model_profiles(id) ON DELETE RESTRICT,
    role TEXT NOT NULL CHECK (role IN ('main_candidate','compaction','artifact','decision')),
    binding_source TEXT NOT NULL DEFAULT 'user' CHECK (binding_source IN ('user','system')),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    priority INTEGER NOT NULL DEFAULT 100,
    options_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(options_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(agent_id, role, model_profile_id)
);
CREATE INDEX IF NOT EXISTS ai_agent_models_role
    ON ai_agent_model_bindings(agent_id, role, enabled, priority);
CREATE TABLE IF NOT EXISTS ai_agent_mcp_bindings (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL REFERENCES ai_agents(id) ON DELETE RESTRICT,
    mcp_setting_id TEXT NOT NULL REFERENCES ai_mcp_settings(id) ON DELETE RESTRICT,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    tool_filter_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(tool_filter_json)),
    permissions_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(permissions_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(agent_id, mcp_setting_id)
);

CREATE TABLE IF NOT EXISTS ai_runtime_settings (
    id INTEGER PRIMARY KEY CHECK (id=1),
    default_agent_id TEXT REFERENCES ai_agents(id) ON DELETE RESTRICT,
    default_model_profile_id TEXT,
    default_decision_model_profile_id TEXT REFERENCES ai_model_profiles(id) ON DELETE RESTRICT,
    max_active_turns INTEGER NOT NULL DEFAULT 2 CHECK (max_active_turns > 0),
    context_policy_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(context_policy_json)),
    privacy_policy_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(privacy_policy_json)),
    retention_policy_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(retention_policy_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS ai_integration_secrets (
    id TEXT PRIMARY KEY,
    plugin_instance_id TEXT REFERENCES ai_plugin_instances(id) ON DELETE CASCADE,
    mcp_setting_id TEXT REFERENCES ai_mcp_settings(id) ON DELETE CASCADE,
    secret_kind TEXT NOT NULL,
    ciphertext BLOB NOT NULL CHECK (typeof(ciphertext)='blob' AND length(ciphertext) >= 16),
    nonce BLOB NOT NULL CHECK (typeof(nonce)='blob' AND length(nonce)=12),
    cipher_version INTEGER NOT NULL CHECK (cipher_version >= 1),
    key_id TEXT NOT NULL,
    secret_revision INTEGER NOT NULL CHECK (secret_revision >= 1),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    CHECK ((plugin_instance_id IS NULL) <> (mcp_setting_id IS NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS ai_plugin_secret_kind
    ON ai_integration_secrets(plugin_instance_id, secret_kind) WHERE plugin_instance_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS ai_mcp_secret_kind
    ON ai_integration_secrets(mcp_setting_id, secret_kind) WHERE mcp_setting_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS ai_knowledge_entries (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    kind TEXT NOT NULL,
    connection_id INTEGER,
    database_name TEXT,
    namespace TEXT,
    agent_id TEXT REFERENCES ai_agents(id) ON DELETE RESTRICT,
    scope_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(scope_json)),
    content_json TEXT NOT NULL CHECK (json_valid(content_json)),
    source_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(source_json)),
    status TEXT NOT NULL CHECK (status IN ('proposed','confirmed','retired')),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ai_knowledge_kind_status ON ai_knowledge_entries(kind, status);"##;

/// 运行实体 DDL（会话 / 轮次 / 消息 / 工具 / 审计 / 结果 / 产物 / 计量 / 面板偏好）。
pub const RUNTIME_SQL: &str = r##"CREATE TABLE IF NOT EXISTS ai_mcp_server_sessions (
    id TEXT PRIMARY KEY,
    profile_id TEXT NOT NULL REFERENCES ai_mcp_server_profiles(id) ON DELETE RESTRICT,
    client_info_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(client_info_json)),
    protocol_version TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('connected','disconnected','expired','failed')),
    next_request_seq INTEGER NOT NULL DEFAULT 1 CHECK (next_request_seq >= 1),
    started_at_ms INTEGER NOT NULL,
    last_seen_at_ms INTEGER NOT NULL,
    closed_at_ms INTEGER,
    UNIQUE(profile_id, id)
);
CREATE TABLE IF NOT EXISTS ai_mcp_server_requests (
    id TEXT PRIMARY KEY,
    profile_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    seq INTEGER NOT NULL CHECK (seq >= 1),
    protocol_request_id_json TEXT NOT NULL CHECK (json_valid(protocol_request_id_json)),
    tool_id TEXT NOT NULL,
    tool_version INTEGER NOT NULL CHECK (tool_version >= 1),
    resource_grant_id TEXT,
    target_json TEXT NOT NULL CHECK (json_valid(target_json)),
    arguments_redacted_json TEXT NOT NULL CHECK (json_valid(arguments_redacted_json)),
    plan_hash TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN (
        'prepared','running','waiting_approval','succeeded','failed','denied','cancelled','unknown')),
    receipt_json TEXT CHECK (receipt_json IS NULL OR json_valid(receipt_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    started_at_ms INTEGER NOT NULL,
    finished_at_ms INTEGER,
    FOREIGN KEY(profile_id, session_id)
        REFERENCES ai_mcp_server_sessions(profile_id, id) ON DELETE RESTRICT,
    FOREIGN KEY(profile_id, resource_grant_id)
        REFERENCES ai_mcp_server_resource_grants(profile_id, id) ON DELETE RESTRICT,
    UNIQUE(session_id, seq),
    UNIQUE(session_id, id)
);
CREATE INDEX IF NOT EXISTS ai_mcp_server_requests_status
    ON ai_mcp_server_requests(profile_id, status, started_at_ms);

CREATE TABLE IF NOT EXISTS ai_sessions (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL REFERENCES ai_agents(id) ON DELETE RESTRICT,
    title TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('active','archived')),
    next_message_seq INTEGER NOT NULL DEFAULT 1 CHECK (next_message_seq >= 1),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ai_sessions_recent ON ai_sessions(status, updated_at_ms DESC);
CREATE TABLE IF NOT EXISTS ai_context_references (
    scope_id TEXT NOT NULL,
    session_id TEXT REFERENCES ai_sessions(id) ON DELETE CASCADE,
    reference_id TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
    PRIMARY KEY(scope_id,reference_id),
    CHECK ((session_id IS NULL AND scope_id='draft') OR scope_id=session_id)
);
CREATE TABLE IF NOT EXISTS ai_session_model_selections (
    scope_id TEXT PRIMARY KEY,
    session_id TEXT REFERENCES ai_sessions(id) ON DELETE CASCADE,
    model_profile_id TEXT REFERENCES ai_model_profiles(id) ON DELETE RESTRICT,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    CHECK ((session_id IS NULL AND scope_id='draft') OR scope_id=session_id)
);
CREATE TABLE IF NOT EXISTS ai_turns (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES ai_sessions(id) ON DELETE RESTRICT,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 1),
    status TEXT NOT NULL CHECK (status IN (
        'queued','preparing','generating','executing_tools','waiting_approval','waiting_user',
        'cancel_requested','cancelled','completed','failed','interrupted','outcome_unknown')),
    target_json TEXT NOT NULL CHECK (json_valid(target_json)),
    runtime_snapshot_json TEXT NOT NULL CHECK (json_valid(runtime_snapshot_json)),
    iteration_count INTEGER NOT NULL DEFAULT 0 CHECK (iteration_count >= 0),
    next_event_seq INTEGER NOT NULL DEFAULT 1 CHECK (next_event_seq >= 1),
    error_code TEXT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    started_at_ms INTEGER NOT NULL,
    finished_at_ms INTEGER,
    UNIQUE(session_id, ordinal),
    UNIQUE(session_id, id)
);
CREATE UNIQUE INDEX IF NOT EXISTS ai_one_active_turn_per_session ON ai_turns(session_id)
WHERE status IN ('queued','preparing','generating','executing_tools','waiting_approval',
                 'waiting_user','cancel_requested');
CREATE TABLE IF NOT EXISTS ai_messages (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES ai_sessions(id) ON DELETE RESTRICT,
    turn_id TEXT,
    seq INTEGER NOT NULL CHECK (seq >= 1),
    role TEXT NOT NULL,
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    persistence_kind TEXT NOT NULL CHECK (persistence_kind IN ('final','checkpoint')),
    redaction_version INTEGER NOT NULL DEFAULT 1 CHECK (redaction_version >= 1),
    created_at_ms INTEGER NOT NULL,
    UNIQUE(session_id, seq),
    UNIQUE(session_id, id),
    FOREIGN KEY(session_id, turn_id) REFERENCES ai_turns(session_id, id) ON DELETE RESTRICT
);
CREATE TABLE IF NOT EXISTS ai_message_parts (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL REFERENCES ai_messages(id) ON DELETE RESTRICT,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    kind TEXT NOT NULL CHECK (kind IN (
        'text','reasoning_summary','plan','context_ref','tool_call_ref','tool_result_ref','artifact_ref','notice')),
    status TEXT NOT NULL CHECK (status IN ('pending','streaming','complete','interrupted','error')),
    schema_version INTEGER NOT NULL DEFAULT 1 CHECK (schema_version >= 1),
    payload_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(payload_json)),
    tool_call_id TEXT REFERENCES ai_tool_calls(id) ON DELETE RESTRICT,
    result_id TEXT REFERENCES ai_results(id) ON DELETE RESTRICT,
    artifact_id TEXT REFERENCES ai_artifacts(id) ON DELETE RESTRICT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(message_id, ordinal),
    CHECK (kind NOT IN ('tool_call_ref','tool_result_ref') OR tool_call_id IS NOT NULL),
    CHECK (kind <> 'artifact_ref' OR artifact_id IS NOT NULL)
);
CREATE INDEX IF NOT EXISTS ai_parts_message_order ON ai_message_parts(message_id, ordinal);
CREATE TABLE IF NOT EXISTS ai_session_ui_state (
    session_id TEXT PRIMARY KEY REFERENCES ai_sessions(id) ON DELETE RESTRICT,
    selected_turn_id TEXT,
    expanded_part_ids_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(expanded_part_ids_json)),
    scroll_anchor_message_id TEXT,
    ui_options_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(ui_options_json)),
    draft_json TEXT CHECK (draft_json IS NULL OR json_valid(draft_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY(session_id, selected_turn_id)
        REFERENCES ai_turns(session_id, id) ON DELETE RESTRICT,
    FOREIGN KEY(session_id, scroll_anchor_message_id)
        REFERENCES ai_messages(session_id, id) ON DELETE RESTRICT
);
CREATE TABLE IF NOT EXISTS ai_tool_calls (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    iteration INTEGER NOT NULL CHECK (iteration >= 1),
    source_ordinal INTEGER NOT NULL CHECK (source_ordinal >= 0),
    provider_call_id TEXT NOT NULL,
    tool_id TEXT NOT NULL,
    tool_version INTEGER NOT NULL CHECK (tool_version >= 1),
    plugin_digest TEXT,
    arguments_redacted_json TEXT NOT NULL CHECK (json_valid(arguments_redacted_json)),
    target_json TEXT NOT NULL CHECK (json_valid(target_json)),
    plan_hash TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN (
        'prepared','waiting_approval','running','succeeded','failed','denied','cancelled','unknown')),
    receipt_json TEXT CHECK (receipt_json IS NULL OR json_valid(receipt_json)),
    started_at_ms INTEGER,
    finished_at_ms INTEGER,
    FOREIGN KEY(session_id, turn_id) REFERENCES ai_turns(session_id, id) ON DELETE RESTRICT,
    UNIQUE(turn_id, iteration, provider_call_id),
    UNIQUE(turn_id, iteration, source_ordinal)
);
CREATE INDEX IF NOT EXISTS ai_calls_turn_status ON ai_tool_calls(turn_id, status);
CREATE TABLE IF NOT EXISTS ai_approvals (
    id TEXT PRIMARY KEY,
    tool_call_id TEXT REFERENCES ai_tool_calls(id) ON DELETE RESTRICT,
    mcp_request_id TEXT REFERENCES ai_mcp_server_requests(id) ON DELETE RESTRICT,
    plan_hash TEXT NOT NULL,
    scope_json TEXT NOT NULL CHECK (json_valid(scope_json)),
    status TEXT NOT NULL CHECK (status IN ('pending','approved','denied','expired','invalidated')),
    reason_code TEXT,
    created_at_ms INTEGER NOT NULL,
    decided_at_ms INTEGER,
    expires_at_ms INTEGER,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    CHECK ((tool_call_id IS NULL) <> (mcp_request_id IS NULL)),
    UNIQUE(tool_call_id, plan_hash),
    UNIQUE(mcp_request_id, plan_hash)
);
CREATE TABLE IF NOT EXISTS ai_tool_audits (
    id TEXT PRIMARY KEY,
    tool_call_id TEXT REFERENCES ai_tool_calls(id) ON DELETE RESTRICT,
    mcp_request_id TEXT REFERENCES ai_mcp_server_requests(id) ON DELETE RESTRICT,
    stage TEXT NOT NULL CHECK (stage IN ('pre_execute','revalidate')),
    hook_id TEXT NOT NULL,
    hook_version TEXT NOT NULL,
    ruleset_version TEXT NOT NULL,
    plan_hash TEXT NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN ('allow','deny','require_approval')),
    risk TEXT NOT NULL,
    rule_ids_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(rule_ids_json)),
    reason_codes_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(reason_codes_json)),
    created_at_ms INTEGER NOT NULL,
    CHECK ((tool_call_id IS NULL) <> (mcp_request_id IS NULL))
);
CREATE INDEX IF NOT EXISTS ai_audits_tool_time ON ai_tool_audits(tool_call_id, created_at_ms);
CREATE INDEX IF NOT EXISTS ai_audits_mcp_time ON ai_tool_audits(mcp_request_id, created_at_ms);

CREATE TABLE IF NOT EXISTS ai_run_events (
    turn_id TEXT NOT NULL REFERENCES ai_turns(id) ON DELETE RESTRICT,
    seq INTEGER NOT NULL CHECK (seq >= 1),
    kind TEXT NOT NULL,
    summary_json TEXT NOT NULL CHECK (json_valid(summary_json)),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(turn_id, seq)
);

CREATE TABLE IF NOT EXISTS ai_results (
    id TEXT PRIMARY KEY,
    session_id TEXT,
    turn_id TEXT,
    source_call_id TEXT REFERENCES ai_tool_calls(id) ON DELETE RESTRICT,
    mcp_session_id TEXT REFERENCES ai_mcp_server_sessions(id) ON DELETE RESTRICT,
    mcp_request_id TEXT,
    metadata_json TEXT NOT NULL CHECK (json_valid(metadata_json)),
    retention_kind TEXT NOT NULL CHECK (retention_kind IN ('memory','retained')),
    status TEXT NOT NULL CHECK (status IN ('available','expired','deleted','failed')),
    relative_path TEXT,
    byte_count INTEGER NOT NULL DEFAULT 0 CHECK (byte_count >= 0),
    content_hash TEXT,
    created_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER,
    FOREIGN KEY(session_id, turn_id) REFERENCES ai_turns(session_id, id) ON DELETE RESTRICT,
    FOREIGN KEY(mcp_session_id, mcp_request_id)
        REFERENCES ai_mcp_server_requests(session_id, id) ON DELETE RESTRICT,
    CHECK (
        (session_id IS NOT NULL AND turn_id IS NOT NULL
         AND mcp_session_id IS NULL AND mcp_request_id IS NULL)
        OR
        (session_id IS NULL AND turn_id IS NULL AND source_call_id IS NULL
         AND mcp_session_id IS NOT NULL AND mcp_request_id IS NOT NULL)
    ),
    CHECK (retention_kind <> 'retained' OR relative_path IS NOT NULL)
);
CREATE TABLE IF NOT EXISTS ai_artifacts (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
    title TEXT NOT NULL,
    renderer_id TEXT NOT NULL,
    renderer_version TEXT NOT NULL,
    spec_json TEXT NOT NULL CHECK (json_valid(spec_json)),
    status TEXT NOT NULL CHECK (status IN ('draft','staged','ready','failed','expired')),
    relative_path TEXT,
    byte_count INTEGER NOT NULL DEFAULT 0 CHECK (byte_count >= 0),
    content_hash TEXT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY(session_id, turn_id) REFERENCES ai_turns(session_id, id) ON DELETE RESTRICT
);
CREATE TABLE IF NOT EXISTS ai_artifact_sources (
    artifact_id TEXT NOT NULL REFERENCES ai_artifacts(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    result_id TEXT REFERENCES ai_results(id) ON DELETE RESTRICT,
    tool_call_id TEXT REFERENCES ai_tool_calls(id) ON DELETE RESTRICT,
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    PRIMARY KEY(artifact_id, ordinal),
    CHECK (result_id IS NOT NULL OR tool_call_id IS NOT NULL)
);
CREATE TABLE IF NOT EXISTS ai_context_checkpoints (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation >= 1),
    first_message_seq INTEGER NOT NULL CHECK (first_message_seq >= 1),
    last_message_seq INTEGER NOT NULL CHECK (last_message_seq >= first_message_seq),
    summary_text TEXT NOT NULL,
    protected_items_json TEXT NOT NULL CHECK (json_valid(protected_items_json)),
    source_hash TEXT NOT NULL,
    model_profile_id TEXT REFERENCES ai_model_profiles(id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL,
    FOREIGN KEY(session_id, turn_id) REFERENCES ai_turns(session_id, id) ON DELETE RESTRICT,
    UNIQUE(turn_id, generation)
);
CREATE TABLE IF NOT EXISTS ai_usage_ledger (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    attempt INTEGER NOT NULL CHECK (attempt >= 1),
    model_profile_id TEXT NOT NULL REFERENCES ai_model_profiles(id) ON DELETE RESTRICT,
    budget_policy_id TEXT REFERENCES ai_budget_policies(id) ON DELETE RESTRICT,
    purpose TEXT NOT NULL CHECK (purpose IN ('main','decision','compaction','artifact')),
    input_tokens INTEGER CHECK (input_tokens >= 0),
    output_tokens INTEGER CHECK (output_tokens >= 0),
    cached_input_tokens INTEGER CHECK (cached_input_tokens >= 0),
    cost_micros INTEGER CHECK (cost_micros >= 0),
    measurement TEXT NOT NULL CHECK (measurement IN ('actual','estimated','unknown')),
    pricing_snapshot_json TEXT NOT NULL CHECK (json_valid(pricing_snapshot_json)),
    title TEXT,
    created_at_ms INTEGER NOT NULL,
    FOREIGN KEY(session_id, turn_id) REFERENCES ai_turns(session_id, id) ON DELETE RESTRICT,
    UNIQUE(request_id, attempt)
);
CREATE INDEX IF NOT EXISTS ai_usage_budget_time ON ai_usage_ledger(budget_policy_id, created_at_ms);
CREATE INDEX IF NOT EXISTS ai_usage_session ON ai_usage_ledger(session_id, created_at_ms);
CREATE TABLE IF NOT EXISTS ai_budget_reservations (
    id TEXT PRIMARY KEY,
    request_id TEXT NOT NULL,
    attempt INTEGER NOT NULL CHECK (attempt >= 1),
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    budget_policy_id TEXT REFERENCES ai_budget_policies(id) ON DELETE RESTRICT,
    amount_micros INTEGER NOT NULL CHECK (amount_micros >= 0),
    status TEXT NOT NULL CHECK (status IN ('held','settled','released','unknown')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY(session_id, turn_id) REFERENCES ai_turns(session_id, id) ON DELETE RESTRICT,
    UNIQUE(request_id, attempt)
);
CREATE INDEX IF NOT EXISTS ai_reservations_budget_status
    ON ai_budget_reservations(budget_policy_id, status, created_at_ms);"##;

/// 面板偏好独立声明，首次建表和约束升级复用同一结构。
pub const PANEL_PREFERENCES_SQL: &str = r##"CREATE TABLE IF NOT EXISTS ai_panel_preferences (
    id INTEGER PRIMARY KEY CHECK (id=1),
    preferred_mode TEXT NOT NULL DEFAULT 'drawer' CHECK (preferred_mode IN ('drawer','embedded','tab')),
    drawer_width_dp REAL NOT NULL DEFAULT 460.0 CHECK (drawer_width_dp > 0 AND drawer_width_dp <= 100000),
    embedded_width_dp REAL NOT NULL DEFAULT 420.0 CHECK (embedded_width_dp > 0 AND embedded_width_dp <= 100000),
    last_session_id TEXT REFERENCES ai_sessions(id) ON DELETE RESTRICT,
    pending_draft_json TEXT CHECK (pending_draft_json IS NULL OR json_valid(pending_draft_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    updated_at_ms INTEGER NOT NULL
);"##;

// MCP Server 管理页的应用层端到端测试（T060）。
//
// 走生产 `AppCommand` 链：Profile 落库 + 对象级 CAS、资源授权落库 + 形状校验、
// 活动记录加载、撤销/收窄范围影响运行期可见工具。不做客户端配置导出，也不做活动导出。

use std::path::PathBuf;

use fluxdb_app::{
    AiMcpServerGrantSaveInput, AiMcpServerProfileSaveInput, AppCommand, AppController, AppEvent,
};
use fluxdb_storage::{FileStorage, Storage};

struct Fixture {
    dir: PathBuf,
    controller: AppController,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fluxdb-mcp-server-settings-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("创建隔离目录");
        let storage = FileStorage::new(&dir);
        storage
            .with_ai_connection(|_| Ok(()))
            .expect("初始化 AI schema");
        let mut controller = AppController::new();
        controller.set_ai_storage(storage);
        Self { dir, controller }
    }

    fn storage(&self) -> FileStorage {
        FileStorage::new(&self.dir)
    }

    fn save_connections(&self, connections: &[fluxdb_core::ConnectionConfig]) {
        self.storage()
            .save_connections(connections)
            .expect("保存连接");
    }

    fn refresh(&mut self) {
        match self.controller.dispatch(AppCommand::RefreshAiSettings) {
            AppEvent::AiSettingsLoaded => {}
            other => panic!("刷新 AI 设置失败: {other:?}"),
        }
    }

    fn save_profile(
        &mut self,
        input: AiMcpServerProfileSaveInput,
    ) -> Result<(String, i64), String> {
        match self
            .controller
            .dispatch(AppCommand::SaveAiMcpServerProfile(input))
        {
            AppEvent::AiMcpServerProfileSaved { id, revision } => Ok((id, revision)),
            AppEvent::AiSettingsError(message) => Err(message),
            other => Err(format!("意外事件: {other:?}")),
        }
    }

    fn save_grant(&mut self, input: AiMcpServerGrantSaveInput) -> Result<(String, i64), String> {
        match self
            .controller
            .dispatch(AppCommand::SaveAiMcpServerGrant(input))
        {
            AppEvent::AiMcpServerGrantSaved { id, revision } => Ok((id, revision)),
            AppEvent::AiSettingsError(message) => Err(message),
            other => Err(format!("意外事件: {other:?}")),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn profile_input(name: &str) -> AiMcpServerProfileSaveInput {
    AiMcpServerProfileSaveInput {
        display_name: name.into(),
        enabled: true,
        write_mode: "disabled".into(),
        allowed_tool_ids: vec!["fluxdb_connections_list".into(), "fluxdb_query".into()],
        ..Default::default()
    }
}

fn sqlite_connection() -> fluxdb_core::ConnectionConfig {
    fluxdb_core::ConnectionConfig {
        id: fluxdb_core::ConnectionId(1),
        name: "T060 SQLite".into(),
        kind: fluxdb_core::DatabaseKind::Sqlite,
        endpoint: fluxdb_core::Endpoint::SqliteFile {
            path: std::path::PathBuf::from("/tmp/t060-mcp-server.sqlite"),
            read_only: false,
        },
        options: std::collections::BTreeMap::new(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    }
}

/// 新建 Profile 必须默认关闭且只读：保存配置本身不能对外开放服务。
#[test]
fn new_profile_is_disabled_and_read_only_until_explicitly_enabled() {
    let mut fixture = Fixture::new("new-profile-defaults");
    let (id, revision) = fixture
        .save_profile(profile_input("新建即安全"))
        .expect("保存 Profile");
    assert_eq!(revision, 1);

    let stored = fixture
        .storage()
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::mcp_server::load_server_profile(conn, &id)
        })
        .expect("读取 Profile")
        .expect("Profile 存在");
    assert!(!stored.enabled, "新建 Profile 必须默认禁用");
    assert_eq!(
        stored.write_mode,
        fluxdb_storage::ai_platform::mcp_server::WriteMode::Disabled
    );
    assert_eq!(
        stored.allowed_tool_ids,
        vec!["fluxdb_connections_list", "fluxdb_query"],
        "工具白名单按原样落库"
    );

    // 编辑后启用并切到 scoped：对象级 CAS 生效，revision 递增。
    let mut input = profile_input("新建即安全");
    input.id = Some(id.clone());
    input.expected_revision = Some(revision);
    input.enabled = true;
    input.write_mode = "scoped".into();
    let (_, next_revision) = fixture.save_profile(input).expect("启用 Profile");
    assert_eq!(next_revision, revision + 1);

    // 过期 revision 被拒，不静默覆盖其他窗口的修改。
    let mut stale = profile_input("新建即安全");
    stale.id = Some(id);
    stale.expected_revision = Some(revision);
    let error = fixture
        .save_profile(stale)
        .expect_err("过期 revision 必须冲突");
    assert!(error.contains("重新加载"), "实际错误: {error}");
}

/// `desktop_approval` 依赖尚未实现的桌面审批桥，必须明确 Unsupported，不能保存成可用能力。
#[test]
fn desktop_approval_write_mode_is_rejected_before_persisting() {
    let mut fixture = Fixture::new("desktop-approval");
    let mut input = profile_input("审批桥");
    input.write_mode = "desktop_approval".into();
    let error = fixture
        .save_profile(input)
        .expect_err("未实现审批桥不应保存成功");
    assert!(
        error.contains("桌面审批桥"),
        "错误应说明审批桥未实现，实际: {error}"
    );
    fixture.refresh();
    assert!(
        fixture.controller.state().ai_mcp_server_profiles.is_empty(),
        "被拒的 Profile 不得出现在状态里"
    );
}

/// 资源授权：形状校验、连接类型一致性与空规则不放行。
#[test]
fn resource_grants_enforce_object_scope_and_connection_kind() {
    let mut fixture = Fixture::new("grants");
    let (profile_id, _) = fixture
        .save_profile(profile_input("授权 Profile"))
        .expect("保存 Profile");
    fixture.save_connections(std::slice::from_ref(&sqlite_connection()));

    let base = AiMcpServerGrantSaveInput {
        profile_id: profile_id.clone(),
        connection_id: 1,
        database_name: "main".into(),
        resource_kind: "sql".into(),
        object_policy_json: r#"{"all_objects":true}"#.into(),
        ..Default::default()
    };
    let (grant_id, revision) = fixture.save_grant(base.clone()).expect("保存授权");
    assert_eq!(revision, 1);

    // 空对象规则不放行整库。
    let mut empty = base.clone();
    empty.object_policy_json = "{}".into();
    let error = fixture.save_grant(empty).expect_err("空规则必须拒绝");
    assert!(error.contains("all_objects"), "实际错误: {error}");

    // 授权类型与连接类型不一致（SQLite 连接却按 Redis 规则）必须拒绝。
    let mut mismatched = base.clone();
    mismatched.resource_kind = "redis".into();
    mismatched.database_name = "0".into();
    mismatched.object_policy_json = r#"{"all_keys":true}"#.into();
    let error = fixture
        .save_grant(mismatched)
        .expect_err("类型不一致必须拒绝");
    assert!(error.contains("不一致"), "实际错误: {error}");

    // 编辑收窄范围：CAS 生效，规则按原样落库。
    let mut narrow = base.clone();
    narrow.id = Some(grant_id.clone());
    narrow.expected_revision = Some(revision);
    narrow.object_policy_json = r#"{"table_allowlist":["orders"]}"#.into();
    narrow.namespace = "public".into();
    let (_, next_revision) = fixture.save_grant(narrow).expect("收窄范围");
    assert_eq!(next_revision, revision + 1);

    let stored = fixture
        .storage()
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::mcp_server::load_resource_grant(conn, &grant_id)
        })
        .expect("读取授权")
        .expect("授权存在");
    assert_eq!(
        stored.object_policy_json,
        r#"{"table_allowlist":["orders"]}"#
    );
    assert_eq!(stored.namespace, "public");
}

/// 管理页刷新后必须从存储加载 Profile / 授权 / 活动；不能被空集合覆盖。
#[test]
fn refresh_loads_profiles_grants_and_activity_from_storage() {
    let mut fixture = Fixture::new("refresh");
    let (profile_id, _) = fixture
        .save_profile(profile_input("活动 Profile"))
        .expect("保存 Profile");
    fixture.save_connections(std::slice::from_ref(&sqlite_connection()));
    fixture
        .save_grant(AiMcpServerGrantSaveInput {
            profile_id: profile_id.clone(),
            connection_id: 1,
            database_name: "main".into(),
            resource_kind: "sql".into(),
            object_policy_json: r#"{"all_objects":true}"#.into(),
            ..Default::default()
        })
        .expect("保存授权");

    // 造一条外部 MCP 活动记录（owner 是 MCP session，不建假 Turn）。
    fixture
        .storage()
        .with_ai_connection(|conn| {
            use fluxdb_storage::ai_platform::mcp_server_runtime::*;
            start_server_session(conn, 1, "s1", &profile_id, "2025-06-18", "{}")?;
            start_server_request(
                conn,
                2,
                "req-1",
                "s1",
                &profile_id,
                "\"1\"",
                "fluxdb_query",
                1,
                None,
                r#"{"target":"conn:1/db:main"}"#,
                r#"{"sql":"SELECT 1"}"#,
                "plan-1",
            )?;
            Ok(())
        })
        .expect("写入 MCP 活动");

    fixture.refresh();
    let state = fixture.controller.state();
    assert_eq!(state.ai_mcp_server_profiles.len(), 1);
    assert_eq!(state.ai_mcp_server_profiles[0].id, profile_id);
    assert_eq!(state.ai_mcp_server_grants.len(), 1);
    assert_eq!(
        state.ai_mcp_server_grants[0].connection_name, "T060 SQLite",
        "授权行带出连接名称，便于用户识别范围"
    );
    assert_eq!(state.ai_mcp_server_requests.len(), 1);
    assert_eq!(state.ai_mcp_server_requests[0].id, "req-1");
    assert_eq!(state.ai_mcp_server_requests[0].status, "prepared");
}

/// 收窄 Profile 工具白名单/停用后，`tools/list` 不再暴露写工具，且调用被拒。
#[test]
fn narrowing_profile_tool_allowlist_removes_tools_from_visible_set() {
    let mut fixture = Fixture::new("narrow-tools");
    let (profile_id, created_revision) = fixture
        .save_profile(AiMcpServerProfileSaveInput {
            allowed_tool_ids: vec!["fluxdb_connections_list".into(), "fluxdb_execute".into()],
            ..profile_input("收窄工具")
        })
        .expect("保存 Profile");
    // 新建一律落到 disabled/只读；显式编辑后才允许切到 scoped（设计 §28.6 默认最小权限）。
    let (profile_id, revision) = fixture
        .save_profile(AiMcpServerProfileSaveInput {
            id: Some(profile_id.clone()),
            display_name: "收窄工具".into(),
            enabled: true,
            write_mode: "scoped".into(),
            allowed_tool_ids: vec!["fluxdb_connections_list".into(), "fluxdb_execute".into()],
            expected_revision: Some(created_revision),
            ..Default::default()
        })
        .expect("切换到 scoped");
    let mut grant = AiMcpServerGrantSaveInput {
        profile_id: profile_id.clone(),
        connection_id: 1,
        database_name: "main".into(),
        resource_kind: "sql".into(),
        object_policy_json: r#"{"all_objects":true}"#.into(),
        allow_write: true,
        write_policy_json:
            r#"{"allowed_operations":["update"],"allowed_objects":["orders"],"require_where":true}"#
                .into(),
        ..Default::default()
    };
    fixture.save_connections(std::slice::from_ref(&sqlite_connection()));
    fixture.save_grant(grant.clone()).expect("保存授权");

    let profile = fixture
        .storage()
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::mcp_server::load_server_profile(conn, &profile_id)
        })
        .expect("读取")
        .expect("Profile");
    let grants = fixture
        .storage()
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::mcp_server::list_resource_grants(conn, &profile_id)
        })
        .expect("授权");
    let connections = vec![sqlite_connection()];
    let before = fluxdb_app::visible_tools(&profile, &grants).expect("tools/list");
    assert!(
        before.iter().any(|tool| tool.name == "fluxdb_execute"),
        "写授权 + 白名单包含时列出写工具"
    );

    // 管理页把写工具从白名单移除。
    let mut input = profile_input("收窄工具");
    input.id = Some(profile_id.clone());
    input.expected_revision = Some(revision);
    input.allowed_tool_ids = vec!["fluxdb_connections_list".into()];
    let (_, next_revision) = fixture.save_profile(input).expect("收窄白名单");
    assert_eq!(next_revision, revision + 1);

    let profile = fixture
        .storage()
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::mcp_server::load_server_profile(conn, &profile_id)
        })
        .expect("读取")
        .expect("Profile");
    let after = fluxdb_app::visible_tools(&profile, &grants).expect("tools/list");
    assert!(
        !after.iter().any(|tool| tool.name == "fluxdb_execute"),
        "白名单移除后写工具不再列出"
    );

    // 即使客户端凭缓存直接调用隐藏工具，执行链仍拒绝。
    let results =
        fluxdb_app::ResultStore::for_turn(fluxdb_app::RESULT_STORE_MEMORY_BYTES, "s", "s", None);
    let service = fluxdb_app::McpExecutionService::new(
        connections,
        profile,
        grants,
        std::sync::Arc::new(std::sync::Mutex::new(results)),
    )
    .expect("装配执行服务");
    let outcome = service.call(
        &fluxdb_app::McpCallIdentity {
            session_id: "s".into(),
            request_id: "r".into(),
        },
        "fluxdb_execute",
        r#"{"target":"conn:1/db:main","sql":"UPDATE orders SET status='x' WHERE id=1"}"#,
        &fluxdb_core::CancelFlag::new(),
    );
    assert!(!outcome.ok, "白名单外的写工具必须被拒");
    assert_eq!(outcome.error.expect("受控错误").code, "tool_not_allowed");

    // 撤销授权后资源不可解析（STALE_RESOURCE），不会静默换绑其它库。
    grant.enabled = false;
    grant.id = Some(
        fixture.controller.state().ai_mcp_server_grants[0]
            .id
            .clone(),
    );
    grant.expected_revision = Some(1);
    let _ = fixture.save_grant(grant);
    fixture.refresh();
    let grants_after = fixture
        .controller
        .state()
        .ai_mcp_server_grants
        .iter()
        .filter(|item| item.enabled)
        .cloned()
        .collect::<Vec<_>>();
    assert!(grants_after.is_empty(), "撤销后授权不再出现在启用集合中");
}

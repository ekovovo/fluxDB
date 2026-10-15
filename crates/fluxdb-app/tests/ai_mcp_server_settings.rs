// MCP Server 管理页的应用层端到端测试。
//
// 走生产 `AppCommand` 链：Profile 落库 + 对象级 CAS、数据源写入策略落库、
// 撤销数据源影响运行期可见工具。不做客户端配置导出。

use std::path::PathBuf;

use fluxdb_app::{
    AiMcpDatasourceSaveInput, AiMcpServerProfileSaveInput, AppCommand, AppController, AppEvent,
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

    fn save_datasource(&mut self, input: AiMcpDatasourceSaveInput) -> Result<(i64, i64), String> {
        match self
            .controller
            .dispatch(AppCommand::SaveAiMcpDatasource(input))
        {
            AppEvent::AiMcpDatasourceSaved {
                connection_id,
                revision,
            } => Ok((connection_id, revision)),
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
        ..Default::default()
    }
}

fn sqlite_connection() -> fluxdb_core::ConnectionConfig {
    fluxdb_core::ConnectionConfig {
        id: fluxdb_core::ConnectionId(1),
        name: "本地 SQLite".into(),
        kind: fluxdb_core::DatabaseKind::Sqlite,
        endpoint: fluxdb_core::Endpoint::SqliteFile {
            path: std::path::PathBuf::from("/tmp/mcp-server.sqlite"),
            read_only: false,
        },
        options: std::collections::BTreeMap::new(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    }
}

/// 新建 Profile 必须默认关闭：保存配置本身不能对外开放服务。
#[test]
fn new_profile_is_disabled_until_explicitly_enabled() {
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

    // 编辑后启用：对象级 CAS 生效，revision 递增。
    let mut input = profile_input("新建即安全");
    input.id = Some(id.clone());
    input.expected_revision = Some(revision);
    input.enabled = true;
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

/// 新增数据源默认策略为 `default`（跟随全局），可选只读 / 允许读写，CAS 生效。
#[test]
fn datasource_default_policy_and_cas() {
    let mut fixture = Fixture::new("datasource");
    let (profile_id, _) = fixture
        .save_profile(profile_input("数据源 Profile"))
        .expect("保存 Profile");
    fixture.save_connections(std::slice::from_ref(&sqlite_connection()));

    let (connection_id, revision) = fixture
        .save_datasource(AiMcpDatasourceSaveInput {
            profile_id: profile_id.clone(),
            connection_id: 1,
            ..Default::default()
        })
        .expect("新增数据源");
    assert_eq!(connection_id, 1);
    assert_eq!(revision, 1);
    let stored = fixture
        .storage()
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::mcp_server::load_datasource(conn, &profile_id, 1)
        })
        .expect("读取数据源")
        .expect("存在");
    assert_eq!(
        stored.write_policy,
        fluxdb_storage::ai_platform::mcp_server::DatasourceWritePolicy::Default
    );

    // 切到允许读写。
    let (_, next_revision) = fixture
        .save_datasource(AiMcpDatasourceSaveInput {
            profile_id: profile_id.clone(),
            connection_id: 1,
            write_policy: "read_write".into(),
            expected_revision: Some(revision),
        })
        .expect("切换策略");
    assert_eq!(next_revision, revision + 1);

    // 过期 revision 被拒。
    let error = fixture
        .save_datasource(AiMcpDatasourceSaveInput {
            profile_id: profile_id.clone(),
            connection_id: 1,
            write_policy: "read_only".into(),
            expected_revision: Some(revision),
        })
        .expect_err("过期 revision 必须冲突");
    assert!(
        error.contains("修改") || error.contains("存在"),
        "实际错误: {error}"
    );
}

/// 引用不存在的连接必须拒绝。
#[test]
fn datasource_requires_existing_connection() {
    let mut fixture = Fixture::new("datasource-missing");
    let (profile_id, _) = fixture
        .save_profile(profile_input("数据源 Profile"))
        .expect("保存 Profile");
    let error = fixture
        .save_datasource(AiMcpDatasourceSaveInput {
            profile_id,
            connection_id: 999,
            ..Default::default()
        })
        .expect_err("不存在的连接必须拒绝");
    assert!(error.contains("不存在"), "实际错误: {error}");
}

/// 管理页刷新后必须从存储加载 Profile / 数据源；不能被空集合覆盖。
#[test]
fn refresh_loads_profiles_and_datasources_from_storage() {
    let mut fixture = Fixture::new("refresh");
    let (profile_id, _) = fixture
        .save_profile(profile_input("活动 Profile"))
        .expect("保存 Profile");
    fixture.save_connections(std::slice::from_ref(&sqlite_connection()));
    fixture
        .save_datasource(AiMcpDatasourceSaveInput {
            profile_id: profile_id.clone(),
            connection_id: 1,
            ..Default::default()
        })
        .expect("新增数据源");

    fixture.refresh();
    let state = fixture.controller.state();
    assert_eq!(state.ai_mcp_server_profiles.len(), 1);
    assert_eq!(state.ai_mcp_server_profiles[0].id, profile_id);
    assert_eq!(state.ai_mcp_datasources.len(), 1);
    assert_eq!(
        state.ai_mcp_datasources[0].connection_name, "本地 SQLite",
        "数据源行带出连接名称"
    );
    assert_eq!(state.ai_mcp_datasources[0].write_policy, "default");
}

/// 只读（或 default + 全局只读）数据源下，写工具不列出；允许读写后列出并可执行。
#[test]
fn datasource_write_policy_controls_visible_write_tools() {
    let mut fixture = Fixture::new("policy");
    let (profile_id, _) = fixture
        .save_profile(profile_input("策略 Profile"))
        .expect("保存 Profile");
    fixture.save_connections(std::slice::from_ref(&sqlite_connection()));
    fixture
        .save_datasource(AiMcpDatasourceSaveInput {
            profile_id: profile_id.clone(),
            connection_id: 1,
            write_policy: "read_only".into(),
            ..Default::default()
        })
        .expect("新增数据源");

    let profile = fixture
        .storage()
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::mcp_server::load_server_profile(conn, &profile_id)
        })
        .expect("读取")
        .expect("Profile");
    let datasources = fixture
        .storage()
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::mcp_server::list_datasources(conn, &profile_id)
        })
        .expect("数据源");
    let connections = vec![sqlite_connection()];
    let service = fluxdb_app::McpExecutionService::new(
        connections.clone(),
        profile.clone(),
        datasources.clone(),
        fluxdb_core::McpWritePolicy::ReadOnly,
        std::collections::BTreeSet::new(),
        std::sync::Arc::new(std::sync::Mutex::new(fluxdb_app::ResultStore::for_turn(
            fluxdb_app::RESULT_STORE_MEMORY_BYTES,
            "s",
            "s",
            None,
        ))),
    )
    .expect("装配执行服务");
    let names = service
        .tool_views()
        .expect("tools/list")
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert!(
        !names.contains(&"fluxdb_execute".to_string()),
        "只读数据源不列出写工具"
    );
    let outcome = service.call(
        &fluxdb_app::McpCallIdentity {
            session_id: "s".into(),
            request_id: "r".into(),
        },
        "fluxdb_execute",
        r#"{"target":"conn:1/db:main","sql":"UPDATE orders SET status='x' WHERE id=1"}"#,
        &fluxdb_core::CancelFlag::new(),
    );
    assert!(!outcome.ok, "只读数据源上的写必须被拒");

    // 切到允许读写（连接非只读）后，写工具列出。
    let (_, revision) = fixture
        .save_datasource(AiMcpDatasourceSaveInput {
            profile_id: profile_id.clone(),
            connection_id: 1,
            write_policy: "read_write".into(),
            expected_revision: Some(1),
        })
        .expect("切换策略");
    assert_eq!(revision, 2);
    let datasources = fixture
        .storage()
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::mcp_server::list_datasources(conn, &profile_id)
        })
        .expect("数据源");
    let service = fluxdb_app::McpExecutionService::new(
        connections,
        profile,
        datasources,
        fluxdb_core::McpWritePolicy::ReadOnly,
        std::collections::BTreeSet::new(),
        std::sync::Arc::new(std::sync::Mutex::new(fluxdb_app::ResultStore::for_turn(
            fluxdb_app::RESULT_STORE_MEMORY_BYTES,
            "s",
            "s",
            None,
        ))),
    )
    .expect("装配执行服务");
    let names = service
        .tool_views()
        .expect("tools/list")
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert!(
        names.contains(&"fluxdb_execute".to_string()),
        "允许读写后列出写工具"
    );
}

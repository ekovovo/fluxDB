// Oracle 用户与权限面板的端到端用例：真实生产视图 + 无头事件驱动。
//
// 断言围绕 docs/oracle.md §8 的硬约束：
// - 账号即 schema，别人的账号只列名字（`grants_unreadable`），不画成空授权；
// - 继承来的授权不给撤销入口；
// - 口令在预览里按草稿原样显示（与 MySQL / PostgreSQL 一致）；
// - 点保存先弹确认框展示本次 SQL，确认后才执行（五种库一致）。

#[gpui::test]
fn oracle_security_panel_renders_directory_and_preview(cx: &mut TestAppContext) {
    use fluxdb_app::{AppCommand, AppEvent, OracleUserAdminTab, TabKind, UserAdminDetailTab};
    use fluxdb_core::{
        ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, OracleBasicOptions,
        OracleConnectionProfile, OracleObjectPrivilege, OracleRoleGrant, OracleSystemPrivilege,
        OracleTablespaceQuota, OracleUser, OracleUserAdminSnapshot, OracleUserDetail, SecretRef,
    };

    let _guard = locale_guard();
    let temp = tempfile::tempdir().expect("isolated profile");
    let storage = FileStorage::new(temp.path().join("profile"));
    init_i18n(
        &app_assets_base_path().join("locales"),
        &storage.root_dir().join("locales"),
        "en",
    );
    let controller = AppController::new();
    let connection_id = ConnectionId(61);
    let connection_config = ConnectionConfig {
        id: connection_id,
        name: "Oracle security".into(),
        kind: DatabaseKind::Oracle,
        endpoint: Endpoint::Tcp {
            host: "127.0.0.1".into(),
            port: 1521,
            database: Some("FREEPDB1".into()),
        },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
        dameng_profile: None,
        sqlserver_profile: None,
        oracle_profile: Some(OracleConnectionProfile {
            basic: OracleBasicOptions {
                host: "127.0.0.1".into(),
                port: 1521,
                service_name: "FREEPDB1".into(),
                username: "fluxdb".into(),
                password: SecretRef::inline("not-used"),
                ..OracleBasicOptions::default()
            },
            ..OracleConnectionProfile::default()
        }),
    };
    // 当前账号 = FLUXDB：只有自己读得到授权明细。
    let snapshot = OracleUserAdminSnapshot {
        current_user: "FLUXDB".into(),
        users: vec![
            OracleUser {
                name: "FLUXDB".into(),
                ..Default::default()
            },
            OracleUser {
                name: "APPUSER".into(),
                ..Default::default()
            },
            OracleUser {
                name: "SYS".into(),
                oracle_maintained: true,
                ..Default::default()
            },
        ],
        own: Some(OracleUserDetail {
            name: "FLUXDB".into(),
            account_status: "OPEN".into(),
            default_tablespace: Some("USERS".into()),
            temporary_tablespace: Some("TEMP".into()),
            ..Default::default()
        }),
        roles: vec![OracleRoleGrant {
            role: "CONNECT".into(),
            default_role: true,
            ..Default::default()
        }],
        system_privileges: vec![OracleSystemPrivilege {
            privilege: "CREATE SESSION".into(),
            ..Default::default()
        }],
        object_privileges: vec![OracleObjectPrivilege {
            owner: "APP".into(),
            object: "ORDERS".into(),
            privilege: "SELECT".into(),
            ..Default::default()
        }],
        quotas: vec![OracleTablespaceQuota {
            tablespace: "USERS".into(),
            max_bytes: None,
        }],
        limits: Vec::new(),
        role_candidates: vec!["CONNECT".into()],
        system_privilege_candidates: vec!["CREATE SESSION".into()],
    };

    cx.update(|cx| {
        extend_component_i18n();
        gpui_component::init(cx);
    });
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            navicat_main_new(
                window,
                cx,
                controller,
                storage,
                gpui_component::ThemeMode::Light,
            )
        });
        Root::new(view, window, cx)
    });
    cx.simulate_resize(gpui::size(gpui::px(1440.), gpui::px(1000.)));
    let view = cx.update(|window, cx| {
        window
            .root::<Root>()
            .expect("root")
            .expect("typed root")
            .read(cx)
            .view()
            .clone()
            .downcast::<NavicatMain>()
            .expect("production view")
    });
    cx.run_until_parked();
    // 打开标签页后立刻回填快照，避免真实拨号（本用例只验证界面契约）。
    let tab_id = cx.update(|_, cx| {
        view.update(cx, |this, cx| {
            this.controller
                .dispatch(AppCommand::ReplaceConnections(vec![connection_config]));
            let tab_id = match this
                .controller
                .dispatch(AppCommand::OpenUserAdmin(connection_id))
            {
                AppEvent::TabOpened(tab_id) | AppEvent::TabActivated(tab_id) => tab_id,
                event => panic!("unexpected open event: {event:?}"),
            };
            this.controller
                .dispatch(AppCommand::FinishOracleUsersLoad {
                    tab_id,
                    result: Ok(snapshot),
                });
            cx.notify();
            tab_id
        })
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });

    cx.update(|_, cx| {
        let state = view.read(cx).controller.state();
        assert_eq!(state.active_tab, Some(tab_id));
        assert!(state.tabs.iter().any(|tab| {
            tab.id == tab_id
                && matches!(&tab.kind, TabKind::UserAdmin(admin) if admin.oracle.is_some())
        }));
    });

    // 面板骨架与工具栏必须真实渲染。
    assert!(
        cx.debug_bounds("workspace-page").is_some(),
        "workspace content must be laid out: {:?}",
        cx.update(|_, cx| {
            let state = view.read(cx).controller.state();
            (
                state.active_tab,
                state
                    .tabs
                    .iter()
                    .find(|tab| tab.id == tab_id)
                    .map(|tab| format!("{:?}", std::mem::discriminant(&tab.kind))),
                state.connections.len(),
            )
        })
    );
    assert!(
        cx.update(|_, cx| {
            let state = view.read(cx).controller.state();
            let admin = state.tabs.iter().find(|tab| tab.id == tab_id).and_then(|tab| {
                match &tab.kind {
                    TabKind::UserAdmin(admin) => Some(admin),
                    _ => None,
                }
            });
            state
                .connections
                .iter()
                .any(|c| c.config.id == connection_id)
                && admin.is_some_and(|admin| admin.oracle.is_some())
        }),
        "connection and Oracle state must both exist"
    );
    let workspace = cx
        .debug_bounds("workspace-page")
        .expect("workspace content must be laid out");
    assert!(
        f32::from(workspace.size.height) > 1.,
        "workspace content must have height: {workspace:?}"
    );
    assert!(
        cx.debug_bounds("home-new-connection").is_none(),
        "an active Oracle user-admin tab must not render the home page"
    );
    assert!(
        cx.debug_bounds("user-admin-save").is_none(),
        "Oracle must not fall through to the MySQL user-admin toolbar"
    );
    assert!(
        cx.debug_bounds("oracle-user-FLUXDB").is_some(),
        "the account list must render"
    );
    for selector in [
        "oracle-security-save",
        "oracle-security-refresh",
        "oracle-security-delete",
        "oracle-security-tab-general",
        "oracle-security-tab-roles",
        "oracle-security-tab-system-privileges",
        "oracle-security-tab-object-privileges",
        "oracle-security-tab-sql-preview",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "Oracle security control must render: {selector}"
        );
    }
    // 账号目录：三个账号都在，且默认选中当前账号。
    for (selector, name) in [
        ("oracle-user-FLUXDB", "FLUXDB"),
        ("oracle-user-APPUSER", "APPUSER"),
        ("oracle-user-SYS", "SYS"),
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "directory must list {name}"
        );
    }

    // 角色页：直接授予的 CONNECT 有撤销入口；切到别的账号则整页不可读。
    let roles_tab = cx
        .debug_bounds("oracle-security-tab-roles")
        .expect("roles tab");
    cx.simulate_click(roles_tab.center(), Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.debug_bounds("oracle-security-role-0").is_some(),
        "direct role grant must be listed"
    );
    assert!(
        cx.debug_bounds("oracle-security-role-0-revoke").is_some(),
        "a directly granted role must be revocable"
    );

    // 授权名候选：只有带 ADMIN OPTION 的直接授权才列出，选中即写入草稿。
    let candidates = cx
        .debug_bounds("oracle-security-grant-name-candidates")
        .expect("role tab must offer name candidates");
    cx.simulate_click(candidates.center(), Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let candidate = cx
        .debug_bounds("oracle-security-grant-name-candidate-0")
        .expect("candidate list must render");
    cx.simulate_click(candidate.center(), Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|_, cx| {
        let state = view
            .read(cx)
            .user_admin_state_for(tab_id)
            .and_then(|admin| admin.oracle)
            .expect("Oracle state");
        assert_eq!(
            state.grant.name, "CONNECT",
            "choosing a candidate must fill the editable grant name"
        );
    });

    let other = cx
        .debug_bounds("oracle-user-APPUSER")
        .expect("other account row");
    cx.simulate_click(other.center(), Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.debug_bounds("oracle-security-role-0").is_none(),
        "another account's grants are not readable and must not render as rows"
    );
    cx.update(|_, cx| {
        let admin = view
            .read(cx)
            .user_admin_state_for(tab_id)
            .expect("admin state");
        let state = admin.oracle.as_ref().expect("Oracle state");
        assert_eq!(state.selected.as_deref(), Some("APPUSER"));
        // 非当前账号：可撤销列表必须为空，不允许把不可读画成可撤销。
        assert!(state.revocable_roles().is_empty());
        assert!(state.revocable_system_privileges().is_empty());
        assert!(state.revocable_object_privileges().is_empty());
        assert_eq!(
            admin.active_detail_tab,
            UserAdminDetailTab::Oracle(OracleUserAdminTab::Roles),
            "switching account keeps the active tab"
        );
    });

    // 回到自己，填一条改密后直接点保存：先弹确认框，确认后才执行，没有「加入变更」。
    let me = cx.debug_bounds("oracle-user-FLUXDB").expect("self row");
    cx.simulate_click(me.center(), Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let general_tab = cx
        .debug_bounds("oracle-security-tab-general")
        .expect("general tab");
    cx.simulate_click(general_tab.center(), Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    for (selector, text) in [
        ("oracle-security-new-password", "FluxDB_New!123"),
        ("oracle-security-new-password-confirm", "FluxDB_New!123"),
    ] {
        let bounds = cx
            .debug_bounds(selector)
            .expect("change-password input must render for the current account");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.simulate_input(text);
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
    // 口令只进内存草稿，不进待执行列表。
    cx.update(|_, cx| {
        let state = view
            .read(cx)
            .user_admin_state_for(tab_id)
            .and_then(|admin| admin.oracle)
            .expect("Oracle state");
        assert_eq!(state.new_password, "FluxDB_New!123");
        assert!(state.pending_changes.is_empty(), "nothing queued separately");
        assert!(state.creating.is_none());
    });
    // 「SQL 预览」页与保存执行的是同一份本地渲染，口令按草稿原样显示。
    let preview_tab = cx
        .debug_bounds("oracle-security-tab-sql-preview")
        .expect("SQL preview tab");
    cx.simulate_click(preview_tab.center(), Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let preview_code = cx
        .debug_bounds("oracle-security-preview-code")
        .expect("preview must use the shared highlighted SQL view");
    // 只有「元素存在」不够：滚动容器里 `flex_1` 拿不到高度时框会坍缩成 0 高，
    // 用户看到的是一条空白细线。这里必须断言真实可见高度。
    assert!(
        f32::from(preview_code.size.height) > 100.,
        "preview code view must have real height, got {preview_code:?}"
    );
    let save = cx
        .debug_bounds("oracle-security-save")
        .expect("save button");
    cx.simulate_click(save.center(), Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    // 五种库统一：点保存先弹确认框展示本次 SQL，确认后才执行。
    assert!(
        cx.debug_bounds("user-admin-sql-preview-modal").is_some(),
        "Save must open the SQL confirmation modal"
    );
    let (preview_sql, applying) = cx.update(|_, cx| {
        let admin = view
            .read(cx)
            .user_admin_state_for(tab_id)
            .expect("admin state");
        (
            admin.pending_sql.as_ref().map(|pending| pending.sql.clone()),
            admin.oracle.as_ref().is_some_and(|state| state.applying),
        )
    });
    let preview_sql = preview_sql.expect("confirmation modal must carry the SQL");
    assert!(
        preview_sql.contains("ALTER USER"),
        "confirmation modal must show the pending statements, got: {preview_sql}"
    );
    // 只弹框、不落库：确认前不能进入执行态。
    assert!(!applying, "Save must not start execution before confirmation");

    // 点「执行」才真正提交。
    let execute = cx
        .debug_bounds("user-admin-sql-preview-execute")
        .expect("modal execute button");
    cx.simulate_click(execute.center(), Modifiers::default());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let (applying, failed) = cx.update(|_, cx| {
            let state = view
                .read(cx)
                .user_admin_state_for(tab_id)
                .and_then(|admin| admin.oracle)
                .expect("Oracle state");
            (state.applying, state.apply_error.is_some())
        });
        if applying || failed {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Confirmed execution did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

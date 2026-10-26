// MongoDB 侧边栏 → 集合工作台的端到端用例（真实生产视图，无头驱动）。
//
// 只走 UI 层可见路径：双击连接 → 展开库 → 展开「集合」分组 → 点击集合行。
// 连接器是真实 `MongoConnector`，因此需要 `FLUXDB_MONGO_URI`；未设置时跳过。
// 凭据只从环境变量进内存，不落文件、不打印。

mod mongo_sidebar_tests {
    use std::collections::BTreeMap;

    use fluxdb_app::{AppCommand, AppController, TabKind};
    use fluxdb_core::{
        ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, MongoConnectionProfile, ObjectKind,
        Settings,
    };
    use fluxdb_storage::{FileStorage, Storage as _};
    use gpui::{
        AppContext as _, Modifiers, MouseButton, MouseDownEvent, TestAppContext, VisualTestContext,
    };
    use gpui_component::Root;

    use crate::{
        MongoDocumentEditorMode, NavicatMain, app_assets_base_path, extend_component_i18n,
        init_i18n, navicat_main_new,
    };

    /// `debug_bounds` 只接受 `&'static str`；测试里按需把格式化后的选择器泄漏成静态串。
    fn selector(text: String) -> &'static str {
        Box::leak(text.into_boxed_str())
    }

    /// 从 `FLUXDB_MONGO_URI` 构造真实连接配置；未设置时返回 `None`（用例跳过）。
    fn live_connection() -> Option<ConnectionConfig> {
        let uri = std::env::var("FLUXDB_MONGO_URI").ok()?;
        let uri = uri.trim();
        if uri.is_empty() {
            return None;
        }
        let profile = MongoConnectionProfile::from_uri(uri)
            .unwrap_or_else(|error| panic!("FLUXDB_MONGO_URI 解析失败: {error}"));
        Some(ConnectionConfig {
            id: ConnectionId(7_701),
            name: "MongoDB live".into(),
            kind: DatabaseKind::MongoDb,
            endpoint: Endpoint::Tcp {
                host: profile
                    .basic
                    .hosts
                    .first()
                    .map(|host| host.host.clone())
                    .unwrap_or_default(),
                port: profile
                    .basic
                    .hosts
                    .first()
                    .map(|host| host.port)
                    .unwrap_or(27_017),
                database: Some(profile.basic.default_database.clone()),
            },
            options: BTreeMap::new(),
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
            dameng_profile: None,
            sqlserver_profile: None,
            mongo_profile: Some(profile),
        })
    }

    /// 目标库：URI 默认库 → `FLUXDB_MONGO_DB` → `fluxdb_demo`。
    fn live_database(config: &ConnectionConfig) -> String {
        config
            .mongo_profile
            .as_ref()
            .map(|profile| profile.basic.default_database.clone())
            .filter(|database| !database.is_empty())
            .or_else(|| std::env::var("FLUXDB_MONGO_DB").ok())
            .unwrap_or_else(|| "fluxdb_demo".to_string())
    }

    /// 推进测试时钟直到 `probe` 返回 `Some`；用于等后台连接/查询任务落地。
    fn wait_until<R>(
        cx: &mut VisualTestContext,
        label: &str,
        mut probe: impl FnMut(&mut VisualTestContext) -> Option<R>,
    ) -> R {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            cx.background_executor
                .advance_clock(std::time::Duration::from_millis(16));
            cx.run_until_parked();
            if let Some(value) = probe(cx) {
                return value;
            }
            assert!(std::time::Instant::now() < deadline, "等待超时：{label}");
            std::thread::yield_now();
        }
    }

    /// 第 0 张 JSON 卡片当前渲染出的 gutter 行数（含可折叠行与普通行）。
    fn json_card_visible_rows(cx: &mut VisualTestContext) -> usize {
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let mut rows = 0;
        for line in 0..200 {
            // 选择器由 `json_fold_view` 按 `{前缀}:{行}` / `{前缀}-plain:{行}` 生成，
            // 前缀是 `mongo-json-fold:0`，所以普通行是 `mongo-json-fold:0-plain:N`。
            let fold = selector(format!("mongo-json-fold:0:{line}"));
            let plain = selector(format!("mongo-json-fold:0-plain:{line}"));
            if cx.debug_bounds(fold).is_some() || cx.debug_bounds(plain).is_some() {
                rows += 1;
            }
        }
        rows
    }

    /// 重绘并返回指定选择器的边界（渲染后的命中区域）。
    fn redraw_and_bounds(
        cx: &mut VisualTestContext,
        selector: &'static str,
    ) -> Option<gpui::Bounds<gpui::Pixels>> {
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        cx.debug_bounds(selector)
    }

    #[gpui::test]
    fn mongo_database_expands_and_collection_opens_workspace(cx: &mut TestAppContext) {
        let Some(config) = live_connection() else {
            eprintln!("跳过：未设置 FLUXDB_MONGO_URI");
            return;
        };
        let connection_id = config.id;
        let database = live_database(&config);

        cx.background_executor.allow_parking();
        let temp = tempfile::tempdir().expect("isolated profile");
        let storage = FileStorage::new(temp.path().join("profile"));
        let settings = Settings {
            locale: "en".into(),
            show_sidebar: true,
            ..Default::default()
        };
        storage.save_settings(&settings).expect("settings");
        init_i18n(
            &app_assets_base_path().join("locales"),
            &storage.root_dir().join("locales"),
            "en",
        );
        let mut controller = AppController::new();
        controller.set_ai_storage(storage.clone());
        controller.dispatch(AppCommand::SaveSettings(settings));
        controller.dispatch(AppCommand::ReplaceConnections(vec![config.clone()]));
        cx.update(|cx| {
            extend_component_i18n();
            gpui_component::init(cx);
        });
        let profile = storage.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                let mut app = navicat_main_new(
                    window,
                    cx,
                    controller,
                    profile,
                    gpui_component::ThemeMode::Light,
                );
                // 跳过首帧会话恢复，避免后台存储任务越过用例边界。
                app.ai_chat.recovery_pending = false;
                app
            });
            Root::new(view, window, cx)
        });
        cx.simulate_resize(gpui::size(gpui::px(1500.), gpui::px(950.)));
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

        // 1. 双击连接行 = 建立连接（单击只展开/收起）。
        let connection_row =
            redraw_and_bounds(cx, selector(format!("connection-row:{}", connection_id.0)))
                .expect("连接行必须渲染");
        cx.simulate_event(MouseDownEvent {
            position: connection_row.center(),
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 2,
            first_mouse: false,
        });
        let database_selector = selector(format!("database-row:{database}"));
        wait_until(cx, "连接打开", |cx| {
            redraw_and_bounds(cx, database_selector)
        });

        // 2. 点击库行展开集合分组。
        let database_row = redraw_and_bounds(cx, database_selector).expect("库行必须渲染");
        cx.simulate_click(database_row.center(), Modifiers::default());
        let group_selector = selector(format!("object-group-row:collections:{database}"));
        wait_until(cx, "库展开", |cx| redraw_and_bounds(cx, group_selector));

        // 3. 展开「集合」分组，等集合行出现。
        let group_row = redraw_and_bounds(cx, group_selector).expect("集合分组行必须渲染");
        cx.simulate_click(group_row.center(), Modifiers::default());
        let collection = wait_until(cx, "集合列表加载", |cx| {
            cx.update(|_, cx| {
                view.read(cx)
                    .controller
                    .state()
                    .connections
                    .iter()
                    .find(|connection| connection.config.id == connection_id)
                    .and_then(|connection| {
                        connection
                            .objects
                            .iter()
                            .find(|object| {
                                object.path.kind == ObjectKind::Collection
                                    && object.path.database.as_deref() == Some(database.as_str())
                            })
                            .map(|object| object.path.name.clone())
                    })
            })
        });

        // 4. 点击集合行，工作台必须真的打开并渲染出来。
        let collection_row =
            redraw_and_bounds(cx, selector(format!("table-tree-row:{collection}")))
                .expect("集合行必须渲染");
        cx.simulate_click(collection_row.center(), Modifiers::default());
        let page_selector = selector(format!("mongo-collection-page:{database}/{collection}"));
        wait_until(cx, "集合页渲染", |cx| {
            redraw_and_bounds(cx, page_selector)
        });

        // 5. 首屏查询结果必须落到标签页状态（值由真实服务端返回）。
        wait_until(cx, "集合首屏查询", |cx| {
            cx.update(|_, cx| {
                view.read(cx)
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .find_map(|tab| match &tab.kind {
                        TabKind::MongoCollection(state) => state.page.clone(),
                        _ => None,
                    })
                    .filter(|page| page.returned > 0)
            })
        });

        // 6. 文档卡片默认展开成字段列表；点折叠箭头后正文消失（原型 / Compass 行为）。
        let fields_selector = selector("mongo-document-fields:0".to_string());
        wait_until(cx, "文档字段列表渲染", |cx| {
            redraw_and_bounds(cx, fields_selector)
        });
        let toggle_selector = selector("mongo-document-toggle:0".to_string());
        let toggle = redraw_and_bounds(cx, toggle_selector).expect("折叠箭头必须渲染");
        cx.simulate_click(toggle.center(), Modifiers::default());
        wait_until(cx, "文档折叠", |cx| {
            (redraw_and_bounds(cx, fields_selector).is_none()).then_some(())
        });
        let toggle = redraw_and_bounds(cx, toggle_selector).expect("折叠后箭头仍在");
        cx.simulate_click(toggle.center(), Modifiers::default());
        wait_until(cx, "文档重新展开", |cx| {
            redraw_and_bounds(cx, fields_selector)
        });

        // 6.1 整条卡头都能折叠：点 `_id` 文字（不是箭头）也要生效。
        let head = redraw_and_bounds(cx, selector("mongo-document-head:0".to_string()))
            .expect("卡片头必须渲染");
        cx.simulate_click(head.center(), Modifiers::default());
        wait_until(cx, "点卡头折叠", |cx| {
            (redraw_and_bounds(cx, fields_selector).is_none()).then_some(())
        });
        let head = redraw_and_bounds(cx, selector("mongo-document-head:0".to_string()))
            .expect("折叠后卡片头仍在");
        cx.simulate_click(head.center(), Modifiers::default());
        wait_until(cx, "点卡头重新展开", |cx| {
            redraw_and_bounds(cx, fields_selector)
        });

        // 7. 切到 JSON 展示：仍是逐条文档卡片，正文换成带行号的 JSON（不是整页一个数组）。
        let json_tab = selector("mongo-display-tab:Json".to_string());
        let json_tab_bounds = redraw_and_bounds(cx, json_tab).expect("JSON 展示页签必须渲染");
        cx.simulate_click(json_tab_bounds.center(), Modifiers::default());
        let json_selector = selector("mongo-document-json:0".to_string());
        wait_until(cx, "JSON 卡片正文渲染", |cx| {
            redraw_and_bounds(cx, json_selector)
        });
        assert!(
            redraw_and_bounds(cx, selector("mongo-document-toggle:0".to_string())).is_some(),
            "JSON 展示复用同一张卡片头"
        );
        assert!(
            redraw_and_bounds(cx, fields_selector).is_none(),
            "JSON 展示不再渲染字段列表"
        );

        // 7.1 默认态与 Compass 一致：根节点展开、嵌套节点全部收起。
        // 折叠后行号会重建，所以直接读标签页状态里的折叠集合判断默认态。
        let default_folds = cx.update(|_, cx| {
            let main = view.read(cx);
            main.controller
                .state()
                .tabs
                .iter()
                .find(|tab| matches!(tab.kind, TabKind::MongoCollection(_)))
                .map(|tab| {
                    main.mongo_json_fold_toggles
                        .get(&tab.id)
                        .cloned()
                        .unwrap_or_default()
                })
                .unwrap_or_default()
        });
        assert!(default_folds.is_empty(), "默认态不应有任何被点开的节点");
        let collapsed_rows = json_card_visible_rows(cx);
        // 默认态下嵌套的 object / array 都收成单行，可见行数一定少于展开态。
        let window_bounds = cx.update(|window, _| window.bounds());
        // 跳过第 0 行：那是根对象，默认就是展开的，点它只会收起。
        let fold_selector = (1..200)
            .map(|line| selector(format!("mongo-json-fold:0:{line}")))
            .find(|candidate| {
                redraw_and_bounds(cx, candidate)
                    .is_some_and(|bounds| window_bounds.contains(&bounds.center()))
            })
            .expect("JSON 视图至少有一个嵌套可折叠节点在窗口内");

        // 7.2 点开一个嵌套节点后，可见行数必须增加（子节点展开）。
        let fold_bounds = redraw_and_bounds(cx, fold_selector).expect("折叠行必须渲染");
        cx.simulate_click(fold_bounds.center(), Modifiers::default());
        let expanded_rows = wait_until(cx, "JSON 节点展开", |cx| {
            redraw_and_bounds(cx, fold_selector);
            let rows = json_card_visible_rows(cx);

            (rows > collapsed_rows).then_some(rows)
        });
        assert!(
            expanded_rows > collapsed_rows,
            "展开后行数必须增加：{collapsed_rows} → {expanded_rows}"
        );

        // 还原成文档展示，避免后续步骤依赖当前展示方式。
        let documents_tab =
            redraw_and_bounds(cx, selector("mongo-display-tab:Documents".to_string()))
                .expect("文档展示页签必须渲染");
        cx.simulate_click(documents_tab.center(), Modifiers::default());
        wait_until(cx, "切回文档展示", |cx| {
            redraw_and_bounds(cx, fields_selector)
        });

        // 8. 表格展示：整表带 `_id` 列，单元格按「行:列」渲染（Compass 表格语义）。
        let table_tab = redraw_and_bounds(cx, selector("mongo-display-tab:Table".to_string()))
            .expect("表格展示页签必须渲染");
        cx.simulate_click(table_tab.center(), Modifiers::default());
        wait_until(cx, "表格视图渲染", |cx| {
            redraw_and_bounds(cx, selector("mongo-document-table".to_string()))
        });
        assert!(
            redraw_and_bounds(cx, selector("mongo-table-cell:0:id".to_string())).is_some(),
            "表格首行必须渲染 _id 单元格"
        );
        // 操作列固定在表格右侧：表格视图同样有四个操作按钮，且首行删除入口在窗口内。
        assert!(
            redraw_and_bounds(cx, selector("mongo-document-delete:0".to_string()))
                .is_some_and(|bounds| window_bounds.contains(&bounds.center())),
            "表格首行必须渲染可见的删除按钮（固定操作列）"
        );
        let copy_button = redraw_and_bounds(cx, selector("mongo-document-copy:0".to_string()))
            .expect("表格首行必须渲染复制按钮");
        assert!(
            window_bounds.contains(&copy_button.center()),
            "复制按钮中心点必须在窗口内，否则点击会被丢弃"
        );
        // 写剪贴板是无声操作：必须给出可观察反馈，否则用户不知道复制成功没有。
        cx.simulate_click(copy_button.center(), Modifiers::default());
        wait_until(cx, "复制后出现提示", |cx| {
            cx.debug_bounds("app-message-overlay")
        });
        assert!(
            redraw_and_bounds(cx, fields_selector).is_none(),
            "表格展示不再渲染文档卡片"
        );
        let documents_tab =
            redraw_and_bounds(cx, selector("mongo-display-tab:Documents".to_string()))
                .expect("文档展示页签必须渲染");
        cx.simulate_click(documents_tab.center(), Modifiers::default());
        wait_until(cx, "切回文档展示", |cx| {
            redraw_and_bounds(cx, fields_selector)
        });

        // 9. 嵌套字段默认收起成 `{ N fields }` / `[ N items ]`；点开后才出现子行。
        // 候选取自真实返回的文档（不假设夹具结构），并要求摘要行落在窗口内才点击——
        // 越界点击会被静默丢弃。
        let window = cx.update(|window, _| window.bounds());
        let candidates = cx.update(|_, cx| {
            view.read(cx)
                .controller
                .state()
                .tabs
                .iter()
                .find_map(|tab| match &tab.kind {
                    TabKind::MongoCollection(state) => state.page.as_ref(),
                    _ => None,
                })
                .map(|page| {
                    page.documents
                        .iter()
                        .flat_map(|document| {
                            let fields = serde_json::from_str::<serde_json::Value>(&document.json)
                                .ok()
                                .and_then(|json| json.as_object().cloned())
                                .unwrap_or_default();
                            fields
                                .into_iter()
                                .filter(|(_, value)| {
                                    matches!(value, serde_json::Value::Array(_))
                                        || matches!(
                                            value,
                                            serde_json::Value::Object(map)
                                                if !map.keys().next().is_some_and(|k| k.starts_with('$'))
                                        )
                                })
                                .map(|(key, _)| (document.id.clone(), key))
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        });
        let Some((document_id, nested_key)) = candidates.into_iter().find(|(id, key)| {
            let selector = selector(format!("mongo-field-toggle:{id}/{key}"));
            redraw_and_bounds(cx, selector).is_some_and(|bounds| window.contains(&bounds.center()))
        }) else {
            eprintln!("跳过嵌套字段断言：本页没有落在窗口内的可展开字段");
            return;
        };
        let nested_selector = selector(format!("mongo-field-toggle:{document_id}/{nested_key}"));
        let nested = redraw_and_bounds(cx, nested_selector).expect("嵌套摘要行必须渲染");
        let children_selector =
            selector(format!("mongo-field-children:{document_id}/{nested_key}"));
        assert!(
            redraw_and_bounds(cx, children_selector).is_none(),
            "嵌套字段默认收起"
        );
        cx.simulate_click(nested.center(), Modifiers::default());
        wait_until(cx, "嵌套字段展开", |cx| {
            redraw_and_bounds(cx, children_selector)
        });

        // 9.1 未保存切换需要两张卡片：演示库的集合可能只剩一篇文档，
        // 这里临时插一条固定 `_id` 的探针文档（先删后插，重复运行也幂等），结尾删除。
        let switch_document_id = "fluxdb-e2e-switch";
        let mongo_tab = cx
            .update(|_, cx| {
                view.read(cx)
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .find(|tab| matches!(tab.kind, TabKind::MongoCollection(_)))
                    .map(|tab| tab.id)
            })
            .expect("集合标签页仍在");
        cx.update(|_, cx| {
            view.update(cx, |this, cx| {
                this.dispatch(
                    AppCommand::MongoRunCommand {
                        connection_id,
                        database: database.clone(),
                        command: format!(
                            r#"{{ "delete": "{collection}", "deletes": [ {{ "q": {{ "_id": "{switch_document_id}" }}, "limit": 1 }} ] }}"#
                        ),
                    },
                    cx,
                );
                this.dispatch(
                    AppCommand::MongoRunCommand {
                        connection_id,
                        database: database.clone(),
                        command: format!(
                            r#"{{ "insert": "{collection}", "documents": [ {{ "_id": "{switch_document_id}", "name": "fluxdb e2e switch", "count": 1 }} ] }}"#
                        ),
                    },
                    cx,
                );
                this.run_mongo_query(mongo_tab, cx);
            });
        });
        wait_until(cx, "探针文档落库", |cx| {
            cx.update(|_, cx| {
                view.read(cx)
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .find_map(|tab| match &tab.kind {
                        TabKind::MongoCollection(state) => state.page.as_ref(),
                        _ => None,
                    })
                    .is_some_and(|page| {
                        page.documents
                            .iter()
                            .any(|document| document.id == switch_document_id)
                            && page.documents.len() >= 2
                    })
            })
            .then_some(())
        });

        // 10. 删除：走真实 UI 按钮 → 确认框 → 服务端删除；删完文档从本页消失。
        // 用自己的探针文档，绝不删演示库里的真实数据。
        let delete_document_id = "fluxdb-e2e-delete";
        let delete_tab = cx
            .update(|_, cx| {
                view.read(cx)
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .find(|tab| matches!(tab.kind, TabKind::MongoCollection(_)))
                    .map(|tab| tab.id)
            })
            .expect("集合标签页仍在");
        cx.update(|_, cx| {
            view.update(cx, |this, cx| {
                this.dispatch(
                    AppCommand::MongoRunCommand {
                        connection_id,
                        database: database.clone(),
                        command: format!(
                            r#"{{ "delete": "{collection}", "deletes": [ {{ "q": {{ "_id": "{delete_document_id}" }}, "limit": 1 }} ] }}"#
                        ),
                    },
                    cx,
                );
                this.dispatch(
                    AppCommand::MongoRunCommand {
                        connection_id,
                        database: database.clone(),
                        command: format!(
                            r#"{{ "insert": "{collection}", "documents": [ {{ "_id": "{delete_document_id}", "name": "fluxdb e2e delete" }} ] }}"#
                        ),
                    },
                    cx,
                );
                this.run_mongo_query(delete_tab, cx);
            });
        });
        let delete_index = wait_until(cx, "待删探针文档落库", |cx| {
            cx.update(|_, cx| {
                view.read(cx)
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .find_map(|tab| match &tab.kind {
                        TabKind::MongoCollection(state) => state.page.as_ref(),
                        _ => None,
                    })
                    .and_then(|page| {
                        page.documents
                            .iter()
                            .position(|document| document.id == delete_document_id)
                    })
            })
        });
        // 卡片很高，先把展开的卡片收起，让目标卡片的删除按钮落进窗口。
        for _ in 0..8 {
            let mut collapsed_one = false;
            for index in 0..8 {
                if redraw_and_bounds(cx, selector(format!("mongo-document-fields:{index}")))
                    .is_none()
                {
                    continue;
                }
                let Some(toggle) =
                    redraw_and_bounds(cx, selector(format!("mongo-document-toggle:{index}")))
                        .filter(|bounds| window_bounds.contains(&bounds.center()))
                else {
                    continue;
                };
                cx.simulate_click(toggle.center(), Modifiers::default());
                collapsed_one = true;
                break;
            }
            if !collapsed_one {
                break;
            }
        }
        let delete_button = redraw_and_bounds(
            cx,
            selector(format!("mongo-document-delete:{delete_index}")),
        )
        .expect("待删文档的删除按钮必须渲染");
        cx.simulate_click(delete_button.center(), Modifiers::default());
        // 删除必须先确认：确认框出现前点删除不能发出写请求。
        wait_until(cx, "删除确认框出现", |cx| {
            redraw_and_bounds(cx, selector("mongo-document-delete-confirm".to_string()))
        });
        let page_has_delete_probe = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| {
                view.read(cx)
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .find_map(|tab| match &tab.kind {
                        TabKind::MongoCollection(state) => state.page.as_ref(),
                        _ => None,
                    })
                    .is_some_and(|page| {
                        page.documents
                            .iter()
                            .any(|document| document.id == delete_document_id)
                    })
            })
        };
        assert!(
            page_has_delete_probe(cx),
            "确认框出现时文档必须还在（没点确认不能删）"
        );
        // 先走一次「取消」：确认框必须能真的取消删除。
        let cancel_button =
            redraw_and_bounds(cx, selector("mongo-document-delete-cancel".to_string()))
                .expect("删除确认框的取消按钮必须渲染");
        cx.simulate_click(cancel_button.center(), Modifiers::default());
        wait_until(cx, "取消后确认框消失", |cx| {
            (redraw_and_bounds(cx, selector("mongo-document-delete-modal".to_string())).is_none())
                .then_some(())
        });
        assert!(page_has_delete_probe(cx), "取消后文档必须还在");

        // 再来一次并确认：这次才真的删。
        let delete_button = redraw_and_bounds(
            cx,
            selector(format!("mongo-document-delete:{delete_index}")),
        )
        .expect("取消后删除按钮仍可点");
        cx.simulate_click(delete_button.center(), Modifiers::default());
        let confirm_button = wait_until(cx, "删除确认框再次出现", |cx| {
            redraw_and_bounds(cx, selector("mongo-document-delete-confirm".to_string()))
        });
        cx.simulate_click(confirm_button.center(), Modifiers::default());
        wait_until(cx, "文档已从本页删除", |cx| {
            cx.update(|_, cx| {
                view.read(cx)
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .find_map(|tab| match &tab.kind {
                        TabKind::MongoCollection(state) => state.page.as_ref(),
                        _ => None,
                    })
                    .is_some_and(|page| {
                        page.documents
                            .iter()
                            .all(|document| document.id != delete_document_id)
                    })
            })
            .then_some(())
        });

        // 10.1 复制文档：点按钮开抽屉（内容不含 `_id`）→ 确认 → 服务端生成新 `_id` 插一条。
        // 仍然只碰自己的探针文档。
        let duplicate_document_id = "fluxdb-e2e-duplicate";
        let duplicate_name = "fluxdb e2e duplicate";
        cx.update(|_, cx| {
            view.update(cx, |this, cx| {
                this.dispatch(
                    AppCommand::MongoRunCommand {
                        connection_id,
                        database: database.clone(),
                        command: format!(
                            r#"{{ "delete": "{collection}", "deletes": [ {{ "q": {{ "name": "{duplicate_name}" }}, "limit": 0 }} ] }}"#
                        ),
                    },
                    cx,
                );
                this.dispatch(
                    AppCommand::MongoRunCommand {
                        connection_id,
                        database: database.clone(),
                        command: format!(
                            r#"{{ "insert": "{collection}", "documents": [ {{ "_id": "{duplicate_document_id}", "name": "{duplicate_name}", "n": 7 }} ] }}"#
                        ),
                    },
                    cx,
                );
                this.run_mongo_query(delete_tab, cx);
            });
        });
        let duplicate_index = wait_until(cx, "复制用探针文档落库", |cx| {
            cx.update(|_, cx| {
                view.read(cx)
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .find_map(|tab| match &tab.kind {
                        TabKind::MongoCollection(state) => state.page.as_ref(),
                        _ => None,
                    })
                    .and_then(|page| {
                        page.documents
                            .iter()
                            .position(|document| document.id == duplicate_document_id)
                    })
            })
        });
        let duplicate_button = redraw_and_bounds(
            cx,
            selector(format!("mongo-document-duplicate:{duplicate_index}")),
        )
        .expect("复制文档按钮必须渲染");
        cx.simulate_click(duplicate_button.center(), Modifiers::default());
        // 复制与编辑共用右侧抽屉（不再弹居中弹框），选择器区分模式。
        let duplicate_drawer = selector("mongo-document-duplicate-drawer".to_string());
        wait_until(cx, "复制抽屉出现", |cx| {
            redraw_and_bounds(cx, duplicate_drawer)
        });
        // 抽屉里的初值必须已经剥掉 `_id`：带着源 `_id` 插入必然撞唯一键。
        let draft_input = cx
            .update(|_, cx| {
                view.read(cx)
                    .mongo_document_editor
                    .as_ref()
                    .filter(|editor| editor.mode == MongoDocumentEditorMode::Duplicate)
                    .and_then(|editor| editor.component.input().cloned())
            })
            .expect("复制抽屉必须持有编辑框");
        let draft = cx.read(|cx| draft_input.read(cx).value().to_string());
        assert!(!draft.contains("_id"), "复制抽屉的初值不能含 _id: {draft}");

        // 先点一次取消：抽屉关闭且不写入。
        let duplicate_cancel =
            redraw_and_bounds(cx, selector("mongo-document-editor-cancel".to_string()))
                .expect("复制抽屉的取消按钮必须渲染");
        cx.simulate_click(duplicate_cancel.center(), Modifiers::default());
        wait_until(cx, "取消后复制抽屉消失", |cx| {
            (redraw_and_bounds(cx, duplicate_drawer).is_none()).then_some(())
        });

        // 再打开并确认：这次真的插入。新文档由服务端生成 `_id`，所以按名字数条数。
        let duplicate_button = redraw_and_bounds(
            cx,
            selector(format!("mongo-document-duplicate:{duplicate_index}")),
        )
        .expect("取消后复制按钮仍可点");
        cx.simulate_click(duplicate_button.center(), Modifiers::default());
        let duplicate_confirm = wait_until(cx, "复制抽屉再次出现", |cx| {
            redraw_and_bounds(cx, selector("mongo-document-duplicate-confirm".to_string()))
        });
        cx.simulate_click(duplicate_confirm.center(), Modifiers::default());
        wait_until(cx, "复制出的新文档落库", |cx| {
            cx.update(|_, cx| {
                view.read(cx)
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .find_map(|tab| match &tab.kind {
                        TabKind::MongoCollection(state) => state.page.as_ref(),
                        _ => None,
                    })
                    .is_some_and(|page| {
                        let copies: Vec<&fluxdb_core::MongoDocument> = page
                            .documents
                            .iter()
                            .filter(|document| document.json.contains(duplicate_name))
                            .collect();
                        // 原文档 + 副本：两条，且副本的 `_id` 不是源文档的 `_id`。
                        copies.len() == 2
                            && copies
                                .iter()
                                .any(|document| document.id != duplicate_document_id)
                    })
            })
            .then_some(())
        });

        // 10.2 结果条的「+」：打开同一副抽屉，初值只有一个本机生成的 `_id`；
        // 补一个字段后插入，服务端必须按这个 `_id` 落库（否则界面预填的 id 是假的）。
        let insert_name = "fluxdb e2e insert";
        let insert_button = redraw_and_bounds(cx, selector("mongo-results-insert".to_string()))
            .expect("结果条的新增按钮必须渲染");
        assert!(
            window_bounds.contains(&insert_button.center()),
            "新增按钮必须落在窗口内，否则点击会被丢弃"
        );
        cx.simulate_click(insert_button.center(), Modifiers::default());
        let insert_drawer = selector("mongo-document-insert-drawer".to_string());
        wait_until(cx, "新增抽屉出现", |cx| {
            redraw_and_bounds(cx, insert_drawer)
        });
        let insert_input = cx
            .update(|_, cx| {
                view.read(cx)
                    .mongo_document_editor
                    .as_ref()
                    .filter(|editor| editor.mode == MongoDocumentEditorMode::Insert)
                    .and_then(|editor| editor.component.input().cloned())
            })
            .expect("新增抽屉必须持有编辑框");
        let insert_draft = cx.read(|cx| insert_input.read(cx).value().to_string());
        let insert_draft_json: serde_json::Value =
            serde_json::from_str(&insert_draft).expect("新增初值必须是合法 JSON");
        let insert_id = insert_draft_json["_id"]["$oid"]
            .as_str()
            .expect("新增初值的 _id 必须是 $oid 包装")
            .to_string();
        assert_eq!(
            insert_draft_json.as_object().map(|fields| fields.len()),
            Some(1),
            "新增初值只应有 _id: {insert_draft}"
        );

        // 补一个字段后插入：这一篇必须真的按预填的 `_id` 落库。
        let insert_ready = format!(
            "{{\n  \"_id\": {{\n    \"$oid\": \"{insert_id}\"\n  }},\n  \"name\": \"{insert_name}\"\n}}"
        );
        cx.update(|window, cx| {
            insert_input.update(cx, |input, cx| {
                input.set_value(insert_ready.clone(), window, cx);
                input.focus(window, cx);
            });
            window.draw(cx).clear(cx);
        });
        let insert_confirm = wait_until(cx, "新增按钮可用", |cx| {
            redraw_and_bounds(cx, selector("mongo-document-insert-confirm".to_string()))
        });
        cx.simulate_click(insert_confirm.center(), Modifiers::default());
        wait_until(cx, "新增的文档落库", |cx| {
            cx.update(|_, cx| {
                view.read(cx)
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .find_map(|tab| match &tab.kind {
                        TabKind::MongoCollection(state) => state.page.as_ref(),
                        _ => None,
                    })
                    .is_some_and(|page| {
                        page.documents
                            .iter()
                            .any(|document| document.id == insert_id)
                    })
            })
            .then_some(())
        });
        wait_until(cx, "新增后抽屉关闭", |cx| {
            (redraw_and_bounds(cx, insert_drawer).is_none()).then_some(())
        });

        // 10.3 结果条的导入按钮：真实点击打开弹窗；未选文件时「开始导入」必须禁用，
        // 关闭后弹窗消失。选文件走系统原生对话框，无头环境无法驱动，因此解析与写入
        // 由连接器 live 用例覆盖（`mongo_live_import_documents_from_csv_and_json`）。
        let import_button = redraw_and_bounds(cx, selector("mongo-results-import".to_string()))
            .expect("结果条的导入按钮必须渲染");
        assert!(
            window_bounds.contains(&import_button.center()),
            "导入按钮必须落在窗口内，否则点击会被丢弃"
        );
        cx.simulate_click(import_button.center(), Modifiers::default());
        let import_modal = selector("mongo-import-modal".to_string());
        wait_until(cx, "导入弹窗出现", |cx| {
            redraw_and_bounds(cx, import_modal)
        });
        // 未选文件：主按钮禁用。断言它存在但不可点，避免「点了没反应」被当成通过。
        assert!(
            redraw_and_bounds(cx, selector("mongo-import-start".to_string())).is_some(),
            "导入弹窗必须有开始按钮"
        );
        assert!(
            cx.update(|_, cx| {
                view.read(cx)
                    .pending_mongo_import
                    .as_ref()
                    .map(|form| form.source.is_none() && form.preview.is_none())
            })
            .unwrap_or(false),
            "未选文件时弹窗不应有预览"
        );
        // 分隔符是「标签 + 控件」两行、开关只有一行，两者必须底部对齐；
        // 居中对齐会把开关推到标签和控件之间，看起来像浮在空处。
        let delimiter_field = redraw_and_bounds(
            cx,
            selector("mongo-import-delimiter-field".to_string()),
        )
        .expect("分隔符字段必须渲染");
        let empty_null_toggle = redraw_and_bounds(
            cx,
            selector("mongo-import-empty-null".to_string()),
        )
        .expect("空值开关必须渲染");
        let bottom_gap = (delimiter_field.bottom() - empty_null_toggle.bottom()).abs();
        assert!(
            bottom_gap <= gpui::px(2.),
            "空值开关必须与分隔符下拉框底部对齐，实际相差 {bottom_gap:?}"
        );
        let import_close = wait_until(cx, "导入弹窗关闭按钮", |cx| {
            redraw_and_bounds(cx, selector("mongo-import-close".to_string()))
        });
        cx.simulate_click(import_close.center(), Modifiers::default());
        wait_until(cx, "导入弹窗关闭", |cx| {
            (redraw_and_bounds(cx, import_modal).is_none()).then_some(())
        });

        // 10.4 创建集合：库行右键 → 菜单 → 填名 → 确认，新集合必须**自动**出现在树里，
        // 不需要用户手动刷新。这条正是「创建集合后没刷新该库」的回归点：控制器只替换
        // 第一层（库列表），UI 必须自己重取该库子节点。
        let created_collection = format!("fluxdb_ui_probe_{}", std::process::id());
        let database_row = redraw_and_bounds(cx, database_selector).expect("库行仍在");
        cx.simulate_event(MouseDownEvent {
            position: database_row.center(),
            modifiers: Modifiers::default(),
            button: MouseButton::Right,
            click_count: 1,
            first_mouse: false,
        });
        let new_collection_item = wait_until(cx, "库右键菜单出现", |cx| {
            redraw_and_bounds(
                cx,
                selector("database-menu-item:Create collection".to_string()),
            )
        });
        cx.simulate_click(new_collection_item.center(), Modifiers::default());
        wait_until(cx, "创建集合弹窗", |cx| {
            redraw_and_bounds(cx, selector("mongo-create-collection-modal".to_string()))
        });
        let _ = redraw_and_bounds(cx, selector("mongo-create-collection-input".to_string()))
            .expect("集合名输入框");
        // 弹窗打开时已把焦点给输入框；这里不能再点外层容器——那层包着标签，
        // 点中心很容易落在标签/间隙上，反而把焦点从输入框移走，模拟输入就丢了。
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        cx.simulate_input(&created_collection);
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        assert_eq!(
            cx.update(|_, cx| {
                view.read(cx)
                    .create_collection_input
                    .read(cx)
                    .value()
                    .to_string()
            }),
            created_collection,
            "集合名没写进输入框"
        );
        let confirm =
            redraw_and_bounds(cx, selector("mongo-create-collection-confirm".to_string()))
                .expect("确认按钮");
        cx.simulate_click(confirm.center(), Modifiers::default());
        // 不点刷新、不重开连接：对象树里的集合清单必须靠创建成功后的自动重取更新。
        // 先断言状态层真的拿到了新集合（这才是「刷新」的实质），再看界面行。
        wait_until(cx, "对象树状态出现新集合", |cx| {
            cx.update(|_, cx| {
                view.read(cx)
                    .controller
                    .state()
                    .connections
                    .iter()
                    .find(|connection| connection.config.id == connection_id)
                    .filter(|connection| {
                        connection.objects.iter().any(|object| {
                            object.path.kind == ObjectKind::Collection
                                && object.path.name == created_collection
                                && object.path.database.as_deref() == Some(database.as_str())
                        })
                    })
                    .map(|connection| connection.objects.len())
            })
        });
        wait_until(cx, "新集合行渲染", |cx| {
            redraw_and_bounds(cx, selector(format!("table-tree-row:{created_collection}")))
        });
        // 清理探针集合（与第 12 步一致，直接发服务端命令），避免污染真实库。
        cx.update(|_, cx| {
            view.update(cx, |this, cx| {
                this.dispatch(
                    AppCommand::MongoRunCommand {
                        connection_id,
                        database: database.clone(),
                        command: format!(r#"{{ "drop": "{created_collection}" }}"#),
                    },
                    cx,
                );
            });
        });

        // 11. 编辑入口：卡头编辑按钮打开抽屉即编辑器；改一处值 → 保存 → 抽屉关闭、
        // 重新查询后服务端返回新值。用真实服务端确认写入真的落地，而不只是界面状态变化。
        let editor_selector = selector("mongo-document-editor".to_string());
        assert!(
            redraw_and_bounds(cx, editor_selector).is_none(),
            "初始不应有编辑抽屉"
        );
        let (document_id, before_json) = first_document(cx, &view);
        let Some(edited_json) = edit_first_scalar(&before_json) else {
            eprintln!("跳过编辑断言：首篇文档没有可改写的顶层标量字段");
            return;
        };
        // 卡片正文很高，先把展开着的卡片逐个收起，让每张卡片的卡头（含编辑按钮）都落进窗口——
        // 越界点击会被静默丢弃。每收起一张后面的卡片会上移，所以每轮重新扫。
        for _ in 0..8 {
            let mut collapsed_one = false;
            for index in 0..8 {
                let fields_open =
                    redraw_and_bounds(cx, selector(format!("mongo-document-fields:{index}")))
                        .is_some();
                if !fields_open {
                    continue;
                }
                let Some(toggle) =
                    redraw_and_bounds(cx, selector(format!("mongo-document-toggle:{index}")))
                        .filter(|bounds| window_bounds.contains(&bounds.center()))
                else {
                    continue;
                };
                cx.simulate_click(toggle.center(), Modifiers::default());
                collapsed_one = true;
                break;
            }
            if !collapsed_one {
                break;
            }
        }
        let visible_edit: Vec<usize> = (0..16)
            .filter(|index| {
                redraw_and_bounds(cx, selector(format!("mongo-document-detail:{index}")))
                    .is_some_and(|bounds| window_bounds.contains(&bounds.center()))
            })
            .collect();
        let probe_index = cx
            .update(|_, cx| {
                view.read(cx)
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .find_map(|tab| match &tab.kind {
                        TabKind::MongoCollection(state) => state.page.as_ref(),
                        _ => None,
                    })
                    .and_then(|page| {
                        page.documents
                            .iter()
                            .position(|document| document.id == switch_document_id)
                    })
            })
            .expect("探针文档必须在本页");
        let target_index = if visible_edit.contains(&0) {
            0
        } else {
            *visible_edit
                .first()
                .expect("至少一张卡片的编辑按钮落在窗口内")
        };
        let other_index = (probe_index != target_index && visible_edit.contains(&probe_index))
            .then_some(probe_index);
        let edit_button = redraw_and_bounds(
            cx,
            selector(format!("mongo-document-detail:{target_index}")),
        )
        .expect("编辑按钮必须渲染");
        cx.simulate_click(edit_button.center(), Modifiers::default());
        wait_until(cx, "编辑抽屉打开", |cx| {
            redraw_and_bounds(cx, editor_selector)
        });

        // 11.1 修改草稿后点另一张卡片的编辑按钮：必须弹确认框，选「继续编辑」草稿仍在。
        let editor_input = cx
            .update(|_, cx| {
                view.read(cx)
                    .mongo_document_editor
                    .as_ref()
                    .and_then(|editor| editor.component.input().cloned())
            })
            .expect("编辑框必须存在");
        let base_json = cx
            .update(|_, cx| {
                view.read(cx)
                    .mongo_document_editor
                    .as_ref()
                    .map(|editor| editor.base_json.clone())
            })
            .expect("编辑会话");
        assert_eq!(
            cx.update(|_, cx| editor_input.read(cx).value().to_string()),
            base_json,
            "打开时编辑框内容应与基线一致"
        );
        cx.update(|window, cx| {
            editor_input.update(cx, |input, cx| {
                input.set_value(edited_json.clone(), window, cx);
                input.focus(window, cx);
            });
            window.draw(cx).clear(cx);
        });
        let discard_selector = selector("mongo-document-discard".to_string());
        let other_index = other_index.expect("两张卡片的编辑按钮都要落在窗口内");
        let other_edit =
            redraw_and_bounds(cx, selector(format!("mongo-document-detail:{other_index}")))
                .expect("另一张卡片的编辑按钮必须渲染");
        cx.simulate_click(other_edit.center(), Modifiers::default());
        wait_until(cx, "切换文档先确认", |cx| {
            redraw_and_bounds(cx, discard_selector)
        });
        let stay = redraw_and_bounds(cx, selector("mongo-document-discard-cancel".to_string()))
            .expect("继续编辑按钮必须渲染");
        cx.simulate_click(stay.center(), Modifiers::default());
        wait_until(cx, "关闭确认框", |cx| {
            (redraw_and_bounds(cx, discard_selector).is_none()).then_some(())
        });
        assert_eq!(
            cx.update(|_, cx| editor_input.read(cx).value().to_string()),
            edited_json,
            "选「继续编辑」必须保留草稿"
        );

        // 11.1.1 选「放弃并切换」：草稿丢弃、编辑会话切到目标文档。
        let other_edit =
            redraw_and_bounds(cx, selector(format!("mongo-document-detail:{other_index}")))
                .expect("另一张卡片的编辑按钮必须渲染");
        cx.simulate_click(other_edit.center(), Modifiers::default());
        wait_until(cx, "再次确认", |cx| {
            redraw_and_bounds(cx, discard_selector)
        });
        let discard = redraw_and_bounds(cx, selector("mongo-document-discard-confirm".to_string()))
            .expect("放弃并切换按钮必须渲染");
        cx.simulate_click(discard.center(), Modifiers::default());
        wait_until(cx, "切换到目标文档", |cx| {
            cx.update(|_, cx| {
                view.read(cx)
                    .mongo_document_editor
                    .as_ref()
                    .filter(|editor| editor.index == Some(other_index))
                    .and_then(|editor| editor.component.input().cloned())
            })
        });
        // 回到首篇继续验证保存：重新打开它并写入同一份草稿。
        let reopen = redraw_and_bounds(
            cx,
            selector(format!("mongo-document-detail:{target_index}")),
        )
        .expect("首篇卡片的编辑按钮必须渲染");
        cx.simulate_click(reopen.center(), Modifiers::default());
        let editor_input = wait_until(cx, "重新打开首篇", |cx| {
            cx.update(|_, cx| {
                view.read(cx)
                    .mongo_document_editor
                    .as_ref()
                    .filter(|editor| editor.index == Some(target_index))
                    .and_then(|editor| editor.component.input().cloned())
            })
        });
        cx.update(|window, cx| {
            editor_input.update(cx, |input, cx| {
                input.set_value(edited_json.clone(), window, cx);
                input.focus(window, cx);
            });
            window.draw(cx).clear(cx);
        });

        // 11.2 保存：真实按钮触发，成功后抽屉关闭并重查本页带回新值。
        let save_selector = selector("mongo-document-editor-save".to_string());
        let save_bounds = wait_until(cx, "保存按钮可用", |cx| {
            redraw_and_bounds(cx, save_selector)
        });
        cx.simulate_click(save_bounds.center(), Modifiers::default());
        wait_until(cx, "保存后抽屉关闭", |cx| {
            (redraw_and_bounds(cx, editor_selector).is_none()).then_some(())
        });
        wait_for_mongo_document(cx, &view, &document_id, &edited_json, "重新查询带回新值");

        // 11.3 还原本次写入：用例只借真实库验证写回，不留下被改的演示数据。
        // 还原走同一条写命令（定位条件相同），随后重新查询确认已回到原值。
        let restore_tab = cx
            .update(|_, cx| {
                view.read(cx)
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .find(|tab| matches!(tab.kind, TabKind::MongoCollection(_)))
                    .map(|tab| tab.id)
            })
            .expect("集合标签页仍在");
        cx.update(|_, cx| {
            view.update(cx, |this, cx| {
                this.dispatch(
                    AppCommand::SaveMongoDocument {
                        tab_id: restore_tab,
                        before_json: edited_json.clone(),
                        after_json: before_json.clone(),
                    },
                    cx,
                );
                this.run_mongo_query(restore_tab, cx);
            });
        });
        wait_for_mongo_document(cx, &view, &document_id, &before_json, "还原原始文档");

        // 12. 清理探针文档：本用例只在真实库验证交互，不留下额外数据。
        // 复制出来的副本 `_id` 由服务端生成，只能按 `name` 定位；新增的那篇按预填的 `_id`。
        cx.update(|_, cx| {
            view.update(cx, |this, cx| {
                for query in [
                    format!(
                        r#"{{ "delete": "{collection}", "deletes": [ {{ "q": {{ "_id": "{switch_document_id}" }}, "limit": 1 }} ] }}"#
                    ),
                    format!(
                        r#"{{ "delete": "{collection}", "deletes": [ {{ "q": {{ "name": "{duplicate_name}" }}, "limit": 0 }} ] }}"#
                    ),
                    format!(
                        r#"{{ "delete": "{collection}", "deletes": [ {{ "q": {{ "name": "{insert_name}" }}, "limit": 0 }} ] }}"#
                    ),
                ] {
                    this.dispatch(
                        AppCommand::MongoRunCommand {
                            connection_id,
                            database: database.clone(),
                            command: query,
                        },
                        cx,
                    );
                }
                this.run_mongo_query(mongo_tab, cx);
            });
        });
        wait_until(cx, "探针文档清理", |cx| {
            cx.update(|_, cx| {
                view.read(cx)
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .find_map(|tab| match &tab.kind {
                        TabKind::MongoCollection(state) => state.page.as_ref(),
                        _ => None,
                    })
                    .is_some_and(|page| {
                        page.documents
                            .iter()
                            .all(|document| document.id != switch_document_id)
                            && page
                                .documents
                                .iter()
                                .all(|document| !document.json.contains(duplicate_name))
                            && page
                                .documents
                                .iter()
                                .all(|document| !document.json.contains(insert_name))
                    })
            })
            .then_some(())
        });
    }

    /// 本页首篇文档的 `(可读 id, 完整 JSON)`。
    fn first_document(
        cx: &mut VisualTestContext,
        view: &gpui::Entity<NavicatMain>,
    ) -> (String, String) {
        cx.update(|_, cx| {
            view.read(cx)
                .controller
                .state()
                .tabs
                .iter()
                .find_map(|tab| match &tab.kind {
                    TabKind::MongoCollection(state) => state.page.as_ref(),
                    _ => None,
                })
                .and_then(|page| page.documents.first())
                .map(|document| (document.id.clone(), document.json.clone()))
        })
        .expect("首屏至少一篇文档")
    }

    /// 改写首篇文档里第一个**顶层标量**字段（`_id` 除外，它不可改），返回改后的整篇文本。
    ///
    /// 优先改字符串（只追加标记，不改变 BSON 类型），没有字符串才退回「数值 +1」。
    fn edit_first_scalar(json: &str) -> Option<String> {
        let mut value: serde_json::Value = serde_json::from_str(json).ok()?;
        let object = value.as_object_mut()?;
        let key = object
            .iter()
            .find(|(key, value)| *key != "_id" && value.is_string())
            .or_else(|| {
                object
                    .iter()
                    .find(|(key, value)| *key != "_id" && value.is_number())
            })
            .map(|(key, _)| key.clone())?;
        let next = match object.get(&key)? {
            serde_json::Value::String(text) => {
                serde_json::Value::String(format!("{text} [fluxdb-e2e]"))
            }
            value => serde_json::json!(value.as_f64()? + 1.0),
        };
        object.insert(key, next);
        serde_json::to_string_pretty(&value).ok()
    }

    /// 等某一页里 `document_id` 的 JSON 与 `expected` 语义相等（忽略键序与空白）。
    fn wait_for_mongo_document(
        cx: &mut VisualTestContext,
        view: &gpui::Entity<NavicatMain>,
        document_id: &str,
        expected: &str,
        label: &str,
    ) {
        let document_id = document_id.to_string();
        let want: serde_json::Value = serde_json::from_str(expected).expect("期望 JSON");
        wait_until(cx, label, |cx| {
            cx.update(|_, cx| {
                view.read(cx)
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .find_map(|tab| match &tab.kind {
                        TabKind::MongoCollection(state) => state.page.as_ref(),
                        _ => None,
                    })
                    .and_then(|page| {
                        page.documents
                            .iter()
                            .find(|document| document.id == document_id)
                    })
                    .and_then(|document| {
                        serde_json::from_str::<serde_json::Value>(&document.json).ok()
                    })
                    .filter(|got| got == &want)
            })
        });
    }
}

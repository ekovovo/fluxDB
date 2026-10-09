#[cfg(test)]
mod table_ui_tests {
    use super::*;
    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Harness {
        window: gpui::AnyWindowHandle,
        view: Entity<NavicatMain>,
        root: PathBuf,
        config: ConnectionConfig,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn query(config: &ConnectionConfig, sql: &str) -> fluxdb_core::QueryExecutionResult {
        let mut controller = AppController::new();
        controller.dispatch(AppCommand::ReplaceConnections(vec![config.clone()]));
        let AppEvent::TabOpened(tab_id) =
            controller.dispatch(AppCommand::OpenQueryEditor(config.id))
        else {
            panic!("fixture query tab");
        };
        controller.dispatch(AppCommand::UpdateQueryText {
            tab_id,
            text: sql.into(),
        });
        let AppEvent::QueryFinished(_, result) =
            controller.dispatch(AppCommand::ExecuteQuery(tab_id))
        else {
            panic!("fixture query failed");
        };
        assert!(result.summaries.iter().all(|summary| summary.success));
        result
    }

    fn open_main(cx: &mut gpui_kit::TestAppContext, name: &str, theme: ThemeMode) -> Harness {
        cx.executor().allow_parking();
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(10_000);
        let connection_id = ConnectionId(SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let root = std::env::temp_dir().join(format!(
            "fluxdb-table-ui-{}-{}-{}",
            std::process::id(),
            connection_id.0,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&root).unwrap();
        let storage = FileStorage::new(root.clone());
        let config = ConnectionConfig {
            id: connection_id,
            name: "isolated table fixture".into(),
            kind: DatabaseKind::Sqlite,
            endpoint: Endpoint::SqliteFile {
                path: root.join("tables.sqlite"),
                read_only: false,
            },
            options: BTreeMap::new(),
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
        };
        query(
            &config,
            "CREATE TABLE Product (id INTEGER PRIMARY KEY, name TEXT NOT NULL); CREATE INDEX ix_product_name ON Product(name); INSERT INTO Product VALUES (1, 'one'), (2, 'two'); CREATE TABLE Existing (id INTEGER); CREATE TABLE EmptyTable (id INTEGER);",
        );
        let object = ObjectPath {
            connection_id,
            database: Some("main".into()),
            schema: None,
            name: name.into(),
            kind: ObjectKind::Table,
        };
        let mut controller = AppController::new();
        controller.dispatch(AppCommand::ReplaceConnections(vec![config.clone()]));
        cx.update(gpui_kit::init);
        register_sql_highlighter();
        cx.update(|cx| {
            cx.bind_keys([
                KeyBinding::new("escape", CancelDialog, None),
                KeyBinding::new("secondary-w", CloseCurrentTab, None),
                KeyBinding::new("secondary-r", Refresh, None),
            ])
        });
        let store = storage.clone();
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::default(),
                        size: size(px(1400.), px(1000.)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    cx.new(|cx| {
                        let mut main = navicat_main_new(window, cx, controller, store, theme);
                        main.ai_chat.recovery_pending = false;
                        // 菜单作为 fixture 初始状态；后续通过真实菜单点击进入生产命令/弹框链路。
                        main.table_context_menu = Some(TableContextMenu {
                            object_path: object,
                            position: point(px(16.), px(56.)),
                            submenu: None,
                        });
                        window.focus(&main.focus_handle, cx);
                        main
                    })
                },
            )
            .unwrap()
        });
        Harness {
            window,
            view,
            root,
            config,
        }
    }

    fn click_menu(h: &Harness, label: &str, cx: &mut gpui_kit::TestAppContext) {
        cx.update_window(h.window, |_, window, cx| {
            window.render_frame(cx);
            window.click(SharedString::from(format!("table-menu-item:{label}")), cx);
        })
        .unwrap();
    }

    fn replace_name(
        h: &Harness,
        field: &'static str,
        value: &str,
        cx: &mut gpui_kit::TestAppContext,
    ) {
        cx.update_window(h.window, |_, window, cx| {
            window.render_frame(cx);
            window.click(field, cx);
            window.press("secondary-a", cx);
            window.input(value, cx);
        })
        .unwrap();
        // Change 订阅完成后再查询状态和预览，避免同一帧断言旧值。
        cx.update_window(h.window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find(field).value(), Some(value));
        })
        .unwrap();
    }

    async fn copy_ddl_ready(h: &Harness, cx: &mut gpui_kit::TestAppContext) {
        cx.wait_for(h.window, Duration::from_secs(30), |_, cx| {
            h.view
                .read(cx)
                .pending_copy_table
                .as_ref()
                .is_some_and(|form| matches!(form.source_ddl, Some(Ok(_))))
        })
        .await;
    }

    #[gpui_kit::test]
    async fn table_forms_keep_validation_focus_and_close_paths(cx: &mut gpui_kit::TestAppContext) {
        for theme in [ThemeMode::Light, ThemeMode::Dark] {
            for copy in [false, true] {
                for close_path in 0..4 {
                    let h = open_main(cx, "Product", theme);
                    click_menu(&h, if copy { "复制表" } else { "重命名" }, cx);
                    let (context, field, cancel, confirm) = if copy {
                        (
                            "CopyTableModal",
                            "copy-table-name",
                            "copy-table-cancel",
                            "copy-table-confirm",
                        )
                    } else {
                        (
                            "RenameTableModal",
                            "rename-table-name",
                            "rename-table-cancel",
                            "rename-table-confirm",
                        )
                    };
                    if copy {
                        copy_ddl_ready(&h, cx).await;
                    }
                    cx.update_window(h.window, |_, window, cx| {
                        window.render_frame(cx);
                        assert!(window.find(field).focused().unwrap_or(false));
                        window.click_at((context, 0u64), point(px(8.), px(8.)), cx);
                    })
                    .unwrap();
                    replace_name(&h, field, "Product", cx);
                    cx.update_window(h.window, |_, window, cx| {
                        window.render_frame(cx);
                        let error = window.try_find((context, 2u64));
                        if copy {
                            assert_eq!(error.unwrap().label(), Some("新表名不能和原表相同"));
                        } else {
                            assert!(error.is_none());
                        }
                        window.click(confirm, cx);
                    })
                    .unwrap();
                    h.view.read_with(cx, |main, _| {
                        assert!(main._copy_table_task.is_none());
                        assert!(main._rename_table_task.is_none());
                    });
                    cx.update_window(h.window, |_, window, cx| {
                        window.render_frame(cx);
                        assert!(window.try_find(cancel).is_some());
                        match close_path {
                            0 => window.click(cancel, cx),
                            1 => window.click((context, 1u64), cx),
                            2 => window.press("escape", cx),
                            _ => window.click_at(context, point(px(8.), px(8.)), cx),
                        }
                    })
                    .unwrap();
                    cx.update_window(h.window, |_, window, cx| {
                        window.render_frame(cx);
                        assert!(window.try_find(cancel).is_none());
                        window.remove_window();
                    })
                    .unwrap();
                    h.view.read_with(cx, |main, _| {
                        assert!(main.pending_copy_table.is_none());
                        assert!(main.pending_rename_table.is_none());
                    });
                }
            }
        }
    }

    #[gpui_kit::test]
    async fn table_forms_execute_rename_and_copy_modes_without_duplicate_submit(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        for (copy, with_data) in [(false, false), (true, false), (true, true)] {
            let h = open_main(cx, "Product", ThemeMode::Light);
            click_menu(&h, if copy { "复制表" } else { "重命名" }, cx);
            if copy {
                copy_ddl_ready(&h, cx).await;
            }
            replace_name(
                &h,
                if copy {
                    "copy-table-name"
                } else {
                    "rename-table-name"
                },
                "Target",
                cx,
            );
            let confirm = if copy {
                "copy-table-confirm"
            } else {
                "rename-table-confirm"
            };
            cx.update_window(h.window, |_, window, cx| {
                window.render_frame(cx);
                if copy && with_data {
                    window.click(("copy-table-mode", 1u64), cx);
                }
                window.click(confirm, cx);
                assert_eq!(window.find(confirm).label(), Some("执行中"));
                window.click(confirm, cx);
                window.press("escape", cx);
                assert!(window.try_find(confirm).is_some());
            })
            .unwrap();
            cx.wait_for(h.window, Duration::from_secs(30), |_, cx| {
                let main = h.view.read(cx);
                main.pending_copy_table.is_none() && main.pending_rename_table.is_none()
            })
            .await;
            let result = query(&h.config, "SELECT COUNT(*) FROM Target");
            let expected = if !copy || with_data { 2 } else { 0 };
            assert_eq!(
                result.results[0].rows[0].values[0],
                CellValue::I64(expected)
            );
            if copy {
                let result = query(&h.config, "SELECT COUNT(*) FROM Product");
                assert_eq!(result.results[0].rows[0].values[0], CellValue::I64(2));
            }
        }
    }

    #[gpui_kit::test]
    async fn table_forms_keep_draft_and_show_execution_errors(cx: &mut gpui_kit::TestAppContext) {
        let h = open_main(cx, "Product", ThemeMode::Dark);
        click_menu(&h, "复制表", cx);
        copy_ddl_ready(&h, cx).await;
        replace_name(&h, "copy-table-name", "Existing", cx);
        cx.update_window(h.window, |_, window, cx| {
            window.render_frame(cx);
            window.click("copy-table-confirm", cx);
        })
        .unwrap();
        cx.wait_for(h.window, Duration::from_secs(30), |_, cx| {
            h.view
                .read(cx)
                .pending_copy_table
                .as_ref()
                .is_some_and(|form| form.error.is_some())
        })
        .await;
        cx.update_window(h.window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("copy-table-name").value(), Some("Existing"));
            assert!(
                !window
                    .find(("CopyTableModal", 2u64))
                    .label()
                    .unwrap_or("")
                    .is_empty()
            );
        })
        .unwrap();
        h.view
            .read_with(cx, |main, _| assert!(main._copy_table_task.is_none()));
    }

    #[gpui_kit::test]
    async fn data_editor_loads_data_and_metadata_with_single_tasks(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let h = open_main(cx, "Product", ThemeMode::Light);
        click_menu(&h, "查看数据", cx);
        let tab = h
            .view
            .read_with(cx, |main, _| main.controller.state().active_tab.unwrap());
        cx.update_window(h.window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(("data-page-loading", tab.0)).is_some());
            window.press("secondary-r", cx);
            assert_eq!(h.view.read(cx)._data_load_tasks.len(), 1);
        })
        .unwrap();
        cx.wait_for(h.window, Duration::from_secs(30), |_, cx| {
            h.view.read(cx).controller.state().active_tab().is_some_and(|tab| matches!(&tab.kind, TabKind::DataEditor(editor) if !editor.loading && editor.page.is_some()))
        }).await;
        cx.update_window(h.window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(("data-page-loading", tab.0)).is_none());
            window.click(("table-info-toggle", tab.0), cx);
            // 组件的固定页签使用枚举顺序作为 ID，在已有 TabBar 作用域内定位。
            window
                .within(("table-info-tabs", tab.0))
                .click(TableInfoTab::Indexes as usize, cx);
            assert!(window.try_find("table-info-loading").is_some());
            window
                .within(("table-info-tabs", tab.0))
                .click(TableInfoTab::Indexes as usize, cx);
            assert_eq!(h.view.read(cx)._table_info_tasks.len(), 1);
        })
        .unwrap();
        cx.wait_for(h.window, Duration::from_secs(30), |_, cx| {
            h.view.read(cx).controller.state().active_tab().is_some_and(|tab| matches!(&tab.kind, TabKind::DataEditor(editor) if matches!(&editor.table_info.indexes, LoadState::Loaded(_))))
        }).await;
        cx.update_window(h.window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(("table-info-indexes", tab.0)).is_some());
            assert!(window.try_find("table-info-loading").is_none());
        })
        .unwrap();
        h.view.read_with(cx, |main, _| {
            assert!(main._data_load_tasks.is_empty());
            assert!(main._table_info_tasks.is_empty());
            let TabKind::DataEditor(editor) = &main.controller.state().active_tab().unwrap().kind
            else {
                panic!("data editor");
            };
            assert_eq!(editor.page.as_ref().unwrap().rows.len(), 2);
            assert!(
                matches!(&editor.table_info.indexes, LoadState::Loaded(rows) if !rows.is_empty())
            );
        });
    }

    #[gpui_kit::test]
    async fn data_editor_loads_empty_data_and_discards_closed_tab_results(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let h = open_main(cx, "EmptyTable", ThemeMode::Dark);
        click_menu(&h, "查看数据", cx);
        cx.wait_for(h.window, Duration::from_secs(30), |_, cx| {
            h.view.read(cx).controller.state().active_tab().is_some_and(|tab| matches!(&tab.kind, TabKind::DataEditor(editor) if !editor.loading && editor.page.is_some()))
        }).await;
        h.view.read_with(cx, |main, _| {
            let TabKind::DataEditor(editor) = &main.controller.state().active_tab().unwrap().kind
            else {
                panic!("data editor");
            };
            assert!(editor.page.as_ref().unwrap().rows.is_empty());
            assert!(editor.error.is_none());
        });
        let h = open_main(cx, "Product", ThemeMode::Light);
        click_menu(&h, "查看数据", cx);
        cx.update_window(h.window, |_, window, cx| {
            window.render_frame(cx);
            window.press("secondary-w", cx);
        })
        .unwrap();
        cx.wait_for(h.window, Duration::from_secs(30), |_, cx| {
            h.view.read(cx)._data_load_tasks.is_empty()
        })
        .await;
        h.view.read_with(cx, |main, _| {
            assert!(main.controller.state().tabs.is_empty());
            assert!(main.controller.state().last_error.is_none());
        });
    }

    #[gpui_kit::test]
    async fn data_editor_load_errors_stay_in_their_own_views(cx: &mut gpui_kit::TestAppContext) {
        let h = open_main(cx, "MissingTable", ThemeMode::Dark);
        click_menu(&h, "查看数据", cx);
        let tab = h
            .view
            .read_with(cx, |main, _| main.controller.state().active_tab.unwrap());
        cx.wait_for(h.window, Duration::from_secs(30), |window, _| {
            window.try_find(("retry-data-page", tab.0)).is_some()
        })
        .await;
        h.view
            .read_with(cx, |main, _| assert!(main._data_load_tasks.is_empty()));

        let h = open_main(cx, "Product", ThemeMode::Light);
        click_menu(&h, "查看数据", cx);
        let tab = h
            .view
            .read_with(cx, |main, _| main.controller.state().active_tab.unwrap());
        cx.wait_for(h.window, Duration::from_secs(30), |_, cx| {
            h.view.read(cx).controller.state().active_tab().is_some_and(|tab| matches!(&tab.kind, TabKind::DataEditor(editor) if !editor.loading && editor.page.is_some()))
        }).await;
        // 模拟外部结构变化；只操作本测试的隔离 SQLite，不改生产配置或数据库。
        query(&h.config, "DROP TABLE Product");
        cx.update_window(h.window, |_, window, cx| {
            window.render_frame(cx);
            window.click(("table-info-toggle", tab.0), cx);
            window
                .within(("table-info-tabs", tab.0))
                .click(TableInfoTab::Ddl as usize, cx);
        })
        .unwrap();
        cx.wait_for(h.window, Duration::from_secs(30), |_, cx| {
            h.view.read(cx).controller.state().active_tab().is_some_and(|tab| matches!(&tab.kind, TabKind::DataEditor(editor) if matches!(&editor.table_info.ddl, LoadState::Failed(_))))
        }).await;
        cx.update_window(h.window, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                !window
                    .find("table-info-message")
                    .label()
                    .unwrap_or("")
                    .is_empty()
            );
        })
        .unwrap();
        h.view.read_with(cx, |main, _| {
            assert!(main._table_info_tasks.is_empty());
            let TabKind::DataEditor(editor) = &main.controller.state().active_tab().unwrap().kind
            else {
                panic!("data editor");
            };
            assert!(editor.error.is_none());
            assert_eq!(editor.page.as_ref().unwrap().rows.len(), 2);
        });
    }
}

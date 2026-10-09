mod dangerous_confirmation_tests {
    use super::*;
    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
    use gpui_kit::{TestAppContext, point, size};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Harness {
        window: gpui::AnyWindowHandle,
        view: Entity<NavicatMain>,
        root: PathBuf,
        tab_id: TabId,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn open_main(cx: &mut TestAppContext, kind: DangerousCommandKind, theme: ThemeMode) -> Harness {
        cx.executor().allow_parking();
        let root = std::env::temp_dir().join(format!(
            "fluxdb-dangerous-confirmation-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let storage = FileStorage::new(root.clone());
        let mut controller = AppController::with_mock_data();
        let mut settings = controller.state().settings.clone();
        settings.confirm_dangerous_sql = true;
        settings.confirm_dangerous_redis = true;
        controller.dispatch(AppCommand::SaveSettings(settings));
        let (event, text) = match kind {
            DangerousCommandKind::Sql => (
                controller.dispatch(AppCommand::OpenQueryEditor(ConnectionId(1))),
                "DELETE FROM Product",
            ),
            DangerousCommandKind::Redis => {
                // 确认的错误分支只连接文档保留网段，绝不向真实服务发送 FLUSHDB。
                let mut config = controller.state().connections[0].config.clone();
                config.kind = DatabaseKind::Redis;
                config.endpoint = Endpoint::Tcp {
                    host: "192.0.2.1".into(),
                    port: 6379,
                    database: None,
                };
                controller.dispatch(AppCommand::ReplaceConnections(vec![config]));
                (
                    controller.dispatch(AppCommand::OpenRedisWorkbench {
                        connection_id: ConnectionId(1),
                        database: 0,
                    }),
                    "FLUSHDB",
                )
            }
        };
        let AppEvent::TabOpened(tab_id) = event else {
            panic!("fixture tab did not open: {event:?}");
        };
        controller.dispatch(match kind {
            DangerousCommandKind::Sql => AppCommand::UpdateQueryText {
                tab_id,
                text: text.into(),
            },
            DangerousCommandKind::Redis => AppCommand::UpdateRedisWorkbenchText {
                tab_id,
                text: text.into(),
            },
        });
        cx.update(gpui_kit::init);
        cx.update(|cx| cx.bind_keys([KeyBinding::new("escape", CancelDialog, None)]));
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::default(),
                        size: size(px(1200.), px(800.)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    cx.new(|cx| {
                        let mut main = navicat_main_new(window, cx, controller, storage, theme);
                        main.ai_chat.recovery_pending = false;
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
            tab_id,
        }
    }

    fn assert_not_started(harness: &Harness, cx: &TestAppContext) {
        harness.view.read_with(cx, |main, _| {
            assert!(main._query_execute_tasks.is_empty());
            assert!(main._redis_workbench_tasks.is_empty());
            assert!(main.controller.state().query_history.is_empty());
            assert!(main.controller.state().redis_workbench_history.is_empty());
        });
    }

    #[gpui_kit::test]
    fn dangerous_confirmation_cancel_paths_preserve_pending_execution(cx: &mut TestAppContext) {
        for theme in [ThemeMode::Light, ThemeMode::Dark] {
            for kind in [DangerousCommandKind::Sql, DangerousCommandKind::Redis] {
                let harness = open_main(cx, kind, theme);
                let (context, cancel_id) = match kind {
                    DangerousCommandKind::Sql => ("DangerousQueryModal", "dangerous-query-cancel"),
                    DangerousCommandKind::Redis => (
                        "DangerousRedisCommandModal",
                        "dangerous-redis-command-cancel",
                    ),
                };
                for close_path in 0..4 {
                    cx.update_window(harness.window, |_, window, cx| {
                        window.render_frame(cx);
                        window.click("运行", cx);
                    })
                    .unwrap();
                    cx.update_window(harness.window, |_, window, cx| {
                        window.render_frame(cx);
                        assert!(window.try_find(cancel_id).is_some());
                        // 点击面板内非交互位置，不能穿透遮罩而取消。
                        window.click_at((context, 0u64), point(px(8.), px(8.)), cx);
                    })
                    .unwrap();
                    assert_not_started(&harness, cx);
                    harness.view.read_with(cx, |main, _| match kind {
                        DangerousCommandKind::Sql => {
                            assert!(main.pending_dangerous_query.is_some())
                        }
                        DangerousCommandKind::Redis => {
                            assert!(main.pending_dangerous_redis_command.is_some())
                        }
                    });
                    cx.update_window(harness.window, |_, window, cx| {
                        window.render_frame(cx);
                        assert_eq!(window.find(cancel_id).label(), Some("取消"));
                        match close_path {
                            0 => window.click(cancel_id, cx),
                            1 => window.click((context, 1u64), cx),
                            2 => window.press("escape", cx),
                            _ => window.click_at(context, point(px(8.), px(8.)), cx),
                        }
                    })
                    .unwrap();
                    cx.update_window(harness.window, |_, window, cx| {
                        window.render_frame(cx);
                        assert!(window.try_find(cancel_id).is_none());
                    })
                    .unwrap();
                    harness.view.read_with(cx, |main, _| {
                        assert!(main.pending_dangerous_query.is_none());
                        assert!(main.pending_dangerous_redis_command.is_none());
                    });
                    assert_not_started(&harness, cx);
                }
                cx.update_window(harness.window, |_, window, _| window.remove_window())
                    .unwrap();
            }
        }
    }

    #[gpui_kit::test]
    async fn dangerous_confirmation_executes_sql_only_after_confirm(cx: &mut TestAppContext) {
        let harness = open_main(cx, DangerousCommandKind::Sql, ThemeMode::Light);
        cx.update_window(harness.window, |_, window, cx| {
            window.render_frame(cx);
            window.click("运行", cx);
        })
        .unwrap();
        assert_not_started(&harness, cx);
        cx.update_window(harness.window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("dangerous-query-confirm").is_some());
            assert_eq!(window.find("dangerous-query-confirm").label(), Some("执行"));
            window.click("dangerous-query-confirm", cx);
            assert!(window.try_find("执行中").is_some());
            assert_eq!(harness.view.read(cx)._query_execute_tasks.len(), 1);
            window.click("执行中", cx);
            assert_eq!(harness.view.read(cx)._query_execute_tasks.len(), 1);
        })
        .unwrap();
        harness.view.read_with(cx, |main, _| {
            assert!(main.pending_dangerous_query.is_none());
        });
        cx.wait_for(harness.window, Duration::from_secs(30), |_, cx| {
            harness.view.read(cx).controller.state().tabs.iter().any(|tab| {
                tab.id == harness.tab_id && matches!(&tab.kind, TabKind::QueryEditor(editor) if !editor.running && !editor.summaries.is_empty())
            })
        }).await;
        cx.update_window(harness.window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("dangerous-query-confirm").is_none());
            assert!(window.try_find("运行").is_some());
        })
        .unwrap();
        harness.view.read_with(cx, |main, _| {
            let tab = main
                .controller
                .state()
                .tabs
                .iter()
                .find(|tab| tab.id == harness.tab_id)
                .unwrap();
            let TabKind::QueryEditor(editor) = &tab.kind else {
                panic!("query tab changed");
            };
            assert!(editor.error.is_none());
            assert!(editor.summaries.iter().all(|summary| summary.success));
            assert_eq!(main.controller.state().query_history.len(), 1);
        });
    }

    #[gpui_kit::test]
    async fn dangerous_confirmation_reports_redis_failure_after_confirm(cx: &mut TestAppContext) {
        let harness = open_main(cx, DangerousCommandKind::Redis, ThemeMode::Dark);
        cx.update_window(harness.window, |_, window, cx| {
            window.render_frame(cx);
            window.click("运行", cx);
        })
        .unwrap();
        assert_not_started(&harness, cx);
        cx.update_window(harness.window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("dangerous-redis-command-confirm").is_some());
            assert_eq!(
                window.find("dangerous-redis-command-confirm").label(),
                Some("执行")
            );
            window.click("dangerous-redis-command-confirm", cx);
            assert!(window.try_find("执行中").is_some());
            assert_eq!(harness.view.read(cx)._redis_workbench_tasks.len(), 1);
            window.click("执行中", cx);
            assert_eq!(harness.view.read(cx)._redis_workbench_tasks.len(), 1);
        })
        .unwrap();
        harness.view.read_with(cx, |main, _| {
            assert!(main.pending_dangerous_redis_command.is_none());
        });
        cx.wait_for(harness.window, Duration::from_secs(30), |_, cx| {
            harness.view.read(cx).controller.state().tabs.iter().any(|tab| {
                tab.id == harness.tab_id && matches!(&tab.kind, TabKind::RedisWorkbench(workbench) if !workbench.running && workbench.error.is_some())
            })
        }).await;
        cx.update_window(harness.window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("dangerous-redis-command-confirm").is_none());
            assert!(
                window
                    .try_find(("retry-redis-workbench", harness.tab_id.0))
                    .is_some()
            );
        })
        .unwrap();
        harness.view.read_with(cx, |main, _| {
            assert!(main._redis_workbench_tasks.is_empty());
            assert!(main.controller.state().last_error.is_some());
        });
    }

    #[gpui_kit::test]
    fn redis_resize_still_persists_on_release_outside_handle(cx: &mut TestAppContext) {
        let harness = open_main(cx, DangerousCommandKind::Redis, ThemeMode::Light);
        let before = harness.view.read_with(cx, |main, _| {
            main.controller
                .state()
                .settings
                .redis_workbench_editor_ratio
        });
        cx.update_window(harness.window, |_, window, cx| {
            window.render_frame(cx);
            let from = window
                .find("redis-workbench-split-handle")
                .bounds()
                .center();
            window.drag(from, from + point(px(0.), px(48.)), cx);
        })
        .unwrap();
        harness.view.read_with(cx, |main, _| {
            let ratio = main
                .controller
                .state()
                .settings
                .redis_workbench_editor_ratio;
            assert!(ratio > before);
            assert!(main.redis_workbench_panel_resize_start.is_none());
            assert_eq!(
                main.storage
                    .load_settings()
                    .unwrap()
                    .redis_workbench_editor_ratio,
                ratio
            );
        });
    }
}

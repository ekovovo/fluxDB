// 「新建连接」生产弹框的 GPUI Headless UI 自动化测试。
//
// 覆盖目标：用真实生产 View（`NavicatMain` + 生产 `new_connection_modal`）与真实事件链
// 验证「打开弹框 → 选数据库类型 → 逐字段输入 → 点击测试连接 → UI 反馈 → 业务结果」。
//
// 关键约束：
// 1. 走生产装配路径 `navicat_main_new`，不构造与生产重复的表单 View；
// 2. 交互一律通过 `gpui_kit::test` 的真实鼠标/键盘事件（click / input / press），
//    不直接调用 `show_new_connection` / `test_new_connection` 等处理函数；
// 3. 控件按稳定 `ElementId` 定位（`connection-name`、`new-connection-kind-postgres` 等），
//    不使用绝对坐标或列表下标；
// 4. 隔离性：`FileStorage` 指向进程专属临时目录，不读写真实用户配置；
//    测试连接指向文档保留网段与假凭据，**不会**触达任何真实数据库。
#[cfg(test)]
mod connection_ui_tests {
    use super::*;
    use gpui_kit::base::test_support::registered_paths;
    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
    use gpui_kit::{TestAppContext, px, size};
    use std::time::Duration;

    /// 测试连接目标：RFC 5737 文档保留网段 TEST-NET-1。本机不会提供该服务，
    /// 因此失败是确定性的「连不上」，既不触达真实数据库也不依赖网络状态。
    const UNREACHABLE_HOST: &str = "192.0.2.1";
    const UNREACHABLE_PORT: &str = "5432";
    /// 假凭据：仅用于断言「输入框确实收到 UI 输入」，不是任何真实密钥。
    const FAKE_PASSWORD: &str = "ui-test-not-a-real-secret";
    /// 生产代码在测试进行中显示的固定文案（`connection_forms.rs` 的 Pending 分支）。
    const PENDING_TEXT: &str = "正在测试连接";
    /// 空名称时的就地校验文案（`validate_new_connection`）。
    const VALIDATION_NAME_REQUIRED: &str = "请填写连接名称";

    /// 首页「新建连接」入口的稳定 ID：用于通过真实点击打开生产弹框。
    /// 首页入口与侧边栏入口都走同一个生产处理函数 `show_new_connection`。
    const ADD_CONNECTION_BUTTON: &str = "home-new-connection";
    const MODAL: &str = "new-connection-modal";
    const KIND_POSTGRES: &str = "new-connection-kind-postgres";
    const TEST_BUTTON: &str = "new-connection-test";
    const STATUS: &str = "new-connection-test-status";

    struct Harness {
        window: gpui::AnyWindowHandle,
        view: Entity<NavicatMain>,
        root: std::path::PathBuf,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            // 清理隔离目录，不触碰真实用户配置。
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// 生产路径构造主视图：与 `main()` 启动一致地初始化组件库并安装 `Root`。
    fn open_main(cx: &mut TestAppContext, label: &str) -> Harness {
        let root = std::env::temp_dir().join(format!(
            "fluxdb-connection-ui-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("创建隔离测试目录");
        let storage = FileStorage::new(root.clone());
        let controller = AppController::new();
        // `gpui_kit::init` 即 `gpui_component::init`：把 `WindowState` 注册为 `Root` 插件，
        // 弹框层才会挂载。顺序与生产启动一致。
        cx.update(gpui_kit::init);
        let (window, view) = cx
            .update(|cx| {
                gpui_kit::open_window(
                    gpui::WindowOptions {
                        window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds {
                            origin: gpui::Point::default(),
                            size: size(px(1600.), px(1000.)),
                        })),
                        ..Default::default()
                    },
                    cx,
                    |window, cx| {
                        cx.new(|cx| {
                            navicat_main_new(window, cx, controller, storage, ThemeMode::Light)
                        })
                    },
                )
                .expect("打开测试窗口")
            });
        Harness { window, view, root }
    }

    /// 通过真实点击首页「新建连接」入口打开生产弹框，并确认弹框已进入渲染。
    fn open_new_connection_modal(harness: &mut Harness, cx: &mut TestAppContext) {
        cx.update_window(harness.window.clone(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find(ADD_CONNECTION_BUTTON).is_some(),
                "首页应存在「新建连接」入口；已注册: {}",
                registered_paths(window)
            );
            window.click(ADD_CONNECTION_BUTTON, cx);
        })
        .expect("打开新建连接弹框");

        cx.update_window(harness.window.clone(), |_, window, cx| {
            window.render_frame(cx);
            // 弹框遮罩挂在 Root 层而非 main-workspace 下。用弹框内部控件的存在性确认
            // 「模态已打开」，并用 within 确认遮罩确实构成可定位的作用域。
            assert!(
                window.try_find(TEST_BUTTON).is_some(),
                "点击后应出现新建连接弹框（含「测试」按钮）；已注册: {}",
                registered_paths(window)
            );
            assert!(
                window.within(MODAL).try_find(TEST_BUTTON).is_some(),
                "「测试」按钮应位于新建连接弹框遮罩作用域内"
            );
        })
        .expect("确认新建连接弹框");
    }

    /// 点击输入框聚焦、全选既有内容后替换为 `text`，返回该输入框当前的可访问值。
    ///
    /// 交互后重新查询快照：上一次 `find` 的结果不会随输入更新。
    /// `field` 需为 `'static` 字符串：GPUI 的 `ElementId` 由点击目标持有。
    /// 生产表单会预填名称与端口等默认值，因此必须先全选覆盖，直接 `input()`
    /// 会把文本插到光标位置而不是替换。
    fn replace_field(
        window: &mut gpui::Window,
        field: &'static str,
        text: &str,
        cx: &mut gpui::App,
    ) -> Option<String> {
        // `input()` 不自动聚焦，必须先真实点击让焦点落到目标输入框。
        window.click(field, cx);
        // `secondary` 在 macOS 是 cmd、其他平台是 ctrl，即「全选」的跨平台写法。
        window.press("secondary-a", cx);
        window.input(text, cx);
        window.find(field).value().map(str::to_string)
    }

    /// 完整输入一份 PostgreSQL 连接表单，全部通过真实鼠标与键盘事件。
    fn fill_postgres_form(harness: &mut Harness, cx: &mut TestAppContext) {
        cx.update_window(harness.window.clone(), |_, window, cx| {
            window.render_frame(cx);
            // 先切到 PostgreSQL：默认是 MySQL，选中态本身也是要断言的 UI 反馈。
            window.click(KIND_POSTGRES, cx);
        })
        .expect("切换数据库类型");

        cx.update_window(harness.window.clone(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find(KIND_POSTGRES).selected(),
                Some(true),
                "点击后 PostgreSQL 磁贴应处于选中态"
            );

            assert_eq!(
                replace_field(window, "connection-name", "UI 测试 PostgreSQL", cx).as_deref(),
                Some("UI 测试 PostgreSQL"),
                "连接名称应被 UI 输入整体替换（生产表单预填了默认名）"
            );
            assert_eq!(
                replace_field(window, "connection-host", UNREACHABLE_HOST, cx).as_deref(),
                Some(UNREACHABLE_HOST),
                "主机应收到 UI 输入"
            );
            assert_eq!(
                replace_field(window, "connection-port", UNREACHABLE_PORT, cx).as_deref(),
                Some(UNREACHABLE_PORT),
                "端口应被 UI 输入整体替换（生产表单预填了默认端口）"
            );
            assert_eq!(
                replace_field(window, "connection-username", "test_user", cx).as_deref(),
                Some("test_user"),
                "用户名应收到 UI 输入"
            );
            // 密码框掩码渲染，其值不应出现在无障碍树里；只断言点击后确实获得焦点。
            window.click("connection-password", cx);
            window.input(FAKE_PASSWORD, cx);
            assert_eq!(
                window.find("connection-password").focused(),
                Some(true),
                "密码框点击后应获得焦点"
            );
        })
        .expect("填写 PostgreSQL 表单");
    }

    /// 主流程：真实点击 → 真实输入 → 真实点击测试连接 → 断言 UI 反馈与业务结果。
    #[gpui_kit::test]
    async fn fills_and_tests_a_postgres_connection_through_the_ui(cx: &mut TestAppContext) {
        let mut harness = open_main(cx, "test-connection");
        open_new_connection_modal(&mut harness, cx);
        fill_postgres_form(&mut harness, cx);

        // 点击「测试」触发真实事件链（校验 → AppCommand::TestConnection → Connector → 回包）。
        cx.update_window(harness.window.clone(), |_, window, cx| {
            window.render_frame(cx);
            window.click(TEST_BUTTON, cx);
            window.render_frame(cx);
        })
        .expect("点击测试连接");

        // 「测试进行中」的禁用态无法用 `disabled()` 断言：GPUI 的 StatefulInteractiveElement
        // 没有 aria-disabled setter，基座 Button 也不上报该属性（kit 自身测试同样
        // 断言 disabled() == None）。因此改为验证行为结果：任务已在跑，且重复点击
        // 不会叠加第二个任务。
        cx.update(|cx| {
            let view = harness.view.read(cx);
            assert!(
                view._test_connection_task.is_some(),
                "点击「测试」后应已启动后台连接测试任务"
            );
            assert!(
                matches!(
                    view.new_connection_form.test_status,
                    Some(ConnectionTestStatus::Pending(_))
                ),
                "测试进行中应显示进行中状态"
            );
        });

        cx.update_window(harness.window.clone(), |_, window, cx| {
            window.render_frame(cx);
            // 进行中重复点击：生产代码有 in-flight 守卫，不应叠加任务。
            window.click(TEST_BUTTON, cx);
        })
        .expect("重复点击测试连接");

        cx.update(|cx| {
            let view = harness.view.read(cx);
            assert!(
                view._test_connection_task.is_some(),
                "进行中重复点击不应破坏或叠加测试任务"
            );
        });

        // 离开 update_window 后等待异步结果：受控条件 + 有界超时，不用固定 sleep。
        // 等待条件是「状态离开进行中」，即后台测试任务真正回包，而不是等待执行器空闲。
        cx.wait_for(harness.window.clone(), Duration::from_secs(30), |window, _| {
            window.try_find(STATUS).is_some_and(|status| {
                status
                    .label()
                    .is_some_and(|text| !text.is_empty() && !text.contains(PENDING_TEXT))
            })
        })
        .await;

        cx.update_window(harness.window.clone(), |_, window, cx| {
            window.render_frame(cx);
            let status = window.find(STATUS);
            let text = status.label().unwrap_or_default().to_string();
            assert_eq!(status.role(), Some(gpui::Role::Status));
            assert!(
                !text.contains(FAKE_PASSWORD),
                "失败反馈不得回显密码，实际: {text}"
            );
            assert!(
                !text.contains(PENDING_TEXT),
                "等待结束后应离开进行中状态（成功或失败终态），实际: {text}"
            );
        })
        .expect("断言测试连接结果");

        // 业务结果：输入值经由 UI 事件写入生产表单状态（未保存，不产生连接记录）。
        cx.update(|cx| {
            let view = harness.view.read(cx);
            assert_eq!(view.new_connection_form.name, "UI 测试 PostgreSQL");
            assert_eq!(view.new_connection_form.host, UNREACHABLE_HOST);
            assert_eq!(view.new_connection_form.port, UNREACHABLE_PORT);
            assert_eq!(view.new_connection_form.username, "test_user");
            assert!(
                matches!(view.new_connection_form.test_status, Some(_)),
                "测试结束后表单应保留测试状态"
            );
            assert!(
                !view
                    .controller
                    .state()
                    .connections
                    .iter()
                    .any(|connection| connection.config.name == "UI 测试 PostgreSQL"),
                "仅点击测试不应创建连接记录"
            );
        });
    }

    /// 校验失败分支：留空名称时点击「测试」，UI 就地给出错误，不发起连接命令。
    #[gpui_kit::test]
    fn shows_validation_error_when_the_name_is_empty(cx: &mut TestAppContext) {
        let mut harness = open_main(cx, "validation");
        open_new_connection_modal(&mut harness, cx);

        // 清空名称。InputState 的 Change 订阅是事件驱动的，表单状态在输入事件
        // 处理链走完后才更新，因此清空与断言分成两段 update_window。
        cx.update_window(harness.window.clone(), |_, window, cx| {
            window.render_frame(cx);
            // 生产表单预填了默认连接名，需先全选删除才能触发「名称必填」校验。
            window.click("connection-name", cx);
            window.press("secondary-a", cx);
            window.press("delete", cx);
            assert_eq!(
                window.find("connection-name").value(),
                Some(""),
                "清空名称后输入框应为空"
            );
        })
        .expect("清空连接名称");

        cx.update(|cx| {
            assert_eq!(
                harness.view.read(cx).new_connection_form.name,
                "",
                "清空输入后生产表单状态应同步为空"
            );
        });

        cx.update_window(harness.window.clone(), |_, window, cx| {
            window.render_frame(cx);
            window.click(TEST_BUTTON, cx);
            window.render_frame(cx);
            let status = window.find(STATUS);
            assert_eq!(
                status.label(),
                Some(VALIDATION_NAME_REQUIRED),
                "空名称应给出就地校验提示"
            );
        })
        .expect("断言校验反馈");

        cx.update(|cx| {
            let view = harness.view.read(cx);
            assert!(
                matches!(
                    view.new_connection_form.test_status,
                    Some(ConnectionTestStatus::Error(_))
                ),
                "表单应记录校验错误状态"
            );
            // 校验失败不应触发后台测试任务。
            assert!(
                view._test_connection_task.is_none(),
                "校验失败不应启动连接测试任务"
            );
        });
    }
}

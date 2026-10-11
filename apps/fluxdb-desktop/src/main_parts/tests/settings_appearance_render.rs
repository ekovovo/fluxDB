// 设置面板「外观」页渲染回归：该页曾在 debug 构建下必崩（hover style already set）。
// 复用 ai_agent_defaults 的隔离 fixture 与 open 辅助函数。
#[cfg(test)]
mod settings_appearance_render_tests {
    use super::ai_agent_defaults_tests::{Fixture, open};
    use super::*;
    use gpui::TestAppContext;

    /// 渲染「外观」页不得 panic。
    ///
    /// 缺陷：`Button::render` 内部已对同一个 base 调过 `.hover(...)`，而主题色块又用
    /// `Styled::hover` 叠加了一次，触发 gpui 的 `debug_assert!("hover style already
    /// set")`；该断言发生在元素构建期，且处于不能 unwind 的绘制帧里，会直接 abort
    /// 整个进程（用户点击「设置 → 外观」即崩溃）。修法是只保留组件自身的 hover 行为。
    ///
    /// 这个用例的价值在于：断言触发是进程级 abort，页面一旦真去构建主题色块就无法
    /// "悄悄失败"；修复前它必然失败，修复后通过。
    #[gpui::test]
    fn settings_appearance_page_renders_without_panic(cx: &mut TestAppContext) {
        let fixture = Fixture::new("settings-appearance-render");
        let controller = fixture.controller();
        let (view, cx) = open(cx, &fixture, controller, SettingsPanelSection::Appearance);

        // 多画几帧：hover 断言在元素构建期触发，首帧即会命中。
        for _ in 0..3 {
            cx.update(|window, cx| {
                let _ = window.draw(cx).clear(cx);
            });
        }
        assert!(
            view.read_with(cx, |main, _| main.settings_panel_section
                == SettingsPanelSection::Appearance),
            "应停留在外观页"
        );
    }
}

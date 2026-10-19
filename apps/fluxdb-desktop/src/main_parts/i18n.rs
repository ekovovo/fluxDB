rust_i18n::i18n!(
    "locales_stub",
    fallback = "en",
    backend = fluxdb_i18n::backend()
);

/// Translate a stable application key. UI code should keep literal keys here so
/// missing translations are detectable by static searches and validation.
fn tr(key: &'static str) -> SharedString {
    rust_i18n::t!(key).to_string().into()
}

fn init_i18n(
    bundled_root: &Path,
    user_root: &Path,
    saved_locale: &str,
) -> fluxdb_i18n::Bootstrap {
    let bootstrap = fluxdb_i18n::initialize(bundled_root, user_root, saved_locale);
    for issue in &bootstrap.issues {
        tracing::warn!(
            target: "fluxdb_desktop",
            source = ?issue.source,
            kind = ?issue.kind,
            path = %issue.path.display(),
            message = %issue.message,
            "忽略有问题的语言包内容"
        );
    }
    tracing::info!(
        target: "fluxdb_desktop",
        locale = %bootstrap.active_locale,
        count = bootstrap.locales.len(),
        "界面语言包已加载"
    );
    bootstrap
}

/// GPUI Component uses the same rust-i18n locale but owns a separate backend.
/// Registering once lets component-provided strings follow the application
/// pack, while missing component keys continue to use the upstream locales.
fn extend_component_i18n() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| rust_i18n::extend!(gpui_component));
}

/// Apply a resolved locale to the application and native surfaces whose labels
/// are cached outside GPUI's reactive graph.
fn apply_ui_locale(locale: &str, cx: &mut App) {
    let changed = !fluxdb_i18n::active_locale().eq_ignore_ascii_case(locale);
    fluxdb_i18n::set_locale(locale);
    if !changed {
        return;
    }
    set_app_menus(cx);
    #[cfg(not(target_os = "linux"))]
    refresh_tray_menu();
    cx.refresh_windows();
}

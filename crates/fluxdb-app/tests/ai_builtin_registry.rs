use std::collections::BTreeSet;

use fluxdb_app::{BuiltinCapability, BuiltinRegistry};
use fluxdb_storage::ai_platform::plugins::PluginInstanceView;

fn instance(id: &str, package_key: &str, runtime_kind: &str, enabled: bool) -> PluginInstanceView {
    PluginInstanceView {
        id: id.into(),
        display_name: id.into(),
        package_id: format!("pkg-{id}"),
        package_key: package_key.into(),
        runtime_kind: runtime_kind.into(),
        enabled,
        revision: 1,
    }
}

#[test]
fn registry_contains_all_builtin_capability_entries() {
    let registry = BuiltinRegistry::new().unwrap();
    assert_eq!(registry.descriptors().len(), 10);
    assert_eq!(
        registry
            .descriptors()
            .iter()
            .map(|descriptor| descriptor.package_key)
            .collect::<BTreeSet<_>>()
            .len(),
        10
    );
    assert_eq!(
        registry
            .find_by_package("fluxdb.decision.jev")
            .unwrap()
            .instance_id,
        "builtin.jev"
    );
    assert!(
        registry
            .find_by_package("fluxdb.security.audit")
            .unwrap()
            .mandatory
    );
    assert!(registry.find_by_package("fluxdb.tools.database").is_none());
    let storage_keys = fluxdb_storage::ai_platform::plugins::BUILTIN_PLUGINS
        .iter()
        .map(|(_, _, package_key, _)| *package_key)
        .collect::<BTreeSet<_>>();
    let app_keys = registry
        .descriptors()
        .iter()
        .map(|descriptor| descriptor.package_key)
        .collect::<BTreeSet<_>>();
    assert_eq!(app_keys, storage_keys);
    let registry_mandatory = registry
        .descriptors()
        .iter()
        .filter(|descriptor| descriptor.mandatory)
        .map(|descriptor| descriptor.instance_id)
        .collect::<BTreeSet<_>>();
    let storage_mandatory = fluxdb_storage::ai_platform::plugins::MANDATORY_INSTANCE_IDS
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    assert_eq!(registry_mandatory, storage_mandatory);
}

#[test]
fn enabled_capabilities_ignore_disabled_and_unknown_runtime_entries() {
    let registry = BuiltinRegistry::default();
    let capabilities = registry.enabled_capabilities(&[
        instance("ui", "fluxdb.context.ui", "builtin", true),
        instance("audit", "fluxdb.security.audit", "builtin", false),
        instance("unknown", "fluxdb.unknown", "builtin", true),
        instance("external", "fluxdb.context.ui", "wasm", true),
    ]);
    assert!(capabilities.contains(&BuiltinCapability::ContextProvider));
    assert!(!capabilities.contains(&BuiltinCapability::SecurityAudit));
}

#[test]
fn mandatory_capabilities_cannot_be_disabled() {
    let registry = BuiltinRegistry::default();
    assert!(!registry.can_disable("builtin.context-ui"));
    assert!(!registry.can_disable("builtin.security-audit"));
    assert!(registry.can_disable("builtin.jev"));
    assert!(!registry.can_disable("unknown"));
}

/// 用量统计看板尚未实现：只占目录位，不登记能力、不可启停，也不显示成内核约束。
#[test]
fn usage_stats_entry_is_a_placeholder_until_the_dashboard_exists() {
    let registry = BuiltinRegistry::default();
    let descriptor = registry
        .find_by_instance("builtin.budget")
        .expect("占位实例仍保留目录行");
    assert_eq!(descriptor.display_name, "AI 用量统计");
    assert!(descriptor.retired);
    assert!(!descriptor.mandatory);
    assert!(descriptor.capabilities.is_empty());
    assert!(registry.is_retired("builtin.budget"));
    assert!(!registry.can_disable("builtin.budget"));
    assert!(
        !registry
            .enabled_capabilities(&[instance("budget", "fluxdb.usage.budget", "builtin", true)])
            .contains(&BuiltinCapability::SecurityAudit)
    );
}

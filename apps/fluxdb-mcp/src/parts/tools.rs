// MCP 工具到内部 stable ToolId 的映射与类型化入参（设计 §20.5、§28.5.1，T058）。
//
// 协议层只做「名称解析 + schema 确认 + 调用服务」，不拼 SQL、不读密码、不绕过 app 直接调用驱动。
// 工具的可见性由 `fluxdb_app::visible_tools` 决定；这里只提供名称集合与入参解析。
/// 一次 `tools/call` 的入参：MCP 用自由对象承载各工具的参数。
///
/// 严格解析由内部 `ToolCatalog::prepare` 依各工具 schema 完成；这里只保留原始 JSON 文本。
pub fn arguments_to_json(arguments: Option<&serde_json::Map<String, serde_json::Value>>) -> String {
    match arguments {
        Some(map) => serde_json::Value::Object(map.clone()).to_string(),
        None => "{}".to_string(),
    }
}

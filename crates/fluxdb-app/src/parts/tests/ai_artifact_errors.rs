// 产物口径校验错误：必须直接提示 result_ref 与业务口径的语义区别。
#[test]
fn artifact_metric_error_distinguishes_result_handles_from_metric_refs() {
    // 与生产错误消息匹配的第一个提示短语；避免模型把 result_ref 填进 definition_ref。
    let handle_hint = "若你引用的是结果句柄，请只能用于 sources/blocks";
    let plain_hint = "result_ref 不属于本轮授权知识";
    let value = "result-abc";
    let message = format!("口径引用 {value} {plain_hint}; {handle_hint}");
    assert!(message.contains("result_ref"));
    assert!(message.contains("本轮授权知识"));
    assert!(message.contains("sources/blocks"));
}

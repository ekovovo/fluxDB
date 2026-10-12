// ---- 产物声明（设计 §27、§42.7，T039）----
//
// 产物是**声明式**的：只描述类型/标题/renderer/口径/来源/块结构，块通过结果句柄
// 与字段名引用已经取得的结果，绝不携带模型自己填的数字。因此这里的校验只覆盖与
// 数据无关的形状；字段是否存在、是否数值、来源是否属于本任务由应用层用真实结果
// 句柄核对。

/// 产物声明格式版本（写入 `ai_artifacts.schema_version`）。
pub const ARTIFACT_SCHEMA_VERSION: u32 = 1;
/// 标题字符上限。
pub const ARTIFACT_MAX_TITLE_CHARS: usize = 200;
/// 单产物块数上限。
pub const ARTIFACT_MAX_BLOCKS: usize = 64;
/// 表格列数上限。
pub const ARTIFACT_MAX_COLUMNS: usize = 64;
/// 说明块正文长度上限。
pub const ARTIFACT_MAX_NOTE_CHARS: usize = 2000;
/// 来源条数上限。
pub const ARTIFACT_MAX_SOURCES: usize = 32;
/// renderer ID 长度上限。
pub const ARTIFACT_MAX_RENDERER_CHARS: usize = 128;
/// 字段名长度上限。
pub const ARTIFACT_MAX_FIELD_CHARS: usize = 200;
/// 时区标识长度上限。
pub const ARTIFACT_MAX_TIMEZONE_CHARS: usize = 64;
/// 单图表数据点上限；超出的行由 renderer 截断并标记 `truncated`。
pub const ARTIFACT_MAX_POINTS: usize = 500;
/// 来源及口径引用 ID 字符上限。
pub const ARTIFACT_MAX_REF_CHARS: usize = 256;
/// 过滤条件条数上限。
pub const ARTIFACT_MAX_FILTERS: usize = 32;
/// 单条过滤条件字符上限。
pub const ARTIFACT_MAX_FILTER_CHARS: usize = 2000;
/// 模板版本字符上限。
pub const ARTIFACT_MAX_TEMPLATE_VERSION_CHARS: usize = 64;
/// 可访问性描述字符上限。
pub const ARTIFACT_MAX_ACCESSIBILITY_CHARS: usize = 2000;
/// RFC3339 时间文本字符上限。
pub const ARTIFACT_MAX_TIMESTAMP_CHARS: usize = 64;

/// 静态快照不会在打开产物时重新查询；本期不支持自动刷新。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRefresh {
    #[default]
    Snapshot,
}

/// 带明确偏移的 RFC3339 起止时间；校验按实际时间而非字符串顺序比较。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactTimeRange {
    pub start: String,
    pub end: String,
}

/// 产物类型。
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Report,
    Chart,
    Table,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::Report => "report",
            ArtifactKind::Chart => "chart",
            ArtifactKind::Table => "table",
        }
    }

    /// 从持久化字符串解析；未知值报错而不是猜测。
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "report" => Ok(ArtifactKind::Report),
            "chart" => Ok(ArtifactKind::Chart),
            "table" => Ok(ArtifactKind::Table),
            other => Err(Error::new(
                ErrorKind::Internal,
                format!("未知产物类型: {other}"),
            )),
        }
    }
}

/// 图表种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactChart {
    Line,
    Bar,
}

/// 一个产物块：图表/表格引用已存在结果的字段；说明只写范围与限制，不填未查询的数字。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArtifactBlock {
    /// 图表：`x`/`y` 是来源结果里的列名，不是数据值。
    Chart {
        result_ref: String,
        chart: ArtifactChart,
        x: String,
        y: String,
    },
    /// 表格：列出来源结果里要展示的列。
    Table {
        result_ref: String,
        columns: Vec<String>,
    },
    /// 说明文字：统计范围/限制/假设，不承载查询结果数字。
    Note { text: String },
}

/// 指标口径：名称、可选知识定义引用或未确认假设、时区。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactMetric {
    pub name: String,
    /// 指向业务知识条目的 ID（口径来源）；没有确认口径时留空并在产物里标注假设。
    pub definition_ref: Option<String>,
    /// 本期只支持 `UTC` 或 `±HH:MM` 固定偏移；IANA 名称返回不支持。
    pub timezone: String,
    /// 未确认的业务口径假设；是否需要用户确认由应用层决定。
    #[serde(default)]
    pub assumption: Option<String>,
}

impl Default for ArtifactMetric {
    fn default() -> Self {
        Self {
            name: String::new(),
            definition_ref: None,
            timezone: "UTC".into(),
            assumption: None,
        }
    }
}

/// 一个来源：结果句柄 ID 与/或产出该结果的工具调用 ID。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSourceRef {
    pub result_ref: Option<String>,
    pub tool_call_ref: Option<String>,
}

/// 产物声明（设计 §27.1）。句柄由宿主分配，模型不能凭空填写未查询的数字。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSpec {
    pub schema_version: u32,
    pub kind: ArtifactKind,
    pub title: String,
    /// renderer ID；必须由已注册的 renderer 提供，否则返回可见不支持。
    pub renderer: String,
    #[serde(default)]
    pub metric: Option<ArtifactMetric>,
    pub sources: Vec<ArtifactSourceRef>,
    #[serde(default)]
    pub blocks: Vec<ArtifactBlock>,
    #[serde(default)]
    pub time_range: Option<ArtifactTimeRange>,
    #[serde(default)]
    pub filters: Vec<String>,
    #[serde(default)]
    pub refresh: ArtifactRefresh,
    #[serde(default = "artifact_template_version")]
    pub template_version: String,
    /// 旧声明可留空；finalization 应以标题生成描述并检查可访问性语义。
    #[serde(default)]
    pub accessibility_description: String,
}

fn artifact_template_version() -> String {
    "1".into()
}

impl Default for ArtifactSpec {
    fn default() -> Self {
        Self {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            kind: ArtifactKind::Report,
            title: String::new(),
            renderer: String::new(),
            metric: None,
            sources: Vec::new(),
            blocks: Vec::new(),
            time_range: None,
            filters: Vec::new(),
            refresh: ArtifactRefresh::Snapshot,
            template_version: artifact_template_version(),
            accessibility_description: String::new(),
        }
    }
}

impl ArtifactSpec {
    /// 与数据无关的形状校验，允许空块草稿；来源作用域、字段类型及完成态语义由应用层核对。
    pub fn validate_shape(&self) -> Result<()> {
        if self.schema_version != ARTIFACT_SCHEMA_VERSION {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!(
                    "不支持的产物 schema 版本 {}（当前 {}）",
                    self.schema_version, ARTIFACT_SCHEMA_VERSION
                ),
            ));
        }
        if self.title.trim().is_empty() {
            return Err(Error::new(ErrorKind::Internal, "产物标题不能为空"));
        }
        if self.title.chars().count() > ARTIFACT_MAX_TITLE_CHARS {
            return Err(Error::new(ErrorKind::Internal, "产物标题超过长度上限"));
        }
        if self.renderer.trim().is_empty()
            || self.renderer.chars().count() > ARTIFACT_MAX_RENDERER_CHARS
        {
            return Err(Error::new(
                ErrorKind::Internal,
                "renderer ID 缺失或超过长度上限",
            ));
        }
        if self.sources.is_empty() {
            return Err(Error::new(ErrorKind::Internal, "产物必须声明至少一个来源"));
        }
        if self.sources.len() > ARTIFACT_MAX_SOURCES {
            return Err(Error::new(ErrorKind::Internal, "产物来源条数超过上限"));
        }
        let mut source_pairs = BTreeSet::new();
        let mut result_refs = BTreeSet::new();
        for source in &self.sources {
            if source.result_ref.is_none() && source.tool_call_ref.is_none() {
                return Err(Error::new(
                    ErrorKind::Internal,
                    "产物来源必须至少包含 result_ref 或 tool_call_ref",
                ));
            }
            for reference in [&source.result_ref, &source.tool_call_ref]
                .into_iter()
                .flatten()
            {
                validate_artifact_text(reference, ARTIFACT_MAX_REF_CHARS, "来源 ID")?;
            }
            // 同次工具调用可产出多个结果，但同一结果或同一来源对不可重复声明。
            if !source_pairs.insert((&source.result_ref, &source.tool_call_ref))
                || source
                    .result_ref
                    .as_ref()
                    .is_some_and(|reference| !result_refs.insert(reference))
            {
                return Err(Error::new(ErrorKind::Internal, "产物来源重复"));
            }
        }
        if self.blocks.len() > ARTIFACT_MAX_BLOCKS {
            return Err(Error::new(ErrorKind::Internal, "产物块数超过上限"));
        }
        if let Some(metric) = &self.metric {
            validate_artifact_text(&metric.name, ARTIFACT_MAX_FIELD_CHARS, "指标名称")?;
            if let Some(reference) = &metric.definition_ref {
                validate_artifact_text(reference, ARTIFACT_MAX_REF_CHARS, "口径定义 ID")?;
            }
            if let Some(assumption) = &metric.assumption {
                validate_artifact_text(assumption, ARTIFACT_MAX_NOTE_CHARS, "口径假设")?;
            }
            validate_timezone(&metric.timezone)?;
        }
        validate_artifact_text(
            &self.template_version,
            ARTIFACT_MAX_TEMPLATE_VERSION_CHARS,
            "模板版本",
        )?;
        if self.accessibility_description.chars().count() > ARTIFACT_MAX_ACCESSIBILITY_CHARS {
            return Err(Error::new(ErrorKind::Internal, "可访问性描述超过长度上限"));
        }
        if self.filters.len() > ARTIFACT_MAX_FILTERS {
            return Err(Error::new(ErrorKind::Internal, "过滤条件条数超过上限"));
        }
        for filter in &self.filters {
            validate_artifact_text(filter, ARTIFACT_MAX_FILTER_CHARS, "过滤条件")?;
        }
        if let Some(range) = &self.time_range {
            let parse = |raw: &str| {
                validate_artifact_text(raw, ARTIFACT_MAX_TIMESTAMP_CHARS, "时间范围")?;
                chrono::DateTime::parse_from_rfc3339(raw)
                    .map_err(|_| Error::new(ErrorKind::Internal, "时间范围必须使用有效 RFC3339"))
            };
            if parse(&range.start)? > parse(&range.end)? {
                return Err(Error::new(ErrorKind::Internal, "时间范围起始晚于结束"));
            }
        }
        if !self.blocks.is_empty() {
            let has_chart = self
                .blocks
                .iter()
                .any(|b| matches!(b, ArtifactBlock::Chart { .. }));
            let has_table = self
                .blocks
                .iter()
                .any(|b| matches!(b, ArtifactBlock::Table { .. }));
            let consistent = match self.kind {
                ArtifactKind::Report => true,
                ArtifactKind::Chart => has_chart && !has_table,
                ArtifactKind::Table => has_table && !has_chart,
            };
            if !consistent {
                return Err(Error::new(ErrorKind::Internal, "产物类型与块类型不一致"));
            }
        }
        for block in &self.blocks {
            match block {
                ArtifactBlock::Chart {
                    result_ref, x, y, ..
                } => {
                    validate_artifact_text(result_ref, ARTIFACT_MAX_REF_CHARS, "图表结果 ID")?;
                    validate_field(x)?;
                    validate_field(y)?;
                }
                ArtifactBlock::Table {
                    result_ref,
                    columns,
                } => {
                    validate_artifact_text(result_ref, ARTIFACT_MAX_REF_CHARS, "表格结果 ID")?;
                    if columns.is_empty() {
                        return Err(Error::new(ErrorKind::Internal, "表格块至少要有一列"));
                    }
                    if columns.len() > ARTIFACT_MAX_COLUMNS {
                        return Err(Error::new(ErrorKind::Internal, "表格块列数超过上限"));
                    }
                    let mut names = BTreeSet::new();
                    for column in columns {
                        validate_field(column)?;
                        if !names.insert(column) {
                            return Err(Error::new(ErrorKind::Internal, "表格块列名重复"));
                        }
                    }
                }
                ArtifactBlock::Note { text } => {
                    if text.trim().is_empty() {
                        return Err(Error::new(ErrorKind::Internal, "说明块正文不能为空"));
                    }
                    if text.chars().count() > ARTIFACT_MAX_NOTE_CHARS {
                        return Err(Error::new(ErrorKind::Internal, "说明块正文超过长度上限"));
                    }
                }
            }
        }
        Ok(())
    }
}

fn validate_field(field: &str) -> Result<()> {
    validate_artifact_text(field, ARTIFACT_MAX_FIELD_CHARS, "产物字段名")
}

fn validate_artifact_text(text: &str, max_chars: usize, label: &str) -> Result<()> {
    if text.trim().is_empty() || text.chars().count() > max_chars {
        return Err(Error::new(
            ErrorKind::Internal,
            format!("{label}缺失或超过长度上限"),
        ));
    }
    Ok(())
}

/// 不依赖系统时区数据库；只有 UTC 与经 chrono 核验的固定偏移可用。
fn validate_timezone(timezone: &str) -> Result<()> {
    validate_artifact_text(timezone, ARTIFACT_MAX_TIMEZONE_CHARS, "时区标识")?;
    if timezone == "UTC" {
        return Ok(());
    }
    let bytes = timezone.as_bytes();
    let fixed = bytes.len() == 6
        && matches!(bytes[0], b'+' | b'-')
        && bytes[3] == b':'
        && [1, 2, 4, 5]
            .iter()
            .all(|&index| bytes[index].is_ascii_digit());
    if !fixed || timezone.parse::<chrono::FixedOffset>().is_err() {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "时区仅支持 UTC 或有效的 ±HH:MM 固定偏移；IANA 时区尚不支持",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod artifact_tests {
    use super::*;
    use serde_json::{Value, json};

    fn legacy_json() -> Value {
        json!({
            "schema_version": 1,
            "kind": "report",
            "title": "Sales",
            "renderer": "fluxdb.bi.report",
            "metric": {"name": "revenue", "definition_ref": null, "timezone": "UTC"},
            "sources": [{"result_ref": "result-1", "tool_call_ref": "call-1"}],
            "blocks": []
        })
    }

    fn spec() -> ArtifactSpec {
        serde_json::from_value(legacy_json()).unwrap()
    }

    fn chart() -> ArtifactBlock {
        ArtifactBlock::Chart {
            result_ref: "result-1".into(),
            chart: ArtifactChart::Bar,
            x: "channel".into(),
            y: "revenue".into(),
        }
    }

    fn table() -> ArtifactBlock {
        ArtifactBlock::Table {
            result_ref: "result-1".into(),
            columns: vec!["channel".into(), "revenue".into()],
        }
    }

    #[test]
    fn legacy_json_defaults_and_round_trip() {
        let old = spec();
        old.validate_shape().unwrap();
        assert_eq!(old.time_range, None);
        assert!(old.filters.is_empty());
        assert_eq!(old.refresh, ArtifactRefresh::Snapshot);
        assert_eq!(old.template_version, "1");
        assert!(old.accessibility_description.is_empty());
        assert_eq!(old.metric.as_ref().unwrap().assumption, None);
        assert_eq!(
            serde_json::from_value::<ArtifactSpec>(serde_json::to_value(&old).unwrap()).unwrap(),
            old
        );
        let defaults = ArtifactSpec::default();
        assert_eq!(defaults.schema_version, ARTIFACT_SCHEMA_VERSION);
        assert_eq!(defaults.template_version, "1");
        assert!(defaults.validate_shape().is_err());
        assert_eq!(ArtifactMetric::default().timezone, "UTC");
    }

    #[test]
    fn schema_version_is_required_and_not_normalized() {
        let mut raw = legacy_json();
        raw.as_object_mut().unwrap().remove("schema_version");
        assert!(serde_json::from_value::<ArtifactSpec>(raw).is_err());
        for version in [0, 2, u32::MAX] {
            let mut raw = legacy_json();
            raw["schema_version"] = json!(version);
            let parsed: ArtifactSpec = serde_json::from_value(raw).unwrap();
            assert_eq!(parsed.schema_version, version);
            assert_eq!(
                parsed.validate_shape().unwrap_err().kind,
                ErrorKind::Unsupported
            );
        }
    }

    #[test]
    fn raw_data_and_unknown_fields_are_rejected_at_every_level() {
        for raw in [
            {
                let mut raw = legacy_json();
                raw["data"] = json!([100]);
                raw
            },
            {
                let mut raw = legacy_json();
                raw["metric"]["value"] = json!(100);
                raw
            },
            {
                let mut raw = legacy_json();
                raw["sources"][0]["rows"] = json!([100]);
                raw
            },
            {
                let mut raw = legacy_json();
                raw["time_range"] = json!({"start": "start", "end": "end", "data": []});
                raw
            },
        ] {
            assert!(serde_json::from_value::<ArtifactSpec>(raw).is_err());
        }
        for block in [
            chart(),
            table(),
            ArtifactBlock::Note {
                text: "Scope".into(),
            },
        ] {
            let mut raw = legacy_json();
            let mut block = serde_json::to_value(block).unwrap();
            block["data"] = json!([100]);
            raw["blocks"] = json!([block]);
            assert!(serde_json::from_value::<ArtifactSpec>(raw).is_err());
        }
    }

    #[test]
    fn only_snapshot_refresh_is_deserializable() {
        for refresh in ["auto", "live", "manual", "Snapshot"] {
            let mut raw = legacy_json();
            raw["refresh"] = json!(refresh);
            assert!(serde_json::from_value::<ArtifactSpec>(raw).is_err());
        }
        let mut raw = legacy_json();
        raw["refresh"] = json!("snapshot");
        serde_json::from_value::<ArtifactSpec>(raw)
            .unwrap()
            .validate_shape()
            .unwrap();
    }

    #[test]
    fn timezone_requires_utc_or_valid_fixed_offset() {
        for timezone in ["UTC", "+08:00", "-05:30", "+00:00", "-00:00", "+23:59"] {
            let mut artifact = spec();
            artifact.metric.as_mut().unwrap().timezone = timezone.into();
            artifact.validate_shape().unwrap();
        }
        for timezone in [
            "Asia/Shanghai",
            "Mars/Fiction",
            "utc",
            " UTC",
            "Z",
            "+24:00",
            "-24:00",
            "+08:60",
            "+8:00",
            "+0800",
            "+08:00junk",
        ] {
            let mut artifact = spec();
            artifact.metric.as_mut().unwrap().timezone = timezone.into();
            assert_eq!(
                artifact.validate_shape().unwrap_err().kind,
                ErrorKind::Unsupported
            );
        }
    }

    #[test]
    fn rfc3339_range_validates_dates_offsets_and_chronological_order() {
        for (start, end) in [
            ("2024-02-29T00:00:00Z", "2024-03-01T00:00:00Z"),
            ("2026-10-12T10:00:00+08:00", "2026-10-12T03:00:00Z"),
            ("2026-10-12T10:00:00.123+08:00", "2026-10-12T02:00:00.123Z"),
        ] {
            let mut artifact = spec();
            artifact.time_range = Some(ArtifactTimeRange {
                start: start.into(),
                end: end.into(),
            });
            artifact.validate_shape().unwrap();
        }
        for (start, end) in [
            ("2023-02-29T00:00:00Z", "2023-03-01T00:00:00Z"),
            ("2026-10-12T10:00:00", "2026-10-12T11:00:00Z"),
            ("2026-10-12T10:00:00+24:00", "2026-10-13T11:00:00Z"),
            ("2026-10-12T01:00:00Z", "2026-10-12T08:00:00+08:00"),
            ("2026-10-12T00:00:00Z", "invalid"),
            ("", "2026-10-12T00:00:00Z"),
        ] {
            let mut artifact = spec();
            artifact.time_range = Some(ArtifactTimeRange {
                start: start.into(),
                end: end.into(),
            });
            assert!(artifact.validate_shape().is_err(), "{start} / {end}");
        }
    }

    #[test]
    fn duplicate_and_empty_source_ids_are_rejected() {
        let mut artifact = spec();
        artifact.sources.push(artifact.sources[0].clone());
        assert!(artifact.validate_shape().is_err());
        artifact.sources[1].tool_call_ref = Some("another-call".into());
        assert!(artifact.validate_shape().is_err());
        // 同次调用的不同真实结果允许分别引用；真实性仍由应用层验证。
        artifact.sources[1].result_ref = Some("result-2".into());
        artifact.sources[1].tool_call_ref = Some("call-1".into());
        artifact.validate_shape().unwrap();
        for source in [
            ArtifactSourceRef {
                result_ref: None,
                tool_call_ref: None,
            },
            ArtifactSourceRef {
                result_ref: Some(" ".into()),
                tool_call_ref: Some("call-1".into()),
            },
            ArtifactSourceRef {
                result_ref: Some("result-1".into()),
                tool_call_ref: Some("".into()),
            },
            ArtifactSourceRef {
                result_ref: None,
                tool_call_ref: Some("x".repeat(ARTIFACT_MAX_REF_CHARS + 1)),
            },
        ] {
            let mut artifact = spec();
            artifact.sources = vec![source];
            assert!(artifact.validate_shape().is_err());
        }
    }

    #[test]
    fn kind_and_blocks_must_agree_except_empty_drafts() {
        for kind in [
            ArtifactKind::Report,
            ArtifactKind::Chart,
            ArtifactKind::Table,
        ] {
            let mut artifact = spec();
            artifact.kind = kind;
            artifact.validate_shape().unwrap();
            for blocks in [
                vec![chart()],
                vec![table()],
                vec![chart(), table()],
                vec![ArtifactBlock::Note {
                    text: "Scope".into(),
                }],
            ] {
                let compatible = match kind {
                    ArtifactKind::Report => true,
                    ArtifactKind::Chart => {
                        blocks
                            .iter()
                            .any(|b| matches!(b, ArtifactBlock::Chart { .. }))
                            && !blocks
                                .iter()
                                .any(|b| matches!(b, ArtifactBlock::Table { .. }))
                    }
                    ArtifactKind::Table => {
                        blocks
                            .iter()
                            .any(|b| matches!(b, ArtifactBlock::Table { .. }))
                            && !blocks
                                .iter()
                                .any(|b| matches!(b, ArtifactBlock::Chart { .. }))
                    }
                };
                artifact.blocks = blocks;
                assert_eq!(artifact.validate_shape().is_ok(), compatible);
            }
        }
    }

    #[test]
    fn table_columns_are_bounded_nonempty_and_unique() {
        for columns in [
            vec![],
            vec![" ".into()],
            vec!["channel".into(), "channel".into()],
            vec!["x".repeat(ARTIFACT_MAX_FIELD_CHARS + 1)],
            (0..=ARTIFACT_MAX_COLUMNS)
                .map(|i| format!("field-{i}"))
                .collect(),
        ] {
            let mut artifact = spec();
            artifact.blocks = vec![ArtifactBlock::Table {
                result_ref: "result-1".into(),
                columns,
            }];
            assert!(artifact.validate_shape().is_err());
        }
    }

    #[test]
    fn text_limits_and_required_ids_apply_to_new_fields() {
        type InvalidCase = fn(&mut ArtifactSpec);
        let cases: &[InvalidCase] = &[
            |s| s.title = "x".repeat(ARTIFACT_MAX_TITLE_CHARS + 1),
            |s| s.renderer.clear(),
            |s| s.renderer = "x".repeat(ARTIFACT_MAX_RENDERER_CHARS + 1),
            |s| s.template_version.clear(),
            |s| s.template_version = "x".repeat(ARTIFACT_MAX_TEMPLATE_VERSION_CHARS + 1),
            |s| s.accessibility_description = "x".repeat(ARTIFACT_MAX_ACCESSIBILITY_CHARS + 1),
            |s| s.filters = vec![" ".into()],
            |s| s.filters = vec!["x".repeat(ARTIFACT_MAX_FILTER_CHARS + 1)],
            |s| s.filters = vec!["x".into(); ARTIFACT_MAX_FILTERS + 1],
            |s| s.metric.as_mut().unwrap().name = "x".repeat(ARTIFACT_MAX_FIELD_CHARS + 1),
            |s| s.metric.as_mut().unwrap().definition_ref = Some(" ".into()),
            |s| {
                s.metric.as_mut().unwrap().definition_ref =
                    Some("x".repeat(ARTIFACT_MAX_REF_CHARS + 1))
            },
            |s| s.metric.as_mut().unwrap().assumption = Some(" ".into()),
            |s| {
                s.metric.as_mut().unwrap().assumption =
                    Some("x".repeat(ARTIFACT_MAX_NOTE_CHARS + 1))
            },
            |s| s.metric.as_mut().unwrap().timezone.clear(),
            |s| s.metric.as_mut().unwrap().timezone = "x".repeat(ARTIFACT_MAX_TIMEZONE_CHARS + 1),
            |s| {
                s.blocks = vec![ArtifactBlock::Note {
                    text: "x".repeat(ARTIFACT_MAX_NOTE_CHARS + 1),
                }]
            },
            |s| s.blocks = vec![ArtifactBlock::Note { text: " ".into() }],
            |s| s.blocks = vec![chart(); ARTIFACT_MAX_BLOCKS + 1],
            |s| {
                s.blocks = vec![ArtifactBlock::Chart {
                    result_ref: "".into(),
                    chart: ArtifactChart::Line,
                    x: "x".into(),
                    y: "y".into(),
                }]
            },
            |s| {
                s.blocks = vec![ArtifactBlock::Chart {
                    result_ref: "result-1".into(),
                    chart: ArtifactChart::Line,
                    x: " ".into(),
                    y: "y".into(),
                }]
            },
            |s| s.sources.clear(),
            |s| {
                s.sources = (0..=ARTIFACT_MAX_SOURCES)
                    .map(|i| ArtifactSourceRef {
                        result_ref: Some(format!("result-{i}")),
                        tool_call_ref: None,
                    })
                    .collect()
            },
        ];
        for (index, mutate) in cases.iter().enumerate() {
            let mut artifact = spec();
            mutate(&mut artifact);
            assert!(artifact.validate_shape().is_err(), "case {index}");
        }
        let mut artifact = spec();
        artifact.filters = vec!["x".repeat(ARTIFACT_MAX_FILTER_CHARS); ARTIFACT_MAX_FILTERS];
        artifact.template_version = "x".repeat(ARTIFACT_MAX_TEMPLATE_VERSION_CHARS);
        artifact.accessibility_description = "x".repeat(ARTIFACT_MAX_ACCESSIBILITY_CHARS);
        artifact.metric.as_mut().unwrap().assumption = Some("Assume gross revenue".into());
        artifact.validate_shape().unwrap();
    }
}

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
/// 饼图切片上限：分类数超过此值时 renderer 不绘制饼图，改用可见说明（占比图切得太细读不出构成）。
pub const ARTIFACT_MAX_PIE_SLICES: usize = 8;
/// 来源引用 ID 字符上限。
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

/// 图表种类。选择依据是 x 的语义与分类数量，而不是渲染偏好：连续有序的 x（时间序列）看趋势
/// 用折线、强调量级用面积；无序分类做比较用柱状，分类名长或按值排名用条形；分类不多且看占比构成用饼图。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactChart {
    /// 折线：x 为有序/连续量，看变化趋势。
    Line,
    /// 柱状（纵向）：x 为分类且标签较短，做分类间比较。
    Bar,
    /// 条形（横向柱状）：分类标签较长或需要按数值排名。
    BarHorizontal,
    /// 面积：形状同折线，强调量级与累计规模。
    Area,
    /// 饼图：分类数不超过 `ARTIFACT_MAX_PIE_SLICES` 时的占比构成。
    Pie,
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

/// 指标口径：名称、未确认假设与时区。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactMetric {
    pub name: String,
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
            if let Some(assumption) = &metric.assumption {
                validate_artifact_text(assumption, ARTIFACT_MAX_NOTE_CHARS, "口径假设")?;
            }
            normalize_timezone(&metric.timezone)?;
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

    /// 归一化可归一化的写法（时区），让落库 spec、回执与报告显示使用同一偏移表示。
    pub fn normalize(&mut self) -> Result<()> {
        if let Some(metric) = &mut self.metric {
            metric.timezone = normalize_timezone(&metric.timezone)?;
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
///
/// 把常见写法归一化成 `UTC` / `±HH:MM`：模型常写 `UTC+08:00`、`GMT+8`、`+8`、
/// `+0800`、`utc+8`。归一化只变换表示，不改变偏移语义；无法识别的写法（IANA
/// 名称、缺数字、越界偏移）返回 `Unsupported`，错误信息回显收到的原文。
pub fn normalize_timezone(timezone: &str) -> Result<String> {
    validate_artifact_text(timezone, ARTIFACT_MAX_TIMEZONE_CHARS, "时区标识")?;
    let raw = timezone.trim();
    if raw.eq_ignore_ascii_case("UTC") || raw.eq_ignore_ascii_case("GMT") || raw == "Z" {
        return Ok("UTC".into());
    }
    // 前缀大小写不敏感：`utc+08:00` 与 `UTC+08:00` 表示同一偏移。
    let rest = ["UTC", "GMT"]
        .iter()
        .find_map(|prefix| {
            raw.get(..prefix.len())
                .filter(|head| head.eq_ignore_ascii_case(prefix))
                .map(|_| raw[prefix.len()..].trim())
        })
        .unwrap_or(raw);
    let (sign, digits) = match rest.as_bytes().first() {
        Some(b'+') => ("+", rest[1..].trim()),
        Some(b'-') => ("-", rest[1..].trim()),
        _ => return Err(unsupported_timezone(timezone)),
    };
    // `+8` / `+8:00` / `+0800` 都是同一偏移；每段必须是 1–2 位数字。
    let (hours, minutes) = match digits.split_once(':') {
        Some((hours, minutes)) => (hours, Some(minutes)),
        None if digits.len() == 4 => (&digits[..2], Some(&digits[2..])),
        None => (digits, None),
    };
    // 每段必须是 1–2 位数字；未写分钟按 0 处理，写了非法分钟直接拒绝。
    let parse_component = |text: &str| {
        (text.len() <= 2 && !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
            .then(|| text.parse::<u32>().ok())
            .flatten()
    };
    let (Some(hours), minutes) = (parse_component(hours), minutes) else {
        return Err(unsupported_timezone(timezone));
    };
    let minutes = match minutes {
        Some(text) => match parse_component(text) {
            Some(value) => value,
            None => return Err(unsupported_timezone(timezone)),
        },
        None => 0,
    };
    let formatted = format!("{sign}{hours:02}:{minutes:02}");
    formatted
        .parse::<chrono::FixedOffset>()
        .map_err(|_| unsupported_timezone(timezone))?;
    Ok(formatted)
}

fn unsupported_timezone(raw: &str) -> Error {
    Error::new(
        ErrorKind::Unsupported,
        format!("时区仅支持 UTC 或有效的 ±HH:MM 固定偏移；收到 {raw:?}，IANA 时区名称尚不支持"),
    )
}


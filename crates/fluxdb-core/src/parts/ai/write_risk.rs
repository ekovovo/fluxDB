// 写入风险评审（设计 §29.2）。
//
// 代码规则与评审模型（Jev 优先、当前对话模型回退）是同一层级的"评分者"：
// 各自给出 0–1 的风险分，合成后与阈值比较，超过阈值转人工。
// 这里只放**无副作用**的评分与合成，不接触存储、模型或驱动。

/// 信号类别：硬信号是机制上可证实的边界，模型分不能把它压下去。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteRiskClass {
    /// 确定性边界（无 WHERE、多语句、DDL、命中 denylist、跨作用域…）：
    /// 与 Zed 的硬编码安全规则、Goose 的 `max(pattern_confidence)` 同类，
    /// 模型只能在其之上加分，不能下穿。
    Hard,
    /// 不确定度估计（谓词没有唯一索引、连接器给不出有界提交证明…）：
    /// 允许评审模型按自己的判断下调，这是"把权限放给 Jev"的实际落点。
    Soft,
}

/// 一条可核查的风险依据；`code` 是稳定机器键，`detail` 只描述规则与结构，不含业务值。
#[derive(Clone, Debug, PartialEq)]
pub struct WriteRiskSignal {
    pub code: String,
    pub score: f64,
    pub class: WriteRiskClass,
    pub detail: String,
}

impl WriteRiskSignal {
    pub fn hard(code: impl Into<String>, score: f64, detail: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            // 越界分数是程序缺陷，收紧到 0–1 而不是把畸形分数带进判定。
            score: score.clamp(0.0, 1.0),
            class: WriteRiskClass::Hard,
            detail: detail.into(),
        }
    }

    pub fn soft(code: impl Into<String>, score: f64, detail: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            score: score.clamp(0.0, 1.0),
            class: WriteRiskClass::Soft,
            detail: detail.into(),
        }
    }
}

/// 一次写入的确定性证据：规则信号 + 交给评审模型的脱敏结构事实。
///
/// `evidence_json` 是**唯一**允许出站的内容：不含 SQL 原文、参数值、对象名、结果集或凭据。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WriteRiskFacts {
    pub signals: Vec<WriteRiskSignal>,
    pub evidence_json: String,
    /// 连接器能否为这次写入提供有界提交（事务内复核真实影响）。由工具按连接能力给出，
    /// 不靠 SQL 文本推断：能力与语句形状是两件事，不能互相冒充。
    pub bounded_capable: bool,
}

impl WriteRiskFacts {
    pub fn new(evidence_json: impl Into<String>) -> Self {
        Self {
            signals: Vec::new(),
            evidence_json: evidence_json.into(),
            bounded_capable: false,
        }
    }

    /// 声明连接器提供了有界提交能力。
    pub fn with_bounded_commit(mut self, bounded: bool) -> Self {
        self.bounded_capable = bounded;
        self
    }

    /// 加一条硬信号（确定性边界）。
    pub fn hard(&mut self, code: impl Into<String>, score: f64, detail: impl Into<String>) {
        self.signals.push(WriteRiskSignal::hard(code, score, detail));
    }

    /// 加一条软信号（不确定度估计）。
    pub fn soft(&mut self, code: impl Into<String>, score: f64, detail: impl Into<String>) {
        self.signals.push(WriteRiskSignal::soft(code, score, detail));
    }

    /// 规则分：取最保守的一条信号，避免多条低分信号把一条硬风险摊薄。
    pub fn rule_score(&self) -> f64 {
        self.signals
            .iter()
            .map(|signal| signal.score)
            .fold(0.0_f64, f64::max)
    }

    /// 硬信号下界：模型分不能把综合分压到这条线以下。
    pub fn hard_floor(&self) -> f64 {
        self.signals
            .iter()
            .filter(|signal| signal.class == WriteRiskClass::Hard)
            .map(|signal| signal.score)
            .fold(0.0_f64, f64::max)
    }

    pub fn codes(&self) -> Vec<&str> {
        self.signals.iter().map(|signal| signal.code.as_str()).collect()
    }
}

/// 评审模型给出的风险分（来源用于审计与卡片文案）。
#[derive(Clone, Debug, PartialEq)]
pub struct ModelRisk {
    pub score: f64,
    pub source: String,
}

impl ModelRisk {
    pub fn new(score: f64, source: impl Into<String>) -> Self {
        Self {
            score: score.clamp(0.0, 1.0),
            source: source.into(),
        }
    }
}

/// 评审结论：要么给分，要么明确不可用（原因进审计与提示，不静默当作低风险）。
#[derive(Clone, Debug, PartialEq)]
pub enum WriteReviewOutcome {
    Scored(ModelRisk),
    /// 评审模型不可用：**不是**低风险，调用方必须退回代码规则分。
    Unavailable { reason: String },
}

/// 合成后的写入风险决定。
#[derive(Clone, Debug, PartialEq)]
pub struct WriteRiskDecision {
    pub rule_score: f64,
    pub hard_floor: f64,
    pub signals: Vec<WriteRiskSignal>,
    pub model_score: Option<f64>,
    pub model_source: Option<String>,
    pub final_score: f64,
    pub threshold: f64,
}

impl WriteRiskDecision {
    /// 综合分不超过阈值才允许自动放行。
    pub fn allows_automatic(&self) -> bool {
        self.final_score <= self.threshold
    }

    /// 审计与审批卡用的一行摘要：各信号与模型分逐项列出，人能看懂为什么是这个结论。
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = self
            .signals
            .iter()
            .map(|signal| {
                let class = match signal.class {
                    WriteRiskClass::Hard => "!",
                    WriteRiskClass::Soft => "",
                };
                format!("{}{}={:.2}", class, signal.code, signal.score)
            })
            .collect();
        parts.sort();
        match (self.model_score, self.model_source.as_deref()) {
            (Some(score), Some(source)) => parts.push(format!("{source}={score:.2}")),
            (Some(score), None) => parts.push(format!("model={score:.2}")),
            _ => parts.push("model=unavailable".to_string()),
        }
        format!(
            "{} → 综合 {:.2} / 阈值 {:.2}",
            parts.join(" "),
            self.final_score,
            self.threshold
        )
    }
}

/// 默认阈值：不高于它自动放行，高于它转人工。
pub const DEFAULT_WRITE_RISK_THRESHOLD: f64 = 0.35;
/// 默认评审模型权重。
pub const DEFAULT_WRITE_REVIEW_WEIGHT: f64 = 0.5;
/// 评审模型权重上限：模型与代码规则平权，但不能单方面压倒规则分。
pub const MAX_WRITE_REVIEW_WEIGHT: f64 = 0.5;

/// 合成写入风险。
///
/// - 没有模型分（Jev 与回退生成模型都不可用）时直接采用规则分；
/// - 有模型分时按权重混合，再与硬信号下界取最大值：模型可以下调"不确定度"，
///   但改不动无 WHERE、多语句、DDL 这类机制上可证实的边界。
pub fn combine_write_risk(
    facts: &WriteRiskFacts,
    model: Option<&ModelRisk>,
    threshold: f64,
    weight: f64,
) -> WriteRiskDecision {
    let rule_score = facts.rule_score();
    let hard_floor = facts.hard_floor();
    let weight = weight.clamp(0.0, MAX_WRITE_REVIEW_WEIGHT);
    let final_score = match model {
        Some(model) => {
            let blended = (1.0 - weight) * rule_score + weight * model.score;
            // 硬下界优先：模型分不能把确定性边界压下去（对齐 Goose 的 max(pattern)）。
            blended.max(hard_floor)
        }
        None => rule_score,
    }
    .clamp(0.0, 1.0);
    WriteRiskDecision {
        rule_score,
        hard_floor,
        signals: facts.signals.clone(),
        model_score: model.map(|model| model.score),
        model_source: model.map(|model| model.source.clone()),
        final_score,
        threshold,
    }
}

/// 自动放行时使用的写入提交路径。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WriteCommit {
    /// 普通提交：判定为可放行的写入按原语句执行。
    #[default]
    Ordinary,
    /// 有界提交：连接器能证明并限制影响，执行时在事务内复核真实影响。
    Bounded,
}

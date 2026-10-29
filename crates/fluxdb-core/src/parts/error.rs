pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    /// 上游 HTTP 状态码（仅模型/网络请求类错误有值）。
    ///
    /// 存在的意义是让**重试策略**能区分「限流/上游故障，可重试」与「鉴权/参数错误，
    /// 重试无意义」——只有中文 message 时调用方无法判断，只能一律重试或一律放弃。
    /// 非 HTTP 来源的错误一律为 `None`，调用方按 `None`（网络层不确定）保守处理。
    pub http_status: Option<u16>,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            http_status: None,
        }
    }

    /// 附带上游 HTTP 状态码的错误。
    ///
    /// 状态码只用于**重试决策与诊断**，不参与任何授权或权限判断。
    pub fn with_http_status(kind: ErrorKind, message: impl Into<String>, status: u16) -> Self {
        Self {
            kind,
            message: message.into(),
            http_status: Some(status),
        }
    }

    /// 提交/回滚失败后，把「结果可能已落库」的错误归为 [`ErrorKind::OutcomeUnknown`]。
    ///
    /// 只有网络类与取消类失败会落到这里：它们是**提交途中失去确认**，服务端可能已提交。
    /// 数据库返回的显式错误（约束冲突、死锁）表示事务已被服务端终止，结果是确定的失败，
    /// 原样返回，避免把普通失败报成「结果未知」而阻塞后续操作。
    pub fn mark_outcome_unknown(self, action: &str) -> Self {
        if matches!(
            self.kind,
            ErrorKind::Connection | ErrorKind::Timeout | ErrorKind::Cancelled
        ) {
            Self::new(
                ErrorKind::OutcomeUnknown,
                format!("{action}失败，写入结果未知：{}", self.message),
            )
        } else {
            self
        }
    }

    /// 该错误是否值得重试（模型请求类）。
    ///
    /// 口径对齐 Pi 复刻的 OpenAI/Anthropic SDK 策略：网络层不确定、限流与服务端故障
    /// 可重试；确定性失败（鉴权/参数/不支持/权限）与用户取消不重试。
    pub fn is_retryable(&self) -> bool {
        if matches!(
            self.kind,
            ErrorKind::Cancelled
                | ErrorKind::Authentication
                | ErrorKind::Permission
                | ErrorKind::Unsupported
                | ErrorKind::Query
                // 结果未知时重试可能重复写入，必须人工核实后再决定。
                | ErrorKind::OutcomeUnknown
        ) {
            return false;
        }
        match self.http_status {
            // 无状态码：网络层失败（连接重置、DNS 抖动等），最值得重试。
            None => matches!(self.kind, ErrorKind::Connection | ErrorKind::Timeout),
            Some(status) => {
                status == 408 || status == 429 || (500..600).contains(&status)
            }
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Connection,
    Authentication,
    Permission,
    Query,
    Cancelled,
    Unsupported,
    /// 拨号/握手/建立连接超时（区别于连接被拒绝等 `Connection` 错误）。
    Timeout,
    Internal,
    /// 提交/回滚未能确认：语句可能已落库，也可能没有。
    ///
    /// 只在**写操作收尾失败**时使用（提交期间断网、只读事务回滚失败等）。
    /// 调用方必须按「结果未知」处理：既不能当成功，也不能当确定失败，
    /// 更不能用它触发自动重试——重放可能造成重复写入。
    OutcomeUnknown,
}


#[cfg(test)]
mod outcome_unknown_tests {
    use super::{Error, ErrorKind};

    #[test]
    fn transport_failures_become_outcome_unknown() {
        // 提交途中断连：结果可能已落库。
        for kind in [ErrorKind::Connection, ErrorKind::Timeout, ErrorKind::Cancelled] {
            let error = Error::new(kind, "connection reset").mark_outcome_unknown("提交事务");
            assert_eq!(error.kind, ErrorKind::OutcomeUnknown, "{kind:?}");
            assert!(error.message.contains("结果未知"));
        }
    }

    #[test]
    fn database_rejections_stay_deterministic_failures() {
        // 服务端明确拒绝（约束冲突等）= 确定没提交，不能报成「结果未知」。
        let error = Error::new(ErrorKind::Query, "duplicate key").mark_outcome_unknown("提交事务");
        assert_eq!(error.kind, ErrorKind::Query);
        assert_eq!(error.message, "duplicate key");
    }

    #[test]
    fn outcome_unknown_is_never_retryable() {
        let error = Error::new(ErrorKind::OutcomeUnknown, "提交结果未知");
        assert!(!error.is_retryable());
    }
}

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
}


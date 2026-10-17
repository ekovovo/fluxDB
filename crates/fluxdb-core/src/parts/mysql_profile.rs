// MySQL/TiDB 专用连接配置模型。
//
// 目标：把原本分散在 `ConnectionConfig.options` 这组扁平字符串里的 TLS /
// SSH 隧道 / 代理 / 超时（connect/query/idle TTL）等连接语义，收敛成结构化、
// 可迁移、可校验的单一模型，作为 `ConnectionConfig` 的补充。
//
// 设计约定（镜像 redis_profile.rs 的既有范式）：
// - 本模型只依赖 `serde` 与 `std`，不引入第三方 URL 解析库。
// - 所有密钥类内容（密码、私钥、证书正文、SSH 口令）都走 [`SecretRef`]：引用
//   （`key`）可落盘，`inline` 受控值标记 `#[serde(skip)]`，绝不序列化到磁盘。
// - 通过 [`MysqlConnectionProfile::from_options`] 承接历史 `options` 扁平参数，
//   通过 [`MysqlConnectionProfile::into_options`] 回写兼容参数供现有消费方复用。
//
// 范围说明：SSH 隧道已复用 Redis 的 `SshTunnel` 真正拨号；SOCKS5/HTTP 代理与
// 断线重连在此仅建模并校验（`validate` 对 enabled 的代理返回"暂未支持"），首版不拨号。

/// MySQL TLS 模式。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum MysqlSslMode {
    /// 禁用 TLS。
    Disabled,
    /// 优先 TLS（服务器支持则加密），默认。
    #[default]
    Preferred,
    /// 强制 TLS：服务器不支持则连接失败。
    Required,
}

/// TLS 相关参数。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MysqlTlsOptions {
    /// 是否启用 TLS。
    pub enabled: bool,
    /// TLS 模式（`Required` 时需先启用并配证书）。
    pub ssl_mode: MysqlSslMode,
    /// CA 证书文件路径引用。
    pub ca: SecretRef,
    /// 客户端证书文件路径引用。
    pub client_cert: SecretRef,
    /// 客户端私钥文件路径引用。
    pub client_key: SecretRef,
    /// SNI / 校验主机名（默认取连接主机名）。
    pub sni: String,
    /// 是否校验证书。关闭仅用于自签环境。
    pub verify: bool,
    /// 连接字符集（默认 utf8mb4）。
    pub charset: String,
    /// 排序规则。
    pub collation: String,
}

impl Default for MysqlTlsOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            ssl_mode: MysqlSslMode::Preferred,
            ca: SecretRef::default(),
            client_cert: SecretRef::default(),
            client_key: SecretRef::default(),
            sni: String::new(),
            verify: true,
            charset: "utf8mb4".into(),
            collation: String::new(),
        }
    }
}

/// SSH 认证方式。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum MysqlSshAuth {
    #[default]
    Password,
    PrivateKey,
}

/// SSH 隧道参数。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MysqlSshOptions {
    /// 是否启用 SSH 隧道。
    pub enabled: bool,
    /// 跳板主机。
    pub host: String,
    /// 跳板端口（默认 22）。
    pub port: u16,
    /// 跳板用户名。
    pub username: String,
    /// SSH 认证类型。
    pub auth: MysqlSshAuth,
    /// 密码（password 认证）。
    pub password: SecretRef,
    /// 私钥文件路径引用（key/private-key 认证）。
    pub private_key: SecretRef,
    /// 私钥口令（可选）。
    pub passphrase: SecretRef,
    /// SSH 连接超时（秒），0 表示继承全局 connect 超时。
    pub connect_timeout_secs: u32,
    /// SSH 心跳间隔（秒），默认 30；0 表示不发送。
    pub keepalive_interval_secs: u32,
}

/// 代理类型。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum MysqlProxyType {
    #[default]
    Socks5,
    HttpConnect,
}

impl MysqlProxyType {
    /// 该类型代理的默认端口。
    pub fn default_port(&self) -> u16 {
        match self {
            MysqlProxyType::Socks5 => 1080,
            MysqlProxyType::HttpConnect => 8080,
        }
    }
}

/// 代理参数（首版仅建模与校验，不实际拨号）。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MysqlProxy {
    /// 是否启用代理。
    pub enabled: bool,
    /// 代理类型。
    pub proxy_type: MysqlProxyType,
    /// 代理主机。
    pub host: String,
    /// 代理端口。
    pub port: u16,
    /// 代理用户名（可选）。
    pub username: String,
    /// 代理密码（可选）。
    pub password: SecretRef,
}

/// 传输层：SSH 隧道或（未拨号的）代理。`transport: Vec` 支持多跳扩展。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MysqlTransportLayer {
    Ssh(MysqlSshOptions),
    Proxy(MysqlProxy),
}

/// 高级连接选项。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MysqlAdvancedOptions {
    /// 建连超时（秒），默认 5。
    pub connect_timeout_secs: u32,
    /// 查询超时（秒），0 表示不设限。
    pub query_timeout_secs: u32,
    /// 连接空闲 TTL（秒），0 表示不回收。
    pub idle_ttl_secs: u32,
    /// TCP 长连接保活。
    pub tcp_keepalive: bool,
}

impl Default for MysqlAdvancedOptions {
    fn default() -> Self {
        Self {
            connect_timeout_secs: 5,
            query_timeout_secs: 0,
            idle_ttl_secs: 0,
            tcp_keepalive: true,
        }
    }
}

/// 基础连接参数。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MysqlBasicOptions {
    /// 目标主机。
    pub host: String,
    /// 目标端口（默认 3306）。
    pub port: u16,
    /// 逻辑库名。
    pub database: String,
    /// 用户名（默认 root）。
    pub username: String,
    /// 认证密码（不安全）。
    pub password: SecretRef,
}

/// MySQL 完整连接档案：承接 TLS / SSH / 代理 / 高级超时。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MysqlConnectionProfile {
    pub basic: MysqlBasicOptions,
    pub tls: MysqlTlsOptions,
    pub transport: Vec<MysqlTransportLayer>,
    pub advanced: MysqlAdvancedOptions,
}

impl MysqlConnectionProfile {
    /// —— 解析职责 ——

    /// 从历史 `options` 扁平参数迁移出一份档案。
    ///
    /// 只读取已知键，其余任意参数不做透传，避免把垃圾灌进结构化模型。
    pub fn from_options(opts: &std::collections::BTreeMap<String, String>) -> Self {
        let get = |key: &str| opts.get(key).cloned();
        let basic = MysqlBasicOptions {
            host: opts.get("host").cloned().unwrap_or_default(),
            port: opts
                .get("port")
                .and_then(|p| p.parse().ok())
                .unwrap_or(3306),
            database: get("database").unwrap_or_default(),
            username: get("username").unwrap_or_else(|| "root".into()),
            password: SecretRef::inline(get("password").unwrap_or_default()),
        };
        let tls = MysqlTlsOptions {
            enabled: connection_option_is_true(get("tls").as_ref()),
            ssl_mode: match get("ssl_mode").as_deref() {
                Some("disabled") => MysqlSslMode::Disabled,
                Some("required") => MysqlSslMode::Required,
                _ => MysqlSslMode::Preferred,
            },
            ca: SecretRef::ref_key(get("tls_ca_ref").unwrap_or_default()),
            client_cert: SecretRef::ref_key(get("tls_client_cert_ref").unwrap_or_default()),
            client_key: SecretRef::ref_key(get("tls_client_key_ref").unwrap_or_default()),
            sni: get("tls_server_name").unwrap_or_default(),
            verify: !connection_option_is_true(get("tls_insecure").as_ref()),
            charset: get("charset")
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| "utf8mb4".into()),
            collation: get("collation").unwrap_or_default(),
        };
        let mut transport = Vec::new();
        let ssh = MysqlSshOptions {
            enabled: connection_option_is_true(get("ssh_enabled").as_ref()),
            host: get("ssh_host").unwrap_or_default(),
            port: get("ssh_port").and_then(|p| p.parse().ok()).unwrap_or(22),
            username: get("ssh_username").unwrap_or_default(),
            auth: match get("ssh_auth").as_deref() {
                Some("key") | Some("private_key") => MysqlSshAuth::PrivateKey,
                _ => MysqlSshAuth::Password,
            },
            password: SecretRef::inline(get("ssh_password").unwrap_or_default()),
            private_key: SecretRef::ref_key(get("ssh_private_key_ref").unwrap_or_default()),
            passphrase: SecretRef::inline(get("ssh_passphrase").unwrap_or_default()),
            connect_timeout_secs: get("ssh_connect_timeout_secs")
                .and_then(|p| p.parse().ok())
                .unwrap_or(0),
            keepalive_interval_secs: get("ssh_keepalive_interval_secs")
                .and_then(|p| p.parse().ok())
                .unwrap_or(30),
        };
        if ssh.enabled {
            transport.push(MysqlTransportLayer::Ssh(ssh));
        }
        let proxy = MysqlProxy {
            enabled: connection_option_is_true(get("proxy_enabled").as_ref()),
            proxy_type: match get("proxy_type").as_deref() {
                Some("http_connect") => MysqlProxyType::HttpConnect,
                _ => MysqlProxyType::Socks5,
            },
            host: get("proxy_host").unwrap_or_default(),
            port: get("proxy_port")
                .and_then(|p| p.parse().ok())
                .unwrap_or_else(|| MysqlProxyType::Socks5.default_port()),
            username: get("proxy_username").unwrap_or_default(),
            password: SecretRef::inline(get("proxy_password").unwrap_or_default()),
        };
        if proxy.enabled {
            transport.push(MysqlTransportLayer::Proxy(proxy));
        }
        Self {
            basic,
            tls,
            transport,
            advanced: MysqlAdvancedOptions {
                connect_timeout_secs: get("connect_timeout_secs")
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(5),
                query_timeout_secs: get("query_timeout_secs")
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(0),
                idle_ttl_secs: get("idle_ttl_secs").and_then(|p| p.parse().ok()).unwrap_or(0),
                // 缺省视为启用（默认 true）。
                tcp_keepalive: match get("tcp_keepalive") {
                    Some(v) => connection_option_is_true(Some(&v)),
                    None => true,
                },
            },
        }
    }

    /// 把档案回写成一组合法的 `options` 兼容参数。
    ///
    /// 供既有扁平参数消费方（如连接测试/保存路径）复用；不写密钥正文，
    /// 密码走引用/占用 `password` 键交给 storage 决定是否落盘。
    pub fn into_options(&self) -> std::collections::BTreeMap<String, String> {
        let mut ops = std::collections::BTreeMap::new();
        if !self.basic.host.is_empty() {
            ops.insert("host".into(), self.basic.host.clone());
        }
        ops.insert("port".into(), self.basic.port.to_string());
        if !self.basic.database.is_empty() {
            ops.insert("database".into(), self.basic.database.clone());
        }
        if !self.basic.username.is_empty() {
            ops.insert("username".into(), self.basic.username.clone());
        }
        if let Some(password) = self.basic.password.value() {
            ops.insert("password".into(), password.to_string());
        }

        let t = &self.tls;
        ops.insert("tls".into(), t.enabled.to_string());
        ops.insert(
            "ssl_mode".into(),
            match t.ssl_mode {
                MysqlSslMode::Disabled => "disabled".into(),
                MysqlSslMode::Preferred => "preferred".into(),
                MysqlSslMode::Required => "required".into(),
            },
        );
        if !t.sni.is_empty() {
            ops.insert("tls_server_name".into(), t.sni.clone());
        }
        ops.insert("tls_insecure".into(), (!t.verify).to_string());
        if !t.ca.key.is_empty() || !t.client_cert.key.is_empty() || !t.client_key.key.is_empty() {
            ops.insert("tls_ca_ref".into(), t.ca.key.clone());
            ops.insert("tls_client_cert_ref".into(), t.client_cert.key.clone());
            ops.insert("tls_client_key_ref".into(), t.client_key.key.clone());
        }
        if t.charset != "utf8mb4" {
            ops.insert("charset".into(), t.charset.clone());
        }
        if !t.collation.is_empty() {
            ops.insert("collation".into(), t.collation.clone());
        }

        for layer in &self.transport {
            match layer {
                MysqlTransportLayer::Ssh(ssh) if ssh.enabled => {
                    ops.insert("ssh_enabled".into(), "true".into());
                    ops.insert("ssh_host".into(), ssh.host.clone());
                    ops.insert("ssh_port".into(), ssh.port.to_string());
                    ops.insert("ssh_username".into(), ssh.username.clone());
                    ops.insert(
                        "ssh_auth".into(),
                        match ssh.auth {
                            MysqlSshAuth::Password => "password".into(),
                            MysqlSshAuth::PrivateKey => "private_key".into(),
                        },
                    );
                    if let Some(p) = ssh.password.value() {
                        ops.insert("ssh_password".into(), p.to_string());
                    }
                    if !ssh.private_key.key.is_empty() {
                        ops.insert("ssh_private_key_ref".into(), ssh.private_key.key.clone());
                    }
                    if let Some(p) = ssh.passphrase.value() {
                        ops.insert("ssh_passphrase".into(), p.to_string());
                    }
                    if ssh.connect_timeout_secs > 0 {
                        ops.insert("ssh_connect_timeout_secs".into(), ssh.connect_timeout_secs.to_string());
                    }
                    if ssh.keepalive_interval_secs != 30 {
                        ops.insert("ssh_keepalive_interval_secs".into(), ssh.keepalive_interval_secs.to_string());
                    }
                }
                MysqlTransportLayer::Proxy(proxy) if proxy.enabled => {
                    ops.insert("proxy_enabled".into(), "true".into());
                    ops.insert(
                        "proxy_type".into(),
                        match proxy.proxy_type {
                            MysqlProxyType::Socks5 => "socks5".into(),
                            MysqlProxyType::HttpConnect => "http_connect".into(),
                        },
                    );
                    ops.insert("proxy_host".into(), proxy.host.clone());
                    ops.insert("proxy_port".into(), proxy.port.to_string());
                    if !proxy.username.is_empty() {
                        ops.insert("proxy_username".into(), proxy.username.clone());
                    }
                    if let Some(p) = proxy.password.value() {
                        ops.insert("proxy_password".into(), p.to_string());
                    }
                }
                _ => {}
            }
        }

        let a = &self.advanced;
        ops.insert("connect_timeout_secs".into(), a.connect_timeout_secs.to_string());
        if a.query_timeout_secs > 0 {
            ops.insert("query_timeout_secs".into(), a.query_timeout_secs.to_string());
        }
        if a.idle_ttl_secs > 0 {
            ops.insert("idle_ttl_secs".into(), a.idle_ttl_secs.to_string());
        }
        ops.insert("tcp_keepalive".into(), a.tcp_keepalive.to_string());

        ops
    }

    /// —— 拨号辅助 ——

    /// 首个启用的 SSH 隧道层（无则 `None`）。
    pub fn ssh(&self) -> Option<&MysqlSshOptions> {
        self.transport.iter().find_map(|layer| match layer {
            MysqlTransportLayer::Ssh(ssh) if ssh.enabled => Some(ssh),
            _ => None,
        })
    }

    /// 首个启用的代理层（无则 `None`；首版不拨号）。
    pub fn proxy(&self) -> Option<&MysqlProxy> {
        self.transport.iter().find_map(|layer| match layer {
            MysqlTransportLayer::Proxy(proxy) if proxy.enabled => Some(proxy),
            _ => None,
        })
    }

    /// 实际用于拨号的目标端点（未被 SSH 隧道改写的前提下）。
    pub fn dial_endpoint(&self) -> (String, u16) {
        (self.basic.host.clone(), self.basic.port)
    }

    /// 认证密码。
    pub fn password(&self) -> Option<&str> {
        self.basic.password.value()
    }

    /// 建连超时。
    pub fn connect_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.advanced.connect_timeout_secs.max(1).into())
    }

    /// 建连超时秒数。
    pub fn connect_timeout_secs(&self) -> u32 {
        self.advanced.connect_timeout_secs
    }

    /// 校验：返回第一条不合法的中文错误；`None` 表示通过。
    pub fn validate(&self) -> Option<String> {
        if self.basic.host.is_empty() {
            return Some("请填写主机".into());
        }
        if self.basic.port == 0 {
            return Some("端口不合法".into());
        }
        if self.tls.ssl_mode == MysqlSslMode::Required && !self.tls.enabled {
            return Some("强制 TLS 模式需先启用 TLS".into());
        }
        if self.proxy().is_some() {
            // 首版代理不拨号：阻止保存/启用一个无法生效的配置。
            return Some("代理传输暂未支持，请先关闭代理或仅使用 SSH 隧道".into());
        }
        if let Some(ssh) = self.ssh() {
            if ssh.host.is_empty() {
                return Some("请填写 SSH 隧道主机".into());
            }
            if ssh.port == 0 {
                return Some("SSH 端口不合法".into());
            }
            if ssh.username.is_empty() {
                return Some("请填写 SSH 用户名".into());
            }
            if ssh.auth == MysqlSshAuth::PrivateKey && ssh.private_key.key.is_empty() {
                return Some("请填写 SSH 私钥文件路径".into());
            }
        }
        if self.advanced.connect_timeout_secs > 86400
            || self.advanced.query_timeout_secs > 86400
            || self.advanced.idle_ttl_secs > 86400
        {
            return Some("高级超时值不能超过 86400 秒".into());
        }
        None
    }
}


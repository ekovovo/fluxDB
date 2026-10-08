fn field_row_light(
    label: &'static str,
    field_id: ConnectionField,
    inputs: &NewConnectionInputs,
    secure: bool,
    colors: UiColors,
    window: &mut Window,
    cx: &mut Context<NavicatMain>,
) -> Field {
    field().label(label).items_center().child(input_box_light(
        field_id, inputs, secure, colors, window, cx,
    ))
}

fn host_port_row_light(
    label: &'static str,
    inputs: &NewConnectionInputs,
    colors: UiColors,
    window: &mut Window,
    cx: &mut Context<NavicatMain>,
) -> Field {
    field().label(label).items_center().child(
        div()
            .w_full()
            .flex()
            .items_center()
            .gap_3()
            .child(
                input_box_light(ConnectionField::Host, inputs, false, colors, window, cx)
                    .flex_1()
                    .min_w(px(0.)),
            )
            .child(
                input_box_light(ConnectionField::Port, inputs, false, colors, window, cx)
                    .w(px(96.))
                    .flex_none(),
            ),
    )
}

fn sqlite_file_row_light(
    inputs: &NewConnectionInputs,
    colors: UiColors,
    window: &mut Window,
    cx: &mut Context<NavicatMain>,
) -> Field {
    field().label("文件路径").items_center().child(
        div()
            .w_full()
            .flex()
            .items_center()
            .gap_3()
            .child(
                input_box_light(
                    ConnectionField::SqlitePath,
                    inputs,
                    false,
                    colors,
                    window,
                    cx,
                )
                .flex_1()
                .min_w(px(0.)),
            )
            .child(folder_button_light(colors, cx).flex_none()),
    )
}

fn connection_method_row_light(
    label: &'static str,
    selected_label: &'static str,
    secondary_label: &'static str,
    colors: UiColors,
) -> Field {
    field().label(label).items_center().child(
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(segment_button_light(selected_label, true, colors))
            .child(segment_button_light(secondary_label, false, colors)),
    )
}

fn segment_button_light(label: &'static str, selected: bool, colors: UiColors) -> Div {
    div()
        .h(px(28.))
        .px_3()
        .rounded(colors.radius_lg)
        .border_1()
        .border_color(if selected {
            rgb(0x9aa3af)
        } else {
            colors.border
        })
        .bg(if selected {
            colors.tree_selected
        } else {
            colors.input_bg
        })
        .flex()
        .items_center()
        .justify_center()
        .text_color(if selected { colors.text } else { colors.muted })
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .hover(move |style| style.bg(colors.hover).border_color(rgb(0xaeb7c2)))
        .child(label)
}

fn checkbox_row_light(label: &'static str, _colors: UiColors) -> Field {
    field()
        .label("")
        .items_center()
        .child(Checkbox::new("new-connection-checkbox-srv").label(label))
}

fn dropdown_row_light(label: &'static str, value: &'static str, colors: UiColors) -> Field {
    field().label(label).items_center().child(
        div()
            .w(px(112.))
            .h(px(34.))
            .rounded(colors.radius_lg)
            .border_1()
            .border_color(colors.border)
            .bg(colors.input_bg)
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .text_color(colors.text)
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .hover(move |style| style.bg(colors.hover).border_color(rgb(0xaeb7c2)))
            .child(value)
            .child(app_icon(AppIcon::ChevronDown, 14., colors.muted)),
    )
}

fn color_row_light(selected_color: &str, colors: UiColors, cx: &mut Context<NavicatMain>) -> Field {
    let mut swatches = div().flex().items_center().gap_2();
    for &(hex, value) in CONNECTION_COLOR_PALETTE {
        swatches = swatches.child(color_swatch_light(
            hex,
            rgb(value),
            selected_color == hex,
            colors,
            cx,
        ));
    }

    field().label("颜色").items_center().child(swatches)
}

fn color_swatch_light(
    hex: &'static str,
    color: gpui::Rgba,
    selected: bool,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    div()
        .size(px(26.))
        .rounded_full()
        .border_2()
        .border_color(if selected {
            if colors.is_dark {
                rgb(0xd1d5db)
            } else {
                rgb(0x858b96)
            }
        } else {
            colors.border
        })
        .flex()
        .items_center()
        .justify_center()
        .hover(move |style| style.bg(colors.hover).border_color(color))
        .child(
            div()
                .size(px(18.))
                .rounded_full()
                .bg(color)
                .border_1()
                .border_color(colors.border),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.set_new_connection_color(hex, cx);
                cx.stop_propagation();
            }),
        )
}

fn connection_test_status(status: &ConnectionTestStatus) -> ObservedElement<Stateful<Div>> {
    let (text, fg) = match status {
        ConnectionTestStatus::Success(text) => (text.clone(), rgb(0x16a34a)),
        ConnectionTestStatus::Error(text) => (text.clone(), rgb(0xff0000)),
        ConnectionTestStatus::Pending(text) => (text.clone(), rgb(0x667085)),
    };

    // 测试连接结果按 status 语义暴露给辅助功能与 UI 自动化：
    // 文案作为可访问名称，Role::Status 让读屏与断言都能区分进行中/成功/失败。
    div()
        .id("new-connection-test-status")
        .test_support()
        .role(Role::Status)
        .aria_label(text.clone())
        .h(px(36.))
        .flex()
        .items_center()
        .overflow_hidden()
        .truncate()
        .text_size(px(14.))
        .text_color(fg)
        .child(text)
}

fn input_box_light(
    field: ConnectionField,
    inputs: &NewConnectionInputs,
    secure: bool,
    colors: UiColors,
    window: &mut Window,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let focused = inputs.for_field(field).focus_handle(cx).is_focused(window);
    // 稳定 ElementId 按字段业务身份派生：同一字段在不同页签/数据库类型下仍是同一个控件，
    // UI 自动化与无障碍树都不必依赖 placeholder 文案或列表下标。
    let input = Input::new(inputs.for_field(field))
        .id(connection_field_element_id(field))
        .aria_label(connection_field_label(field))
        .small()
        .appearance(false)
        .focus_bordered(false);

    div()
        .w_full()
        .h(px(34.))
        .rounded(colors.radius_lg)
        .border_1()
        .border_color(if focused {
            if colors.is_dark {
                rgb(0x8ab4ff)
            } else {
                rgb(0x111111)
            }
        } else {
            colors.border
        })
        .bg(colors.input_bg)
        .shadow(vec![box_shadow(
            px(0.),
            px(1.),
            px(4.),
            px(0.),
            hsla(0., 0., 0., if focused { 0.08 } else { 0.11 }),
        )])
        .flex()
        .items_center()
        .overflow_hidden()
        .child(input.w_full().h_full().px_3().text_size(px(14.)))
        .when(secure, |this| this.child(password_eye_button(colors, cx)))
}

fn password_eye_button(colors: UiColors, cx: &mut Context<NavicatMain>) -> Div {
    div()
        .size(px(30.))
        .mr_1()
        .rounded(colors.radius)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .text_color(colors.muted)
        .hover(move |style| style.bg(colors.hover))
        .child(
            div()
                .relative()
                .w(px(16.))
                .h(px(10.))
                .rounded_full()
                .border_1()
                .border_color(colors.muted)
                .flex()
                .items_center()
                .justify_center()
                .child(div().size(px(4.)).rounded_full().bg(colors.muted)),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| {
                this.toggle_new_connection_password_visibility(window, cx);
                cx.stop_propagation();
            }),
        )
}

fn folder_button_light(colors: UiColors, cx: &mut Context<NavicatMain>) -> Div {
    div()
        .size(px(38.))
        .rounded(colors.radius_lg)
        .border_1()
        .border_color(colors.border)
        .bg(colors.input_bg)
        .flex()
        .items_center()
        .justify_center()
        .hover(move |style| style.bg(colors.hover).border_color(rgb(0xaeb7c2)))
        .child(
            div()
                .relative()
                .w(px(18.))
                .h(px(14.))
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left(px(2.))
                        .w(px(8.))
                        .h(px(4.))
                        .rounded(colors.radius * 0.5)
                        .bg(colors.muted),
                )
                .child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .w(px(18.))
                        .h(px(12.))
                        .rounded(colors.radius * 0.5)
                        .border_2()
                        .border_color(colors.muted)
                        .bg(colors.input_bg),
                ),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| {
                this.choose_sqlite_file(window, cx);
                cx.stop_propagation();
            }),
        )
}

fn database_default_port(kind: DatabaseKind) -> &'static str {
    match kind {
        DatabaseKind::MySql => "3306",
        DatabaseKind::TiDb => "4000",
        DatabaseKind::MongoDb => "27017",
        DatabaseKind::Redis => "6379",
        DatabaseKind::Postgres => "5432",
        DatabaseKind::Sqlite => "",
    }
}

fn database_default_port_u16(kind: DatabaseKind) -> u16 {
    database_default_port(kind).parse().unwrap_or(0)
}

fn parse_port(port: &str) -> Result<u16, String> {
    let port = port.trim();
    if port.is_empty() {
        return Err("请填写端口".to_string());
    }
    port.parse::<u16>()
        .map_err(|_| "端口必须是 1-65535 的数字".to_string())
}

fn non_empty_option(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn first_connection_field(kind: DatabaseKind) -> ConnectionField {
    connection_fields(kind)[0]
}

fn connection_fields(kind: DatabaseKind) -> &'static [ConnectionField] {
    match kind {
        DatabaseKind::MySql | DatabaseKind::TiDb => &[
            ConnectionField::Name,
            ConnectionField::Host,
            ConnectionField::Port,
            ConnectionField::Username,
            ConnectionField::Password,
            ConnectionField::Database,
            ConnectionField::UrlParams,
        ],
        DatabaseKind::Sqlite => &[ConnectionField::Name, ConnectionField::SqlitePath],
        DatabaseKind::MongoDb => &[
            ConnectionField::Name,
            ConnectionField::Host,
            ConnectionField::Port,
            ConnectionField::Username,
            ConnectionField::Password,
            ConnectionField::MongoDefaultDb,
            ConnectionField::MongoAuthDb,
        ],
        DatabaseKind::Redis => &[
            ConnectionField::Name,
            ConnectionField::Host,
            ConnectionField::Port,
            ConnectionField::Username,
            ConnectionField::Password,
            ConnectionField::UrlParams,
        ],
        DatabaseKind::Postgres => &[
            ConnectionField::Name,
            ConnectionField::Host,
            ConnectionField::Port,
            ConnectionField::Username,
            ConnectionField::Password,
            ConnectionField::Database,
            ConnectionField::UrlParams,
        ],
    }
}

/// 文本字段输入框的稳定 `ElementId`，按字段业务身份命名（如 `connection-name`）。
///
/// 与 placeholder 文案解耦：文案面向用户可随时调整，ID 面向控件定位必须稳定。
fn connection_field_element_id(field: ConnectionField) -> &'static str {
    match field {
        ConnectionField::Name => "connection-name",
        ConnectionField::Host => "connection-host",
        ConnectionField::Port => "connection-port",
        ConnectionField::Username => "connection-username",
        ConnectionField::Password => "connection-password",
        ConnectionField::Database => "connection-database",
        ConnectionField::UrlParams => "connection-url-params",
        ConnectionField::SqlitePath => "connection-sqlite-path",
        ConnectionField::MongoDefaultDb => "connection-mongo-default-db",
        ConnectionField::MongoAuthDb => "connection-mongo-auth-db",
        ConnectionField::TlsCa => "connection-tls-ca",
        ConnectionField::TlsClientCert => "connection-tls-client-cert",
        ConnectionField::TlsClientKey => "connection-tls-client-key",
        ConnectionField::TlsSni => "connection-tls-sni",
        ConnectionField::SshHost => "connection-ssh-host",
        ConnectionField::SshPort => "connection-ssh-port",
        ConnectionField::SshUsername => "connection-ssh-username",
        ConnectionField::SshPassword => "connection-ssh-password",
        ConnectionField::SshPrivateKey => "connection-ssh-private-key",
        ConnectionField::SshPassphrase => "connection-ssh-passphrase",
        ConnectionField::SentinelMasterName => "connection-sentinel-master-name",
        ConnectionField::SentinelEndpoints => "connection-sentinel-endpoints",
        ConnectionField::ClusterStartNodes => "connection-cluster-start-nodes",
        ConnectionField::CloudSubscription => "connection-cloud-subscription",
        ConnectionField::CloudResource => "connection-cloud-resource",
        ConnectionField::DiscoveryUri => "connection-discovery-uri",
        ConnectionField::PgDefaultSchema => "connection-pg-default-schema",
        ConnectionField::PgApplicationName => "connection-pg-application-name",
        ConnectionField::PgConnectTimeoutSecs => "connection-pg-connect-timeout-secs",
        ConnectionField::PgQueryTimeoutSecs => "connection-pg-query-timeout-secs",
        ConnectionField::MysqlCharset => "connection-mysql-charset",
        ConnectionField::MysqlProxyType => "connection-mysql-proxy-type",
        ConnectionField::MysqlSshConnectTimeout => "connection-mysql-ssh-connect-timeout",
        ConnectionField::MysqlSshKeepalive => "connection-mysql-ssh-keepalive",
        ConnectionField::MysqlProxyHost => "connection-mysql-proxy-host",
        ConnectionField::MysqlProxyPort => "connection-mysql-proxy-port",
        ConnectionField::MysqlProxyUsername => "connection-mysql-proxy-username",
        ConnectionField::MysqlProxyPassword => "connection-mysql-proxy-password",
        ConnectionField::MysqlConnectTimeout => "connection-mysql-connect-timeout",
        ConnectionField::MysqlQueryTimeout => "connection-mysql-query-timeout",
        ConnectionField::MysqlIdleTtl => "connection-mysql-idle-ttl",
    }
}

/// 文本字段输入框的可访问名称。
///
/// 密码类字段不暴露标签以外的信息，`Input` 的 masked 语义已保证取值不进无障碍树；
/// 这里只给字段名，不拼接当前值。
fn connection_field_label(field: ConnectionField) -> &'static str {
    match field {
        ConnectionField::Name => "连接名称",
        ConnectionField::Host => "主机",
        ConnectionField::Port => "端口",
        ConnectionField::Username => "用户名",
        ConnectionField::Password => "密码",
        ConnectionField::Database => "数据库名",
        ConnectionField::UrlParams => "URL 参数",
        ConnectionField::SqlitePath => "SQLite 文件路径",
        ConnectionField::MongoDefaultDb => "默认数据库",
        ConnectionField::MongoAuthDb => "认证数据库",
        ConnectionField::TlsCa => "CA 证书文件路径",
        ConnectionField::TlsClientCert => "客户端证书文件路径",
        ConnectionField::TlsClientKey => "客户端私钥文件路径",
        ConnectionField::TlsSni => "SNI",
        ConnectionField::SshHost => "SSH 主机",
        ConnectionField::SshPort => "SSH 端口",
        ConnectionField::SshUsername => "SSH 用户名",
        ConnectionField::SshPassword => "SSH 密码",
        ConnectionField::SshPrivateKey => "SSH 私钥文件路径",
        ConnectionField::SshPassphrase => "SSH 私钥口令",
        ConnectionField::SentinelMasterName => "Sentinel 主库名",
        ConnectionField::SentinelEndpoints => "Sentinel 节点列表",
        ConnectionField::ClusterStartNodes => "Cluster 起始节点列表",
        ConnectionField::CloudSubscription => "云订阅 / 账号 ID",
        ConnectionField::CloudResource => "云资源 / 数据库标识",
        ConnectionField::DiscoveryUri => "连接串导入",
        ConnectionField::PgDefaultSchema => "默认 schema",
        ConnectionField::PgApplicationName => "应用名",
        ConnectionField::PgConnectTimeoutSecs => "建连超时（秒）",
        ConnectionField::PgQueryTimeoutSecs => "查询超时（秒）",
        ConnectionField::MysqlCharset => "连接字符集",
        ConnectionField::MysqlProxyType => "代理类型",
        ConnectionField::MysqlSshConnectTimeout => "SSH 连接超时（秒）",
        ConnectionField::MysqlSshKeepalive => "SSH 心跳间隔（秒）",
        ConnectionField::MysqlProxyHost => "代理主机",
        ConnectionField::MysqlProxyPort => "代理端口",
        ConnectionField::MysqlProxyUsername => "代理用户名",
        ConnectionField::MysqlProxyPassword => "代理密码",
        ConnectionField::MysqlConnectTimeout => "建连超时（秒）",
        ConnectionField::MysqlQueryTimeout => "查询超时（秒）",
        ConnectionField::MysqlIdleTtl => "连接空闲 TTL（秒）",
    }
}

fn connection_field_placeholder(field: ConnectionField) -> &'static str {
    match field {
        ConnectionField::Name => "连接名称",
        ConnectionField::Host => "127.0.0.1",
        ConnectionField::Port => "端口",
        ConnectionField::Username => "用户名",
        ConnectionField::Password => "可选",
        ConnectionField::Database => "数据库名",
        ConnectionField::MongoDefaultDb => "可选",
        ConnectionField::UrlParams => "key=value&key2=value2（Redis: tls=true&sentinel_master=mymaster）",
        ConnectionField::SqlitePath => "/path/to/database.db or :memory:",
        ConnectionField::MongoAuthDb => "可选，通常为 admin",
        ConnectionField::TlsCa => "CA 证书文件路径",
        ConnectionField::TlsClientCert => "客户端证书文件路径",
        ConnectionField::TlsClientKey => "客户端私钥文件路径",
        ConnectionField::TlsSni => "服务器名指示（SNI），可选",
        ConnectionField::SshHost => "跳板机主机",
        ConnectionField::SshPort => "22",
        ConnectionField::SshUsername => "SSH 用户名",
        ConnectionField::SshPassword => "SSH 密码",
        ConnectionField::SshPrivateKey => "私钥文件路径",
        ConnectionField::SshPassphrase => "私钥口令，可选",
        // —— PostgreSQL ——
        ConnectionField::PgDefaultSchema => "默认 schema，可选（如 public）",
        ConnectionField::PgApplicationName => "应用名，可选",
        ConnectionField::PgConnectTimeoutSecs => "5",
        ConnectionField::PgQueryTimeoutSecs => "0=不设限（statement_timeout）",
        // —— MySQL / TiDB ——
        ConnectionField::MysqlCharset => "utf8mb4",
        ConnectionField::MysqlProxyType => "socks5 (socks5/http_connect)",
        ConnectionField::MysqlSshConnectTimeout => "0=继承全局",
        ConnectionField::MysqlSshKeepalive => "0=不发送",
        ConnectionField::MysqlProxyHost => "代理主机",
        ConnectionField::MysqlProxyPort => "代理端口",
        ConnectionField::MysqlProxyUsername => "可选",
        ConnectionField::MysqlProxyPassword => "可选",
        ConnectionField::MysqlConnectTimeout => "5",
        ConnectionField::MysqlQueryTimeout => "0=不设限",
        ConnectionField::MysqlIdleTtl => "0=不回收",
        ConnectionField::SentinelMasterName => "Sentinel 主库名",
        ConnectionField::SentinelEndpoints => "host:port, host:port",
        ConnectionField::ClusterStartNodes => "host:port, host:port",
        ConnectionField::CloudSubscription => "订阅 / 账号 ID",
        ConnectionField::CloudResource => "资源 / 数据库名",
        ConnectionField::DiscoveryUri => "redis:// 或 rediss:// 连接串",
    }
}

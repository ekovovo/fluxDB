// 调用前安全审计的规则集与求值（设计 §25.1）。
//
// 定位：这是内核强制的 SecurityAuditHook 的**规则内容**，不是权限系统本身。
// 授权（工具开关/Agent 白名单/连接读写）与审批方式（ExecutionPolicy）在别处决定
// 「能不能」与「要不要人工点」；这里只回答「这一次的具体计划有没有命中已声明的禁止项」。
//
// 不变量：
// - **Deny 优先**：任一规则判 Deny，结果就是 Deny，插件 Allow 翻不了案；
// - **严格度合并**：多条规则命中时取最严格者（Deny > RequireApproval > Allow）；
// - **默认不拦**：规则集为空时返回 Allow；规则只增加约束，不放宽 `authorize`；
// - **只读输入**：规则看到规范化参数与目标，看不到凭据、密文或数据样本；
// - **白名单 fail closed**：配了白名单但语句无法解析时必须拒绝，不能因为"读不出表名"就放行。
//
// 规则来源是插件自身的配置声明（协议见 `PluginConfigDeclaration`）：
// `builtin.security-audit` 声明标量字段，用户在「管理」弹框里填写，值落在该实例的
// `ai_plugin_instances.config_json`。新增一条规则 = 改声明 + 加一个求值分支，
// 不改权限链、审批链与存储结构。

/// 规则字段键：与安全审计插件的配置声明一一对应，改名等同协议不兼容变更。
pub const RULE_DENIED_TABLES: &str = "denied_tables";
pub const RULE_ALLOWED_TABLES: &str = "allowed_tables";
pub const RULE_PROTECTED_TABLES: &str = "protected_tables";
pub const RULE_DENIED_COLUMNS: &str = "denied_columns";
pub const RULE_FORBID_DDL: &str = "forbid_ddl";
pub const RULE_WRITE_REQUIRES_WHERE: &str = "write_requires_where";
pub const RULE_DENIED_REDIS_COMMANDS: &str = "denied_redis_commands";
pub const RULE_DENIED_REDIS_KEY_PREFIXES: &str = "denied_redis_key_prefixes";

/// 一次求值的结论；`rule_id` 只用于审计事实，不参与判定。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleVerdict {
    pub outcome: AuditOutcome,
    /// 命中的规则 ID（机器键）；没有命中时为 `None`。
    pub rule_id: Option<&'static str>,
    /// 可见原因码（不含参数原文），写入审计行的 `reason_codes`。
    pub reason_code: Option<String>,
}

impl RuleVerdict {
    fn allow() -> Self {
        Self {
            outcome: AuditOutcome::Allow,
            rule_id: None,
            reason_code: None,
        }
    }

    fn matched(outcome: AuditOutcome, rule_id: &'static str, reason_code: String) -> Self {
        Self {
            outcome,
            rule_id: Some(rule_id),
            reason_code: Some(reason_code),
        }
    }
}

/// 规则集：全部来自插件配置，**缺省全部为空/关闭**（默认不拦任何调用）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuditRules {
    /// 禁止触碰的表（读与写都拒）；裸名匹配任意 schema，`schema.table` 要求两者都相等。
    pub denied_tables: Vec<String>,
    /// 只允许触碰的表；为空表示不限制。命中白名单之外的任何表即拒。
    pub allowed_tables: Vec<String>,
    /// 禁止修改（写工具/写语句）但允许读取的表。
    pub protected_tables: Vec<String>,
    /// 禁止出现的列；`table.column` 要求表名相符，裸列名匹配任意表。
    pub denied_columns: Vec<String>,
    /// 拒绝结构变更（CREATE/ALTER/DROP/TRUNCATE/GRANT 等）。
    pub forbid_ddl: bool,
    /// 没有 WHERE 的 UPDATE/DELETE 转为需审批（不直接拒绝）。
    pub write_requires_where: bool,
    /// 禁止的 Redis 命令名。
    pub denied_redis_commands: Vec<String>,
    /// 禁止触碰的 Redis Key 前缀。
    pub denied_redis_key_prefixes: Vec<String>,
}

impl AuditRules {
    /// 规则集版本：由规则内容推导的稳定指纹。
    ///
    /// 用途是让审计事实能区分"旧规则放的"与"新规则放的"；改任何一条规则都会换版本。
    /// 与 `plan_hash` 同理只用于变化检测，不作为安全哈希。
    pub fn version(&self) -> String {
        let material = format!(
            "v1\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.denied_tables.join(","),
            self.allowed_tables.join(","),
            self.protected_tables.join(","),
            self.denied_columns.join(","),
            self.forbid_ddl,
            self.write_requires_where,
            self.denied_redis_commands.join(","),
            self.denied_redis_key_prefixes.join(","),
        );
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in material.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("kernel-audit-v1-{hash:016x}")
    }

    /// 规则集是否为空：为空时求值直接返回 Allow，不解析参数。
    pub fn is_empty(&self) -> bool {
        self.denied_tables.is_empty()
            && self.allowed_tables.is_empty()
            && self.protected_tables.is_empty()
            && self.denied_columns.is_empty()
            && !self.forbid_ddl
            && !self.write_requires_where
            && self.denied_redis_commands.is_empty()
            && self.denied_redis_key_prefixes.is_empty()
    }

    /// 从 `ai_plugin_instances.config_json` 的字段值构造规则集。
    ///
    /// 类型不符的取值按"未配置"处理而不是报错：声明已经约束过类型，运行期再遇到脏值
    /// 只说明配置是手改的，此时低估规则比让整条链路失败更安全（规则只会更宽松，
    /// 真正的硬边界仍在 `authorize` 与连接权限）。
    pub fn from_config(values: &serde_json::Map<String, serde_json::Value>) -> Self {
        Self {
            denied_tables: text_list(values, RULE_DENIED_TABLES),
            allowed_tables: text_list(values, RULE_ALLOWED_TABLES),
            protected_tables: text_list(values, RULE_PROTECTED_TABLES),
            denied_columns: text_list(values, RULE_DENIED_COLUMNS),
            forbid_ddl: switch_value(values, RULE_FORBID_DDL),
            write_requires_where: switch_value(values, RULE_WRITE_REQUIRES_WHERE),
            denied_redis_commands: text_list(values, RULE_DENIED_REDIS_COMMANDS),
            denied_redis_key_prefixes: text_list(values, RULE_DENIED_REDIS_KEY_PREFIXES),
        }
    }

    /// 求值一次调用。
    ///
    /// 只按 `tool_id` 分流：SQL 工具看 SQL 规则，Redis 工具看 Redis 规则，
    /// 其余工具（文件/产物/上下文）没有规则命中，保持放行。
    pub fn evaluate(&self, input: &AuditInput) -> RuleVerdict {
        if self.is_empty() {
            return RuleVerdict::allow();
        }
        let arguments: serde_json::Value =
            match serde_json::from_str(&input.normalized_arguments_json) {
                Ok(value) => value,
                // 参数不是 JSON 对象时给不出结论；这里不拒绝，交给上游 schema 校验兜底。
                Err(_) => return RuleVerdict::allow(),
            };
        if input.tool_id.starts_with("redis.") {
            return self.evaluate_redis(input, &arguments);
        }
        if matches!(
            input.tool_id.as_str(),
            "db.query" | "db.execute" | "db.explain"
        ) {
            return self.evaluate_sql(input, &arguments);
        }
        RuleVerdict::allow()
    }

    /// Redis 工具：按命令名与 Key 前缀判定。
    fn evaluate_redis(&self, input: &AuditInput, arguments: &serde_json::Value) -> RuleVerdict {
        let mut strictest = RuleVerdict::allow();
        // `redis.inspect` 用 `key`，`redis.scan` 用 `pattern`，`redis.execute` 用 `argv`。
        // 命令名与 Key 都从参数取，不用工具名反推。
        let argv = arguments
            .get("argv")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !argv.is_empty() && !self.denied_redis_commands.is_empty() {
            if let Some(command) = argv.first() {
                let normalized = command.to_ascii_lowercase();
                if self
                    .denied_redis_commands
                    .iter()
                    .any(|denied| denied.trim().eq_ignore_ascii_case(&normalized))
                {
                    strictest = strictest.merge(RuleVerdict::matched(
                        AuditOutcome::Deny,
                        RULE_DENIED_REDIS_COMMANDS,
                        format!("denied_command:{normalized}"),
                    ));
                }
            }
        }
        if !self.denied_redis_key_prefixes.is_empty() {
            let keys = if argv.is_empty() {
                arguments
                    .get("key")
                    .and_then(|value| value.as_str())
                    .map(|key| vec![key.to_string()])
                    .unwrap_or_default()
            } else {
                match crate::extract_redis_command_keys(&argv) {
                    crate::RedisKeyExtraction::Keys(keys) => keys,
                    // 无法静态解析 Key 的命令不在这里拒绝：范围核验在 Redis 工具内完成，
                    // 审计只对自己能看见的 Key 施加前缀规则。
                    crate::RedisKeyExtraction::NoKeys
                    | crate::RedisKeyExtraction::Unresolvable { .. } => Vec::new(),
                }
            };
            if let Some(hit) = keys.iter().find(|key| {
                self.denied_redis_key_prefixes
                    .iter()
                    .any(|prefix| !prefix.is_empty() && key.starts_with(prefix.as_str()))
            }) {
                // 只记录命中了哪个前缀，不把 Key 名写进审计事实。
                let prefix = self
                    .denied_redis_key_prefixes
                    .iter()
                    .find(|prefix| hit.starts_with(prefix.as_str()))
                    .cloned()
                    .unwrap_or_default();
                strictest = strictest.merge(RuleVerdict::matched(
                    AuditOutcome::Deny,
                    RULE_DENIED_REDIS_KEY_PREFIXES,
                    format!("denied_key_prefix:{prefix}"),
                ));
            }
        }
        let _ = input;
        strictest
    }

    /// SQL 工具：按表/列白黑名单、DDL 与无 WHERE 写入判定。
    fn evaluate_sql(&self, input: &AuditInput, arguments: &serde_json::Value) -> RuleVerdict {
        let Some(sql) = arguments.get("sql").and_then(|value| value.as_str()) else {
            return RuleVerdict::allow();
        };
        let mut strictest = RuleVerdict::allow();
        let parsed = parse_sql_shape(sql);
        match parsed {
            Some(shape) => {
                if self.forbid_ddl && shape.is_ddl {
                    strictest = strictest.merge(RuleVerdict::matched(
                        AuditOutcome::Deny,
                        RULE_FORBID_DDL,
                        "ddl_forbidden".to_string(),
                    ));
                }
                let is_write = shape.is_write;
                if let Some(table) = shape
                    .tables
                    .iter()
                    .find(|table| matches_table(&self.denied_tables, table))
                {
                    strictest = strictest.merge(RuleVerdict::matched(
                        AuditOutcome::Deny,
                        RULE_DENIED_TABLES,
                        format!("denied_table:{}", table.display()),
                    ));
                }
                if is_write {
                    if let Some(table) = shape
                        .tables
                        .iter()
                        .find(|table| matches_table(&self.protected_tables, table))
                    {
                        strictest = strictest.merge(RuleVerdict::matched(
                            AuditOutcome::Deny,
                            RULE_PROTECTED_TABLES,
                            format!("protected_table_write:{}", table.display()),
                        ));
                    }
                }
                if let Some(column) = shape
                    .columns
                    .iter()
                    .find(|column| matches_column(&self.denied_columns, column))
                {
                    strictest = strictest.merge(RuleVerdict::matched(
                        AuditOutcome::Deny,
                        RULE_DENIED_COLUMNS,
                        format!("denied_column:{}", column.display()),
                    ));
                }
                if is_write && self.write_requires_where && !shape.has_predicate {
                    strictest = strictest.merge(RuleVerdict::matched(
                        AuditOutcome::RequireApproval,
                        RULE_WRITE_REQUIRES_WHERE,
                        "write_without_predicate".to_string(),
                    ));
                }
            }
            // 解析不出来：白名单规则无法核实，按 fail closed 拒绝；没有白名单时给不出结论，
            // 保持放行（多语句/事务控制等结构性拒绝由 SQL 策略层负责）。
            None => {
                if !self.allowed_tables.is_empty() {
                    strictest = strictest.merge(RuleVerdict::matched(
                        AuditOutcome::Deny,
                        RULE_ALLOWED_TABLES,
                        "unparsable_sql_against_allowlist".to_string(),
                    ));
                }
            }
        }
        if !self.allowed_tables.is_empty() {
            if let Some(shape) = parse_sql_shape(sql) {
                if let Some(table) = shape
                    .tables
                    .iter()
                    .find(|table| !matches_table(&self.allowed_tables, table))
                {
                    strictest = strictest.merge(RuleVerdict::matched(
                        AuditOutcome::Deny,
                        RULE_ALLOWED_TABLES,
                        format!("table_outside_allowlist:{}", table.display()),
                    ));
                }
            }
        }
        let _ = input;
        strictest
    }
}

impl RuleVerdict {
    /// 取更严格的一方：Deny > RequireApproval > Allow。
    fn merge(self, other: RuleVerdict) -> RuleVerdict {
        fn rank(outcome: AuditOutcome) -> u8 {
            match outcome {
                AuditOutcome::Allow => 0,
                AuditOutcome::RequireApproval => 1,
                AuditOutcome::Deny => 2,
            }
        }
        if rank(other.outcome) > rank(self.outcome) {
            other
        } else {
            self
        }
    }
}

/// 解析出的语句形状：判定只需要这几个事实，保留原始 SQL 文本。
struct SqlShape {
    is_write: bool,
    is_ddl: bool,
    has_predicate: bool,
    tables: Vec<SqlObject>,
    columns: Vec<SqlObject>,
}

/// 一个表/列引用：`qualifier` 是沿名前缀（schema 或表名），`name` 是自身名字。
#[derive(Clone, Debug)]
struct SqlObject {
    qualifier: Option<String>,
    name: String,
}

impl SqlObject {
    /// 可见文案：只用于原因码，**不含参数值**，因此不构成数据泄漏面。
    fn display(&self) -> String {
        match &self.qualifier {
            Some(qualifier) => format!("{qualifier}.{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// 用通用方言解析语句形状；解析失败返回 `None`。
///
/// 这里刻意不用具体方言：规则只关心"引用了哪些表/列、是不是写、有没有 WHERE"，
/// 这些在通用方言下已能稳定识别；具体方言的严格校验由 SQL 策略层负责。
fn parse_sql_shape(sql: &str) -> Option<SqlShape> {
    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;
    use std::ops::ControlFlow;

    let statements = Parser::parse_sql(&GenericDialect {}, sql).ok()?;
    if statements.len() != 1 {
        return None;
    }
    let statement = &statements[0];
    let is_ddl = matches!(
        statement,
        Statement::CreateTable(_)
            | Statement::CreateIndex(_)
            | Statement::CreateView { .. }
            | Statement::CreateVirtualTable { .. }
            | Statement::CreateRole(_)
            | Statement::CreatePolicy(_)
            | Statement::AlterTable(_)
            | Statement::AlterIndex { .. }
            | Statement::AlterView { .. }
            | Statement::Drop { .. }
            | Statement::Truncate(_)
            | Statement::Grant(_)
            | Statement::Revoke(_)
    );
    let is_write = is_ddl
        || matches!(
            statement,
            Statement::Insert(_)
                | Statement::Update(_)
                | Statement::Delete(_)
                | Statement::Merge { .. }
        );

    // 谓词只看写语句的 WHERE：`UPDATE t SET a=1` 与 `DELETE FROM t` 没有谓词，
    // 影响面无法从语句本身界定。
    let has_predicate = match statement {
        Statement::Update(update) => update.selection.is_some(),
        Statement::Delete(delete) => delete.selection.is_some(),
        _ => true,
    };

    let mut tables = Vec::new();
    let _ = sqlparser::ast::visit_relations(statement, |name| {
        if let Some(object) = object_from_name(name) {
            tables.push(object);
        }
        ControlFlow::<()>::Continue(())
    });

    let mut columns = Vec::new();
    let _ = sqlparser::ast::visit_expressions(statement, |expr| {
        match expr {
            Expr::Identifier(ident) => columns.push(SqlObject {
                qualifier: None,
                name: ident.value.clone(),
            }),
            Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
                let name = parts[parts.len() - 1].value.clone();
                let qualifier = parts[parts.len() - 2].value.clone();
                columns.push(SqlObject {
                    qualifier: Some(qualifier),
                    name,
                });
            }
            _ => {}
        }
        ControlFlow::<()>::Continue(())
    });
    dedup_objects(&mut tables);
    dedup_objects(&mut columns);

    Some(SqlShape {
        is_write,
        is_ddl,
        has_predicate,
        tables,
        columns,
    })
}

/// 从限定名取出「表名 + 沿名前缀（schema 或表名）」。
fn object_from_name(name: &ObjectName) -> Option<SqlObject> {
    let parts = name
        .0
        .iter()
        .filter_map(|part| match part {
            ObjectNamePart::Identifier(ident) => Some(ident.value.clone()),
            ObjectNamePart::Function(_) => None,
        })
        .collect::<Vec<_>>();
    let table = parts.last()?.clone();
    let qualifier = if parts.len() >= 2 {
        parts.get(parts.len() - 2).cloned()
    } else {
        None
    };
    Some(SqlObject {
        qualifier,
        name: table,
    })
}

fn dedup_objects(objects: &mut Vec<SqlObject>) {
    let mut seen = BTreeSet::new();
    objects.retain(|object| seen.insert(object.display().to_ascii_lowercase()));
}

/// 表是否命中规则列表：`schema.table` 要求 schema 与表名都相等，裸名匹配任意 schema。
fn matches_table(patterns: &[String], table: &SqlObject) -> bool {
    patterns.iter().any(|pattern| {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return false;
        }
        match pattern.split_once('.') {
            Some((schema, name)) => {
                table.name.eq_ignore_ascii_case(name)
                    && table
                        .qualifier
                        .as_deref()
                        .is_some_and(|qualifier| qualifier.eq_ignore_ascii_case(schema))
            }
            None => table.name.eq_ignore_ascii_case(pattern),
        }
    })
}

/// 列是否命中规则列表：`table.column` 要求限定名相等，裸名匹配任意限定名。
fn matches_column(patterns: &[String], column: &SqlObject) -> bool {
    patterns.iter().any(|pattern| {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return false;
        }
        match pattern.split_once('.') {
            Some((qualifier, name)) => {
                column.name.eq_ignore_ascii_case(name)
                    && column
                        .qualifier
                        .as_deref()
                        .is_some_and(|value| value.eq_ignore_ascii_case(qualifier))
            }
            None => column.name.eq_ignore_ascii_case(pattern),
        }
    })
}

/// 读取 `text_list` 字段：只接受字符串数组，逐项去空白、去空、去重。
fn text_list(values: &serde_json::Map<String, serde_json::Value>, key: &str) -> Vec<String> {
    let Some(items) = values.get(key).and_then(|value| value.as_array()) else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    items
        .iter()
        .filter_map(|item| item.as_str())
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .filter(|item| seen.insert(item.to_ascii_lowercase()))
        .map(str::to_string)
        .collect()
}

/// 读取 `switch` 字段：只有字面量 `"true"` 才算开启，其余（含缺失/脏值）按关闭。
fn switch_value(values: &serde_json::Map<String, serde_json::Value>, key: &str) -> bool {
    values
        .get(key)
        .and_then(|value| value.as_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

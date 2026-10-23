#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DataFilterPopoverKind {
    Field,
    Operator,
    Value,
    SortField,
    SortMenu,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DataFilterPopover {
    tab_id: TabId,
    kind: DataFilterPopoverKind,
    rule_index: Option<usize>,
    sort_index: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
struct DataFilterRule {
    enabled: bool,
    field: Option<String>,
    operator: DataFilterOperator,
    /// 有序值：BETWEEN 的上下限不能被排序交换；IN 列表也保持用户输入顺序。
    values: Vec<String>,
    /// 输入模式中无法无损拆成字段/运算符/值的表达式文本。
    /// 该行只读，避免切换模式时静默改变语义。
    advanced_expression: Option<String>,
    /// 从输入表达式解析出的原始类型。表单编辑值后丢弃，避免把用户新输入的
    /// `"007"` 错误保留成整数类型。
    typed_values: Option<Vec<fluxdb_core::FilterValue>>,
    /// 本行与下一行之间的组合符；最后一行的值不参与表达式。
    connector: fluxdb_core::FilterCombinator,
    grouped: bool,
}

/// 值编辑器草稿：打开「值」弹层时从条件快照初始化；「确定」才写回条件。
/// 字段切换、异步建议值请求返回都会校验 `field` 是否仍是本草稿目标字段，
/// 避免旧字段的选择或请求结果覆盖新字段。
#[derive(Clone, Debug, Eq, PartialEq)]
struct DataFilterValueDraft {
    tab_id: TabId,
    rule_index: usize,
    /// 打开弹层时的目标字段快照，用于字段切换后的身份/竞态校验。
    field: String,
    /// 打开弹层时的运算符快照，用于确认时校验值形态兼容性。
    operator: DataFilterOperator,
    values: Vec<String>,
    /// 单值输入框文案（保留用户正在输入、尚未提交的文本）。
    input: String,
    /// 批量粘贴区是否展开。
    batch_open: bool,
    /// 批量粘贴区多行文本内容。
    batch_text: String,
    /// 批量粘贴分隔方式。
    batch_separator: BatchSeparator,
}

/// 批量粘贴的分隔方式。默认每行一个值；明确选择时才按逗号/制表符拆分，
/// 不假装支持完整 CSV 引号规则。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum BatchSeparator {
    #[default]
    Newline,
    Comma,
    Tab,
}

impl BatchSeparator {
    fn label(self) -> SharedString {
        match self {
            Self::Newline => tr("app.filter.separator_newline"),
            Self::Comma => tr("app.filter.separator_comma"),
            Self::Tab => tr("app.filter.separator_tab"),
        }
    }
}

/// 按选定的分隔方式切分批量粘贴文本。
///
/// 规则必须明确：默认每行一个值；明确选择时才按逗号/制表符拆分。
/// 这里不做完整 CSV 引号解析（不伪装支持带引号/转义的 CSV），
/// 只保留原始片段，交由调用方做 trim 去空与去重。
fn split_filter_batch_text(text: &str, separator: BatchSeparator) -> Vec<&str> {
    match separator {
        BatchSeparator::Newline => text.split('\n').collect(),
        BatchSeparator::Comma => text.split(',').collect(),
        BatchSeparator::Tab => text.split('\t').collect(),
    }
}

/// 运算符还能否再加入一个值。
///
/// 上限由值形态决定：无值 0、单值 1、区间 2（介于的上下限），
/// 列表（IN / NOT IN）不限数量。
fn data_filter_rule_can_add_more_value(operator: DataFilterOperator, current_len: usize) -> bool {
    operator.max_values().is_none_or(|max| current_len < max)
}

/// 从批量片段解析出的待添加值：trim、忽略空行，纯逻辑便于单测。
fn filter_batch_values_to_add(
    text: &str,
    separator: BatchSeparator,
    selected: &[String],
) -> (Vec<String>, usize, usize) {
    let mut added = Vec::new();
    let mut duplicated = 0usize;
    let mut ignored_empty = 0usize;
    for part in split_filter_batch_text(text, separator) {
        let trimmed = part.trim();
        // 忽略空行；保留字符串中有意义的内部空格/引号/逗号，不擅自改写。
        if trimmed.is_empty() {
            ignored_empty += 1;
            continue;
        }
        let value = trimmed.to_string();
        if selected.contains(&value) {
            duplicated += 1;
        } else {
            added.push(value);
        }
    }
    (added, duplicated, ignored_empty)
}

/// 切换字段后清空旧字段值：旧字段的值对本新字段不再有意义（状态残留根因修复）。
/// 返回 (新规则, 是否曾有过旧值)。
fn data_filter_rule_after_field_switch(
    field: String,
    mut rule: DataFilterRule,
) -> (DataFilterRule, bool) {
    rule.field = Some(field);
    let had_values = !rule.values.is_empty();
    rule.values.clear();
    rule.typed_values = None;
    (rule, had_values)
}

/// 按目标运算符的值形态收敛已有值：列表保留全部，单值/区间只保留前 1/2 个，
/// 无值运算符清空——旧值会以错误的 OR 组合或多余的 BETWEEN 参数参与查询。
/// 单值之间互切（`=` 改 `!=`）保留同一个值，避免改运算符就丢输入。
fn data_filter_values_for_arity(
    values: &[String],
    operator: DataFilterOperator,
) -> Vec<String> {
    match operator.max_values() {
        Some(max) => values.iter().take(max).cloned().collect(),
        None => values.to_vec(),
    }
}

fn data_filter_rule_after_operator_switch(
    operator: DataFilterOperator,
    mut rule: DataFilterRule,
) -> DataFilterRule {
    rule.values = data_filter_values_for_arity(&rule.values, operator);
    rule.typed_values = rule.typed_values.as_ref().map(|values| {
        match operator.max_values() {
            Some(max) => values.iter().take(max).cloned().collect(),
            None => values.clone(),
        }
    });
    rule.operator = operator;
    rule
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DataSortRule {
    enabled: bool,
    field: String,
    ascending: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DataFilterMode {
    Builder,
    Text,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedDataEditorSqlText {
    filter_text: String,
    sort_text: String,
    limit: Option<u64>,
}

#[derive(Clone, Debug, Default)]
struct SqlTextSelection {
    text: String,
    anchor: usize,
    cursor: usize,
    selecting: bool,
    bounds: Option<Bounds<Pixels>>,
}

impl SqlTextSelection {
    fn selected_range(&self) -> Option<(usize, usize)> {
        let start = self.anchor.min(self.cursor);
        let end = self.anchor.max(self.cursor);
        (start < end).then_some((start, end))
    }

    fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selected_range()?;
        self.text.get(start..end).map(str::to_string)
    }
}

impl Default for DataFilterRule {
    fn default() -> Self {
        Self {
            enabled: true,
            field: None,
            operator: DataFilterOperator::Contains,
            values: Vec::new(),
            advanced_expression: None,
            typed_values: None,
            connector: fluxdb_core::FilterCombinator::And,
            grouped: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DataFilterValueArity {
    /// 不需要值（是 null / 是空的…），不显示值槽。
    None,
    /// 恰好一个值（=、!=、<、包含…）：值槽就地输入，不用多值弹层。
    Single,
    /// 两个值构成区间（介于 / 不介于），用值弹层但最多两个值。
    Range,
    /// 任意多个值（在列表 / 不在列表），用多选值弹层。
    Many,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DataFilterOperator {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Contains,
    NotContains,
    StartsWith,
    NotStartsWith,
    EndsWith,
    NotEndsWith,
    IsNull,
    IsNotNull,
    IsEmpty,
    IsNotEmpty,
    Between,
    NotBetween,
    InList,
    NotInList,
}

impl DataFilterOperator {
    fn label(self) -> SharedString {
        match self {
            Self::Eq => tr("app.filter.op_eq"),
            Self::Ne => tr("app.filter.op_ne"),
            Self::Lt => tr("app.filter.op_lt"),
            Self::Le => tr("app.filter.op_le"),
            Self::Gt => tr("app.filter.op_gt"),
            Self::Ge => tr("app.filter.op_ge"),
            Self::Contains => tr("app.filter.op_contains"),
            Self::NotContains => tr("app.filter.op_not_contains"),
            Self::StartsWith => tr("app.filter.op_starts_with"),
            Self::NotStartsWith => tr("app.filter.op_not_starts_with"),
            Self::EndsWith => tr("app.filter.op_ends_with"),
            Self::NotEndsWith => tr("app.filter.op_not_ends_with"),
            Self::IsNull => tr("app.filter.op_is_null"),
            Self::IsNotNull => tr("app.filter.op_is_not_null"),
            Self::IsEmpty => tr("app.filter.op_is_empty"),
            Self::IsNotEmpty => tr("app.filter.op_is_not_empty"),
            Self::Between => tr("app.filter.op_between"),
            Self::NotBetween => tr("app.filter.op_not_between"),
            Self::InList => tr("app.filter.op_in_list"),
            Self::NotInList => tr("app.filter.op_not_in_list"),
        }
    }

    /// 运算符要求的值形态，决定值槽用哪种编辑器。
    fn value_arity(self) -> DataFilterValueArity {
        match self {
            Self::IsNull | Self::IsNotNull | Self::IsEmpty | Self::IsNotEmpty => {
                DataFilterValueArity::None
            }
            Self::Between | Self::NotBetween => DataFilterValueArity::Range,
            Self::InList | Self::NotInList => DataFilterValueArity::Many,
            Self::Eq
            | Self::Ne
            | Self::Lt
            | Self::Le
            | Self::Gt
            | Self::Ge
            | Self::Contains
            | Self::NotContains
            | Self::StartsWith
            | Self::NotStartsWith
            | Self::EndsWith
            | Self::NotEndsWith => DataFilterValueArity::Single,
        }
    }

    fn requires_values(self) -> bool {
        self.value_arity() != DataFilterValueArity::None
    }

    /// 最多允许的值个数；`None` 表示不限（列表）。
    fn max_values(self) -> Option<usize> {
        match self.value_arity() {
            DataFilterValueArity::None => Some(0),
            DataFilterValueArity::Single => Some(1),
            DataFilterValueArity::Range => Some(2),
            DataFilterValueArity::Many => None,
        }
    }

    fn all() -> &'static [Self] {
        &[
            Self::Eq,
            Self::Ne,
            Self::Lt,
            Self::Le,
            Self::Gt,
            Self::Ge,
            Self::Contains,
            Self::NotContains,
            Self::StartsWith,
            Self::NotStartsWith,
            Self::EndsWith,
            Self::NotEndsWith,
            Self::IsNull,
            Self::IsNotNull,
            Self::IsEmpty,
            Self::IsNotEmpty,
            Self::Between,
            Self::NotBetween,
            Self::InList,
            Self::NotInList,
        ]
    }
}

fn data_filter_specs_from_rules(rules: &[DataFilterRule]) -> Vec<FilterSpec> {
    rules
        .iter()
        .filter(|rule| rule.enabled)
        .filter_map(data_filter_spec_from_rule)
        .collect()
}

impl From<&DataFilterRule> for fluxdb_core::FilterFormRule {
    fn from(rule: &DataFilterRule) -> Self {
        Self {
            enabled: rule.enabled,
            field: rule.field.clone(),
            operator: match rule.operator {
                DataFilterOperator::Eq => fluxdb_core::FilterOperator::Eq,
                DataFilterOperator::Ne => fluxdb_core::FilterOperator::Ne,
                DataFilterOperator::Lt => fluxdb_core::FilterOperator::Lt,
                DataFilterOperator::Le => fluxdb_core::FilterOperator::Le,
                DataFilterOperator::Gt => fluxdb_core::FilterOperator::Gt,
                DataFilterOperator::Ge => fluxdb_core::FilterOperator::Ge,
                DataFilterOperator::Contains => fluxdb_core::FilterOperator::Contains,
                DataFilterOperator::NotContains => fluxdb_core::FilterOperator::NotContains,
                DataFilterOperator::StartsWith => fluxdb_core::FilterOperator::StartsWith,
                DataFilterOperator::NotStartsWith => fluxdb_core::FilterOperator::NotStartsWith,
                DataFilterOperator::EndsWith => fluxdb_core::FilterOperator::EndsWith,
                DataFilterOperator::NotEndsWith => fluxdb_core::FilterOperator::NotEndsWith,
                DataFilterOperator::IsNull => fluxdb_core::FilterOperator::IsNull,
                DataFilterOperator::IsNotNull => fluxdb_core::FilterOperator::IsNotNull,
                DataFilterOperator::IsEmpty => fluxdb_core::FilterOperator::IsEmpty,
                DataFilterOperator::IsNotEmpty => fluxdb_core::FilterOperator::IsNotEmpty,
                DataFilterOperator::Between => fluxdb_core::FilterOperator::Between,
                DataFilterOperator::NotBetween => fluxdb_core::FilterOperator::NotBetween,
                DataFilterOperator::InList => fluxdb_core::FilterOperator::InList,
                DataFilterOperator::NotInList => fluxdb_core::FilterOperator::NotInList,
            },
            values: rule
                .typed_values
                .as_ref()
                .filter(|values| values.len() == rule.values.len())
                .cloned()
                .unwrap_or_else(|| {
                    rule.values
                        .iter()
                        // 用户新输入的值必须保持字符串语义：`007` 不能变成整数 7。
                        .map(|value| fluxdb_core::FilterValue::Text(value.clone()))
                        .collect()
                }),
            grouped: rule.grouped,
        }
    }
}

fn data_filter_operator_from_core(operator: fluxdb_core::FilterOperator) -> DataFilterOperator {
    match operator {
        fluxdb_core::FilterOperator::Eq => DataFilterOperator::Eq,
        fluxdb_core::FilterOperator::Ne => DataFilterOperator::Ne,
        fluxdb_core::FilterOperator::Lt => DataFilterOperator::Lt,
        fluxdb_core::FilterOperator::Le => DataFilterOperator::Le,
        fluxdb_core::FilterOperator::Gt => DataFilterOperator::Gt,
        fluxdb_core::FilterOperator::Ge => DataFilterOperator::Ge,
        fluxdb_core::FilterOperator::Contains => DataFilterOperator::Contains,
        fluxdb_core::FilterOperator::NotContains => DataFilterOperator::NotContains,
        fluxdb_core::FilterOperator::StartsWith => DataFilterOperator::StartsWith,
        fluxdb_core::FilterOperator::NotStartsWith => DataFilterOperator::NotStartsWith,
        fluxdb_core::FilterOperator::EndsWith => DataFilterOperator::EndsWith,
        fluxdb_core::FilterOperator::NotEndsWith => DataFilterOperator::NotEndsWith,
        fluxdb_core::FilterOperator::IsNull => DataFilterOperator::IsNull,
        fluxdb_core::FilterOperator::IsNotNull => DataFilterOperator::IsNotNull,
        fluxdb_core::FilterOperator::IsEmpty => DataFilterOperator::IsEmpty,
        fluxdb_core::FilterOperator::IsNotEmpty => DataFilterOperator::IsNotEmpty,
        fluxdb_core::FilterOperator::Between => DataFilterOperator::Between,
        fluxdb_core::FilterOperator::NotBetween => DataFilterOperator::NotBetween,
        fluxdb_core::FilterOperator::InList => DataFilterOperator::InList,
        fluxdb_core::FilterOperator::NotInList => DataFilterOperator::NotInList,
    }
}

fn data_filter_expression_from_rules(
    rules: &[DataFilterRule],
    columns: &[GdbColumn],
) -> fluxdb_core::Result<fluxdb_core::FilterExpression> {
    let mut terms = Vec::new();
    let mut index = 0;
    while index < rules.len() {
        let rule = &rules[index];
        if rule.grouped {
            let group_start = index;
            while index + 1 < rules.len() && rules[index + 1].grouped {
                index += 1;
            }
            let group = &rules[group_start..=index];
            if let Some(expression) = data_filter_rule_sequence_expression(group, columns)? {
                terms.push((
                    expression,
                    rules[index].connector,
                ));
            }
        } else if let Some(expression) = data_filter_single_rule_expression(rule, columns)? {
            terms.push((expression, rule.connector));
        }
        index += 1;
    }
    Ok(data_filter_join_terms(terms))
}

fn data_filter_single_rule_expression(
    rule: &DataFilterRule,
    columns: &[GdbColumn],
) -> fluxdb_core::Result<Option<fluxdb_core::FilterExpression>> {
    if !rule.enabled {
        return Ok(None);
    }
    if let Some(expression) = rule
        .advanced_expression
        .as_deref()
        .filter(|expression| !expression.trim().is_empty())
    {
        return Ok(Some(fluxdb_core::FilterExpression::Raw(
            expression.to_string(),
        )));
    }
    let mut form_rule = fluxdb_core::FilterFormRule::from(rule);
    form_rule.grouped = false;
    let type_name = form_rule
        .field
        .as_deref()
        .and_then(|field| columns.iter().find(|column| column.name == field))
        .and_then(|column| column.type_name.as_deref());
    form_rule.values = form_rule
        .values
        .iter()
        .map(|value| data_filter_value_for_column(value, type_name))
        .collect();
    let expression = fluxdb_core::filter_expression_from_form(
        std::slice::from_ref(&form_rule),
        fluxdb_core::FilterCombinator::And,
    )
    .map_err(|error| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Query, error.to_string()))?;
    let fluxdb_core::FilterExpression::And(mut items) = expression else {
        return Ok(Some(expression));
    };
    Ok((items.len() == 1).then(|| items.remove(0)))
}

fn data_filter_rule_sequence_expression(
    rules: &[DataFilterRule],
    columns: &[GdbColumn],
) -> fluxdb_core::Result<Option<fluxdb_core::FilterExpression>> {
    let mut terms = Vec::new();
    for rule in rules {
        if let Some(expression) = data_filter_single_rule_expression(rule, columns)? {
            terms.push((expression, rule.connector));
        }
    }
    Ok((!terms.is_empty()).then(|| data_filter_join_terms(terms)))
}

fn data_filter_value_for_column(
    value: &fluxdb_core::FilterValue,
    type_name: Option<&str>,
) -> fluxdb_core::FilterValue {
    let fluxdb_core::FilterValue::Text(text) = value else {
        return value.clone();
    };
    match data_filter_column_scalar_type(type_name) {
        DataFilterColumnScalarType::Integer => text
            .trim()
            .parse::<i64>()
            .map(fluxdb_core::FilterValue::Integer)
            .unwrap_or_else(|_| value.clone()),
        DataFilterColumnScalarType::Float => text
            .trim()
            .parse::<f64>()
            .map(fluxdb_core::FilterValue::Float)
            .unwrap_or_else(|_| value.clone()),
        DataFilterColumnScalarType::Boolean => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => fluxdb_core::FilterValue::Boolean(true),
            "false" | "0" => fluxdb_core::FilterValue::Boolean(false),
            _ => value.clone(),
        },
        DataFilterColumnScalarType::Other => value.clone(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DataFilterColumnScalarType {
    Integer,
    Float,
    Boolean,
    Other,
}

fn data_filter_column_scalar_type(type_name: Option<&str>) -> DataFilterColumnScalarType {
    let Some(type_name) = type_name else {
        return DataFilterColumnScalarType::Other;
    };
    let base = type_name
        .trim()
        .to_ascii_lowercase()
        .split('(')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    match base.as_str() {
        "int" | "integer" | "tinyint" | "smallint" | "mediumint" | "bigint" | "int2"
        | "int4" | "int8" | "serial" | "smallserial" | "bigserial" | "year" => {
            DataFilterColumnScalarType::Integer
        }
        "numeric" | "decimal" | "dec" | "real" | "double" | "double precision" | "float"
        | "float4" | "float8" | "number" => DataFilterColumnScalarType::Float,
        "bool" | "boolean" | "bit" => DataFilterColumnScalarType::Boolean,
        _ => DataFilterColumnScalarType::Other,
    }
}

/// 行间组合符使用 SQL 默认优先级：AND 先结合，再由 OR 连接各段。
fn data_filter_join_terms(
    terms: Vec<(fluxdb_core::FilterExpression, fluxdb_core::FilterCombinator)>,
) -> fluxdb_core::FilterExpression {
    if terms.is_empty() {
        return fluxdb_core::FilterExpression::And(Vec::new());
    }
    let mut or_terms = Vec::new();
    let mut and_terms = Vec::new();
    for (index, (expression, connector)) in terms.iter().enumerate() {
        and_terms.push(expression.clone());
        if index + 1 == terms.len() || *connector == fluxdb_core::FilterCombinator::Or {
            let part = if and_terms.len() == 1 {
                and_terms.remove(0)
            } else {
                fluxdb_core::FilterExpression::And(std::mem::take(&mut and_terms))
            };
            or_terms.push(part);
        }
    }
    if or_terms.len() == 1 {
        or_terms.remove(0)
    } else {
        fluxdb_core::FilterExpression::Or(or_terms)
    }
}

fn normalized_data_filter_value(value: &str) -> String {
    value
        .trim()
        .trim_matches(['\'', '"', '‘', '’', '“', '”', '「', '」', '『', '』', '＂', '＇'])
        .to_string()
}

fn data_filter_spec_from_rule(rule: &DataFilterRule) -> Option<FilterSpec> {
    let field = rule.field.clone()?;
    let values = rule
        .values
        .iter()
        .map(|value| CellValue::Text(normalized_data_filter_value(value)))
        .collect::<Vec<_>>();
    let op = match rule.operator {
        DataFilterOperator::Eq => FilterOp::Eq,
        DataFilterOperator::Ne => FilterOp::NotEq,
        DataFilterOperator::Lt => FilterOp::LessThan,
        DataFilterOperator::Le => FilterOp::LessThanOrEqual,
        DataFilterOperator::Gt => FilterOp::GreaterThan,
        DataFilterOperator::Ge => FilterOp::GreaterThanOrEqual,
        DataFilterOperator::Contains => FilterOp::Contains,
        DataFilterOperator::NotContains => FilterOp::NotContains,
        DataFilterOperator::StartsWith => FilterOp::StartsWith,
        DataFilterOperator::NotStartsWith => FilterOp::NotStartsWith,
        DataFilterOperator::EndsWith => FilterOp::EndsWith,
        DataFilterOperator::NotEndsWith => FilterOp::NotEndsWith,
        DataFilterOperator::IsNull => FilterOp::IsNull,
        DataFilterOperator::IsNotNull => FilterOp::IsNotNull,
        DataFilterOperator::IsEmpty => FilterOp::IsEmpty,
        DataFilterOperator::IsNotEmpty => FilterOp::IsNotEmpty,
        DataFilterOperator::Between => FilterOp::Between,
        DataFilterOperator::NotBetween => FilterOp::NotBetween,
        DataFilterOperator::InList => FilterOp::InList,
        DataFilterOperator::NotInList => FilterOp::NotInList,
    };

    Some(FilterSpec {
        field,
        op,
        values,
        enabled: rule.enabled,
    })
}

fn local_filter_manager_condition_entries(
    filters: &BTreeMap<String, BTreeSet<String>>,
    editing_field: Option<&str>,
) -> Vec<(String, BTreeSet<String>)> {
    let mut entries = filters
        .iter()
        .filter(|(field, values)| {
            !values.is_empty() && editing_field.is_none_or(|editing| field.as_str() != editing)
        })
        .map(|(field, values)| (field.clone(), values.clone()))
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}

fn data_search_matches(
    page: &DataPage,
    visible_column_indexes: &[usize],
    query: &str,
) -> Vec<DataSearchMatch> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return Vec::new();
    }

    let mut matches = Vec::new();
    for (row_ix, row) in page.rows.iter().enumerate() {
        for (visible_col, source_col) in visible_column_indexes.iter().enumerate() {
            let Some(value) = row.values.get(*source_col) else {
                continue;
            };
            if data_cell_display_text(value)
                .to_ascii_lowercase()
                .contains(query.as_str())
            {
                matches.push(DataSearchMatch {
                    row_ix,
                    col_ix: visible_col + 1,
                });
            }
        }
    }
    matches
}

fn next_data_search_match(
    matches: &[DataSearchMatch],
    current: Option<DataSearchMatch>,
) -> Option<DataSearchMatch> {
    if matches.is_empty() {
        return None;
    }
    let Some(current) = current else {
        return matches.first().copied();
    };
    matches
        .iter()
        .position(|item| *item == current)
        .map(|index| matches[(index + 1) % matches.len()])
        .or_else(|| matches.first().copied())
}

fn data_search_match_position(
    matches: &[DataSearchMatch],
    current: Option<DataSearchMatch>,
) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }

    current
        .and_then(|current| {
            matches
                .iter()
                .position(|item| *item == current)
                .map(|index| index + 1)
        })
        .or(Some(1))
}

fn data_search_match_label(
    matches: &[DataSearchMatch],
    current: Option<DataSearchMatch>,
) -> String {
    match data_search_match_position(matches, current) {
        Some(position) => tr("app.filter.matches")
            .replace("%{position}", &position.to_string())
            .replace("%{count}", &matches.len().to_string()),
        None => tr("app.filter.no_matches").to_string(),
    }
}

fn local_table_filters_after_value_toggle(
    mut filters: BTreeMap<String, BTreeSet<String>>,
    field: &str,
    value: &str,
) -> BTreeMap<String, BTreeSet<String>> {
    let values = filters.entry(field.to_string()).or_default();
    if !values.remove(value) {
        values.insert(value.to_string());
    }
    if values.is_empty() {
        filters.remove(field);
    }
    filters
}

#[cfg(test)]
mod data_filter_expression_tests {
    use super::*;

    fn rule(field: &str, value: &str, connector: fluxdb_core::FilterCombinator) -> DataFilterRule {
        DataFilterRule {
            enabled: true,
            field: Some(field.into()),
            operator: DataFilterOperator::Eq,
            values: vec![value.into()],
            advanced_expression: None,
            typed_values: None,
            connector,
            grouped: false,
        }
    }

    #[test]
    fn row_connectors_follow_sql_and_precedence() {
        let expression = data_filter_expression_from_rules(
            &[
                rule("a", "1", fluxdb_core::FilterCombinator::Or),
                rule("b", "2", fluxdb_core::FilterCombinator::And),
                rule("c", "3", fluxdb_core::FilterCombinator::And),
            ],
            &[],
        )
        .expect("expression");
        let fluxdb_core::FilterExpression::Or(items) = expression else {
            panic!("OR root");
        };
        assert!(matches!(&items[0], fluxdb_core::FilterExpression::Condition(_)));
        assert!(matches!(&items[1], fluxdb_core::FilterExpression::And(nodes) if nodes.len() == 2));
    }

    #[test]
    fn numeric_column_text_values_use_native_filter_types() {
        let columns = vec![
            GdbColumn {
                name: "id".into(),
                type_name: Some("integer".into()),
                nullable: false,
                primary_key: true,
                comment: None,
            },
            GdbColumn {
                name: "amount".into(),
                type_name: Some("numeric(12,4)".into()),
                nullable: false,
                primary_key: false,
                comment: None,
            },
        ];
        let expression = data_filter_expression_from_rules(
            &[
                rule("id", "1", fluxdb_core::FilterCombinator::And),
                rule("amount", "2.5", fluxdb_core::FilterCombinator::And),
            ],
            &columns,
        )
        .expect("expression");
        let fluxdb_core::FilterExpression::And(items) = expression else {
            panic!("AND root");
        };
        let fluxdb_core::FilterExpression::Condition(id) = &items[0] else {
            panic!("id condition");
        };
        let fluxdb_core::FilterExpression::Condition(amount) = &items[1] else {
            panic!("amount condition");
        };
        assert_eq!(id.values, vec![fluxdb_core::FilterValue::Integer(1)]);
        assert_eq!(amount.values, vec![fluxdb_core::FilterValue::Float(2.5)]);
    }
}

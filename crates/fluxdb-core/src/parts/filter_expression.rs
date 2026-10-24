use std::sync::OnceLock;

/// 数据页筛选的统一表达式树。
///
/// UI 表单、条件文本和连接器共享这棵树；解析与 SQL 渲染的方言差异由
/// `FilterDialect` 实现负责，调用方不按 `DatabaseKind` 分支。
#[derive(Clone, Debug, PartialEq)]
pub enum FilterExpression {
    Condition(FilterCondition),
    And(Vec<FilterExpression>),
    Or(Vec<FilterExpression>),
    Not(Box<FilterExpression>),
    /// 当前方言能够解析、但无法无损映射到表单条件的表达式。
    ///
    /// 它只保存数据库方言的规范化文本，仍由同一方言执行；UI 以只读高级条件展示，
    /// 不会为了强行回填表单而改变语义。
    Raw(String),
}
#[derive(Clone, Debug, PartialEq)]
pub struct FilterCondition {
    pub field: String,
    pub op: FilterOperator,
    pub values: Vec<FilterValue>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilterOperator {
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

impl FilterOperator {
    /// 该运算符是否需要一个或多个值。`IS NULL`/`IS EMPTY` 一类无值条件
    /// 不能在表单转换时被误判为缺少值。
    pub fn requires_values(self) -> bool {
        !matches!(
            self,
            Self::IsNull | Self::IsNotNull | Self::IsEmpty | Self::IsNotEmpty
        )
    }

    fn value_arity(self) -> Option<usize> {
        match self {
            Self::IsNull | Self::IsNotNull | Self::IsEmpty | Self::IsNotEmpty => Some(0),
            Self::Between | Self::NotBetween => Some(2),
            Self::InList | Self::NotInList => None,
            _ => Some(1),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum FilterValue {
    Text(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Null,
}

/// 一次筛选文本解析的结果。`text` 是规范化文本，用于在往返时保持稳定；
/// `diagnostic` 只在不合法时给出，不能靠部分解析覆盖用户输入。
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedFilterExpression {
    pub expression: FilterExpression,
    pub text: String,
}

/// 数据库筛选方言契约。
///
/// 实现放在 `fluxdb-connectors`，UI 与 app 只通过这个接口工作。所有方法都必须
/// 保持参数不进入 SQL 字符串；渲染结果只用于预览，真实执行仍由连接器绑定参数。
pub trait FilterDialect: Send + Sync {
    fn parse(
        &self,
        text: &str,
    ) -> std::result::Result<ParsedFilterExpression, FilterExpressionError>;

    fn render(
        &self,
        expression: &FilterExpression,
    ) -> std::result::Result<String, FilterExpressionError>;

    /// 使用当前数据页列元数据渲染预览 SQL。
    ///
    /// 方言可据此避免对文本列生成无意义的 `CAST`；默认实现保持旧契约。
    fn render_with_columns(
        &self,
        expression: &FilterExpression,
        _columns: &[crate::Column],
    ) -> std::result::Result<String, FilterExpressionError> {
        self.render(expression)
    }

    /// 把表达式编译为连接器可执行的条件；参数以占位符绑定，禁止拼接字面量。
    fn compile(&self, expression: &FilterExpression)
    -> std::result::Result<CompiledFilterExpression, FilterExpressionError>;

    /// 使用列元数据编译表达式；默认实现保持旧行为。
    fn compile_with_columns(
        &self,
        expression: &FilterExpression,
        _columns: &[crate::Column],
    ) -> std::result::Result<CompiledFilterExpression, FilterExpressionError> {
        self.compile(expression)
    }
}

type FilterDialectResolver =
    fn(crate::DatabaseKind) -> Option<&'static dyn FilterDialect>;
static FILTER_DIALECT_RESOLVER: OnceLock<FilterDialectResolver> = OnceLock::new();

/// 由连接器 crate 注册具体方言实现，Core 不反向依赖连接器。
pub fn register_filter_dialect_resolver(resolver: FilterDialectResolver) {
    let _ = FILTER_DIALECT_RESOLVER.set(resolver);
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompiledFilterExpression {
    pub sql: String,
    pub parameters: Vec<FilterValue>,
}

/// 连接器可执行的条件树。SQL 片段只含占位符；参数下标按遍历顺序索引。
#[derive(Clone, Debug, PartialEq)]
pub enum CompiledFilterNode {
    Condition {
        sql: String,
        parameters: Vec<FilterValue>,
    },
    And(Vec<CompiledFilterNode>),
    Or(Vec<CompiledFilterNode>),
    Not(Box<CompiledFilterNode>),
}

impl CompiledFilterExpression {
    pub fn node(&self) -> Option<CompiledFilterNode> {
        if self.sql.trim().is_empty() {
            None
        } else {
            Some(CompiledFilterNode::Condition {
                sql: self.sql.clone(),
                parameters: self.parameters.clone(),
            })
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterExpressionError {
    pub message: String,
    pub position: Option<usize>,
}

impl FilterExpressionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            position: None,
        }
    }

    pub fn at(message: impl Into<String>, position: usize) -> Self {
        Self {
            message: message.into(),
            position: Some(position),
        }
    }
}

impl std::fmt::Display for FilterExpressionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.position {
            Some(position) => write!(formatter, "{}（位置 {}）", self.message, position),
            None => formatter.write_str(&self.message),
        }
    }
}

impl std::error::Error for FilterExpressionError {}

/// 表单编辑器的规则视图。表单只负责编辑这套结构；它到表达式树的转换集中在
/// 一个函数里，避免 UI 自己决定 AND/OR 的优先级。
#[derive(Clone, Debug, PartialEq)]
pub struct FilterFormRule {
    pub enabled: bool,
    pub field: Option<String>,
    pub operator: FilterOperator,
    pub values: Vec<FilterValue>,
    pub grouped: bool,
}

/// 表单规则的 AND/OR 组合入口。`combinator` 是表单标题处的全局组合符；
/// 后续增加嵌套分组时也只扩大这里，不把逻辑散落到各连接器。
pub fn filter_expression_from_form(
    rules: &[FilterFormRule],
    combinator: FilterCombinator,
) -> std::result::Result<FilterExpression, FilterExpressionError> {
    let mut expressions = Vec::new();
    let mut group = Vec::new();
    for rule in rules.iter().filter(|rule| rule.enabled) {
        let Some(field) = rule.field.as_deref().filter(|field| !field.is_empty()) else {
            continue;
        };
        if rule.values.is_empty() && rule.operator.requires_values() {
            return Err(FilterExpressionError::new(format!(
                "字段 {field} 的筛选条件缺少值"
            )));
        }
        if rule
            .operator
            .value_arity()
            .is_some_and(|arity| arity != 0 && rule.values.len() != arity)
        {
            return Err(FilterExpressionError::new(format!(
                "字段 {field} 的筛选值数量不匹配"
            )));
        }
        let condition = FilterExpression::Condition(FilterCondition {
            field: field.to_string(),
            op: rule.operator,
            values: rule.values.clone(),
        });
        if rule.grouped {
            group.push(condition);
        } else {
            if !group.is_empty() {
                expressions.push(FilterExpression::And(std::mem::take(&mut group)));
            }
            expressions.push(condition);
        }
    }
    if !group.is_empty() {
        expressions.push(FilterExpression::And(group));
    }
    Ok(match combinator {
        FilterCombinator::And => FilterExpression::And(expressions),
        FilterCombinator::Or => FilterExpression::Or(expressions),
    })
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FilterCombinator {
    #[default]
    And,
    Or,
}

/// 表达式投影为表单规则。顶层组合直接映射为表单的 AND/OR；
/// 单个条件映射为一条规则；无法无损扁平化的子树保留为只读高级规则。
pub struct FilterFormProjection {
    pub rules: Vec<FilterFormRule>,
    /// 与 `rules` 等长，第 i 项表示该规则之后与下一项的连接符。
    pub connectors: Vec<FilterCombinator>,
    /// 与 `rules` 等长，0 表示顶层，正值表示所在括号嵌套深度。
    pub group_depths: Vec<u8>,
    /// 与 `rules` 等长；规则 i 之后关闭到该值，0 表示不关闭。
    pub group_ends: Vec<u8>,
    pub combinator: FilterCombinator,
}

pub fn filter_form_projection_from_expression(
    expression: &FilterExpression,
    render_raw: impl Fn(&FilterExpression) -> String,
) -> FilterFormProjection {
    let (root_combinator, projected) = match expression {
        FilterExpression::And(items) if !items.is_empty() => (
            FilterCombinator::And,
            project_level(
                items,
                FilterCombinator::And,
                FilterCombinator::And,
                0,
                &render_raw,
            ),
        ),
        FilterExpression::Or(items) if !items.is_empty() => (
            FilterCombinator::Or,
            project_level(
                items,
                FilterCombinator::Or,
                FilterCombinator::Or,
                0,
                &render_raw,
            ),
        ),
        FilterExpression::And(_) | FilterExpression::Or(_) => {
            (FilterCombinator::And, Some(ProjectedSequence::default()))
        }
        _ => (
            FilterCombinator::And,
            project_simple_term(expression, 0, &render_raw)
                .map(|rule| ProjectedSequence::from_rule(rule, FilterCombinator::And, 0)),
        ),
    };
    match projected {
        Some(projected) => FilterFormProjection {
            rules: projected.rules,
            connectors: projected.connectors,
            group_depths: projected.group_depths,
            group_ends: projected.group_ends,
            combinator: root_combinator,
        },
        None => {
            let raw = render_raw(expression);
            FilterFormProjection {
                rules: vec![FilterFormRule {
                    enabled: true,
                    field: None,
                    operator: FilterOperator::Eq,
                    values: vec![FilterValue::Text(raw)],
                    grouped: false,
                }],
                connectors: vec![FilterCombinator::And],
                group_depths: vec![0],
                group_ends: vec![0],
                combinator: FilterCombinator::And,
            }
        }
    }
}

#[derive(Default)]
struct ProjectedSequence {
    rules: Vec<FilterFormRule>,
    connectors: Vec<FilterCombinator>,
    group_depths: Vec<u8>,
    group_ends: Vec<u8>,
}

impl ProjectedSequence {
    fn from_rule(rule: FilterFormRule, connector: FilterCombinator, depth: u8) -> Self {
        Self {
            rules: vec![rule],
            connectors: vec![connector],
            group_depths: vec![depth],
            group_ends: vec![0],
        }
    }
}

fn project_level(
    items: &[FilterExpression],
    combinator: FilterCombinator,
    outer: FilterCombinator,
    depth: u8,
    render_raw: &impl Fn(&FilterExpression) -> String,
) -> Option<ProjectedSequence> {
    let mut projected = ProjectedSequence::default();
    for item in items {
        let sequence = match item {
            FilterExpression::Condition(_) | FilterExpression::Raw(_) | FilterExpression::Not(_) => {
                ProjectedSequence::from_rule(
                    project_simple_term(item, depth, render_raw)?,
                    combinator,
                    depth,
                )
            }
            FilterExpression::And(children) | FilterExpression::Or(children)
                if children.is_empty() =>
            {
                continue;
            }
            FilterExpression::And(children) if combinator == FilterCombinator::And => {
                project_level(children, combinator, combinator, depth, render_raw)?
            }
            FilterExpression::Or(children) if combinator == FilterCombinator::Or => {
                project_level(children, combinator, combinator, depth, render_raw)?
            }
            FilterExpression::And(children) => {
                project_level(children, FilterCombinator::And, combinator, depth + 1, render_raw)?
            }
            FilterExpression::Or(children) => {
                let sequence =
                    project_level(children, FilterCombinator::Or, combinator, depth + 1, render_raw)?;
                let mut sequence = sequence;
                if let Some(first) = sequence.group_depths.first_mut() {
                    *first = depth + 1;
                }
                ProjectedSequence {
                    rules: sequence.rules,
                    connectors: sequence.connectors,
                    group_depths: sequence.group_depths,
                    group_ends: sequence.group_ends,
                }
            }
        };
        if sequence.rules.is_empty() {
            continue;
        }
        projected.rules.extend(sequence.rules);
        projected.connectors.extend(sequence.connectors);
        projected.group_depths.extend(sequence.group_depths);
        projected.group_ends.extend(sequence.group_ends);
    }
    if depth > 0
        && let Some(last) = projected.group_ends.last_mut()
    {
        *last = (*last).max(depth);
    }
    let last_depth = projected.group_depths.last().copied().unwrap_or(depth);
    if last_depth > depth
        && let Some(last) = projected.group_ends.last_mut()
    {
        *last = last_depth;
    }
    if let Some(last) = projected.connectors.last_mut() {
        *last = outer;
    }
    Some(projected)
}

fn project_simple_term(
    expression: &FilterExpression,
    depth: u8,
    render_raw: &impl Fn(&FilterExpression) -> String,
) -> Option<FilterFormRule> {
    let rule = match expression {
        FilterExpression::Condition(condition) => FilterFormRule {
            enabled: true,
            field: Some(condition.field.clone()),
            operator: condition.op,
            values: condition.values.clone(),
            grouped: depth > 0,
        },
        FilterExpression::Not(_) | FilterExpression::Raw(_) => FilterFormRule {
            enabled: true,
            field: None,
            operator: FilterOperator::Eq,
            values: vec![FilterValue::Text(render_raw(expression))],
            grouped: depth > 0,
        },
        FilterExpression::And(_) | FilterExpression::Or(_) => return None,
    };
    Some(rule)
}

pub fn filter_value_text(value: &FilterValue) -> String {
    match value {
        FilterValue::Text(value) => value.clone(),
        FilterValue::Integer(value) => value.to_string(),
        FilterValue::Float(value) => value.to_string(),
        FilterValue::Boolean(value) => value.to_string(),
        FilterValue::Null => "NULL".to_string(),
    }
}

/// 筛选方言解析/编译的稳定入口。实现由连接器层注册，Core 不依赖具体数据库实现。
pub fn filter_dialect_for(
    kind: crate::DatabaseKind,
) -> Option<&'static dyn FilterDialect> {
    FILTER_DIALECT_RESOLVER
        .get()
        .copied()
        .and_then(|resolver| resolver(kind))
}

#[cfg(test)]
mod filter_form_tests {
    use super::*;

    #[test]
    fn grouped_form_rules_stay_an_and_group_under_root_or() {
        let rules = vec![
            FilterFormRule {
                enabled: true,
                field: Some("a".into()),
                operator: FilterOperator::Eq,
                values: vec![FilterValue::Integer(1)],
                grouped: true,
            },
            FilterFormRule {
                enabled: true,
                field: Some("b".into()),
                operator: FilterOperator::Eq,
                values: vec![FilterValue::Integer(2)],
                grouped: true,
            },
            FilterFormRule {
                enabled: true,
                field: Some("c".into()),
                operator: FilterOperator::Eq,
                values: vec![FilterValue::Integer(3)],
                grouped: false,
            },
        ];
        let expression =
            filter_expression_from_form(&rules, FilterCombinator::Or).expect("form");
        let FilterExpression::Or(items) = expression else {
            panic!("root OR");
        };
        assert!(matches!(&items[0], FilterExpression::And(group) if group.len() == 2));
        assert!(matches!(&items[1], FilterExpression::Condition(_)));
    }
}

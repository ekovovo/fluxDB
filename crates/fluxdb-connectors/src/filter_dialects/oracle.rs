use fluxdb_core::{
    CompiledFilterExpression, FilterDialect, FilterExpression, FilterExpressionError,
    ParsedFilterExpression,
};

/// Oracle 筛选方言。
///
/// 表达式语法与 PostgreSQL 接近（`IS NULL`、`BETWEEN`、`LIKE … ESCAPE`），差异在
/// 标识符引用（双引号、大小写敏感）、绑定占位符（`:N`）、布尔字面量（无 SQL 层布尔）
/// 与「空字符串即 NULL」。这些差异由 `FilterSyntax::Oracle` 在渲染/编译层处理，
/// 本类型只负责把统一表达式接到那条链路。
pub struct OracleFilterDialect;

pub(crate) const ORACLE_SYNTAX: OracleFilterDialect = OracleFilterDialect;

impl FilterDialect for OracleFilterDialect {
    fn parse(&self, text: &str) -> Result<ParsedFilterExpression, FilterExpressionError> {
        super::parse_for_syntax(super::FilterSyntax::Oracle, text)
    }

    fn render(&self, expression: &FilterExpression) -> Result<String, FilterExpressionError> {
        super::render_for_syntax(super::FilterSyntax::Oracle, expression)
    }

    fn render_with_columns(
        &self,
        expression: &FilterExpression,
        columns: &[fluxdb_core::Column],
    ) -> Result<String, FilterExpressionError> {
        super::render_for_syntax_with_columns(super::FilterSyntax::Oracle, expression, columns)
    }

    fn compile(
        &self,
        expression: &FilterExpression,
    ) -> Result<CompiledFilterExpression, FilterExpressionError> {
        super::compile_for_syntax(super::FilterSyntax::Oracle, expression)
    }

    fn compile_with_columns(
        &self,
        expression: &FilterExpression,
        columns: &[fluxdb_core::Column],
    ) -> Result<CompiledFilterExpression, FilterExpressionError> {
        super::compile_for_syntax_with_columns(super::FilterSyntax::Oracle, expression, columns)
    }
}

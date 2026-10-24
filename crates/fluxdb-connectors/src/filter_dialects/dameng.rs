use fluxdb_core::{
    CompiledFilterExpression, FilterDialect, FilterExpression, FilterExpressionError,
    ParsedFilterExpression,
};

/// 达梦当前复用通用的 SQL 比较语法；保留独立类型，后续可只改本文件适配
/// Oracle 风格函数、伪列或 `ESCAPE` 差异，不影响 PostgreSQL/MySQL 实现。
pub struct DamengFilterDialect;

pub(crate) const DAMENG_SYNTAX: DamengFilterDialect = DamengFilterDialect;

impl FilterDialect for DamengFilterDialect {
    fn parse(&self, text: &str) -> Result<ParsedFilterExpression, FilterExpressionError> {
        super::parse_for_syntax(super::FilterSyntax::Dameng, text)
    }

    fn render(&self, expression: &FilterExpression) -> Result<String, FilterExpressionError> {
        super::render_for_syntax(super::FilterSyntax::Dameng, expression)
    }

    fn render_with_columns(
        &self,
        expression: &FilterExpression,
        columns: &[fluxdb_core::Column],
    ) -> Result<String, FilterExpressionError> {
        super::render_for_syntax_with_columns(super::FilterSyntax::Dameng, expression, columns)
    }

    fn compile(
        &self,
        expression: &FilterExpression,
    ) -> Result<CompiledFilterExpression, FilterExpressionError> {
        super::compile_for_syntax(super::FilterSyntax::Dameng, expression)
    }

    fn compile_with_columns(
        &self,
        expression: &FilterExpression,
        columns: &[fluxdb_core::Column],
    ) -> Result<CompiledFilterExpression, FilterExpressionError> {
        super::compile_for_syntax_with_columns(super::FilterSyntax::Dameng, expression, columns)
    }
}

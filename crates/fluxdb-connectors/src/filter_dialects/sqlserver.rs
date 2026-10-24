use fluxdb_core::{
    CompiledFilterExpression, FilterDialect, FilterExpression, FilterExpressionError,
    ParsedFilterExpression,
};

pub struct SqlServerFilterDialect;

pub(crate) const SQLSERVER_SYNTAX: SqlServerFilterDialect = SqlServerFilterDialect;

impl FilterDialect for SqlServerFilterDialect {
    fn parse(&self, text: &str) -> Result<ParsedFilterExpression, FilterExpressionError> {
        super::parse_for_syntax(super::FilterSyntax::SqlServer, text)
    }

    fn render(&self, expression: &FilterExpression) -> Result<String, FilterExpressionError> {
        super::render_for_syntax(super::FilterSyntax::SqlServer, expression)
    }

    fn render_with_columns(
        &self,
        expression: &FilterExpression,
        columns: &[fluxdb_core::Column],
    ) -> Result<String, FilterExpressionError> {
        super::render_for_syntax_with_columns(super::FilterSyntax::SqlServer, expression, columns)
    }

    fn compile(
        &self,
        expression: &FilterExpression,
    ) -> Result<CompiledFilterExpression, FilterExpressionError> {
        super::compile_for_syntax(super::FilterSyntax::SqlServer, expression)
    }

    fn compile_with_columns(
        &self,
        expression: &FilterExpression,
        columns: &[fluxdb_core::Column],
    ) -> Result<CompiledFilterExpression, FilterExpressionError> {
        super::compile_for_syntax_with_columns(super::FilterSyntax::SqlServer, expression, columns)
    }
}

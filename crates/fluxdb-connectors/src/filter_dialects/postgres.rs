use fluxdb_core::{
    CompiledFilterExpression, FilterDialect, FilterExpression, FilterExpressionError,
    ParsedFilterExpression,
};

pub struct PostgresFilterDialect;

pub(crate) const POSTGRES_SYNTAX: PostgresFilterDialect = PostgresFilterDialect;

impl FilterDialect for PostgresFilterDialect {
    fn parse(&self, text: &str) -> Result<ParsedFilterExpression, FilterExpressionError> {
        super::parse_for_syntax(super::FilterSyntax::Postgres, text)
    }

    fn render(&self, expression: &FilterExpression) -> Result<String, FilterExpressionError> {
        super::render_for_syntax(super::FilterSyntax::Postgres, expression)
    }

    fn render_with_columns(
        &self,
        expression: &FilterExpression,
        columns: &[fluxdb_core::Column],
    ) -> Result<String, FilterExpressionError> {
        super::render_for_syntax_with_columns(super::FilterSyntax::Postgres, expression, columns)
    }

    fn compile(
        &self,
        expression: &FilterExpression,
    ) -> Result<CompiledFilterExpression, FilterExpressionError> {
        super::compile_for_syntax(super::FilterSyntax::Postgres, expression)
    }

    fn compile_with_columns(
        &self,
        expression: &FilterExpression,
        columns: &[fluxdb_core::Column],
    ) -> Result<CompiledFilterExpression, FilterExpressionError> {
        super::compile_for_syntax_with_columns(super::FilterSyntax::Postgres, expression, columns)
    }
}

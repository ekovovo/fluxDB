//! fluxDB SQL 语义补全引擎。
//!
//! 设计目标：补全候选由「光标所在的语法槽位」决定，而不是由光标前的字符串决定。
//! 管线为 词法上下文 → 容错解析 → 符号表 → 槽位定位 → 槽位策略 → 过滤排序。
//!
//! 本 crate 不依赖任何其他 workspace crate：宿主（fluxdb-app）实现
//! [`CompletionDialect`] 与 [`MetadataSource`] 反向适配，新增数据库只需再加一个
//! 方言实现，引擎本身不需要改动。

mod diagnose;
mod dialect;
mod engine;
mod locate;
mod metadata;
mod model;
mod parse;
mod strategy;
mod symbols;
mod word;

pub use diagnose::{Diagnostic, DiagnosticKind, DiagnosticRequest, diagnose};
pub use dialect::{
    CompletionDialect, DAMENG, GENERIC, MYSQL, ORACLE, POSTGRES, SQLITE, SQLSERVER,
    builtin_function_signature,
};
pub use engine::{complete, fuzzy_score};
pub use metadata::{
    ColumnMeta, ForeignKeyMeta, MetadataSource, RoutineKind, RoutineMeta, TableKey, TableKind,
    TableMeta,
};
pub use model::{Candidate, CandidateKind, CompletionOutput, CompletionRequest, WordContext};

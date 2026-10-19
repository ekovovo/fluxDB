// ER 关系取值：稳定机器键与解析（er-design §5.3/§5.4 的 wire 表示）。
//
// 标签是持久化与对外投影共用的机器键，不随文案变化；解析只接受已知取值，
// 未知取值返回 `None`，由调用方给受控错误，不做「像哪个就是哪个」的猜测。
//
// 本文件被 include! 进 core lib.rs（crate root scope），不重复 use crate-root 导入。

impl ErRelationshipOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            ErRelationshipOrigin::DatabaseConstraint => "database_constraint",
            ErRelationshipOrigin::User => "user",
            ErRelationshipOrigin::SqlObservation => "sql_observation",
            ErRelationshipOrigin::Agent => "agent",
        }
    }
}

/// 关系来源的中文标签（ER 面板展示用）。机器键见 `ErRelationshipOrigin::as_str`。
pub fn er_relationship_origin_label(origin: ErRelationshipOrigin) -> &'static str {
    match origin {
        ErRelationshipOrigin::DatabaseConstraint => "数据库约束",
        ErRelationshipOrigin::User => "用户",
        ErRelationshipOrigin::SqlObservation => "JOIN 观察",
        ErRelationshipOrigin::Agent => "AI 建议",
    }
}

impl ErReviewState {
    pub fn as_str(self) -> &'static str {
        match self {
            ErReviewState::Proposed => "proposed",
            ErReviewState::Confirmed => "confirmed",
            ErReviewState::Rejected => "rejected",
        }
    }
}

impl ErEnforcementKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErEnforcementKind::None => "none",
            ErEnforcementKind::DeclaredForeignKey => "declared_foreign_key",
            ErEnforcementKind::DeclaredUnique => "declared_unique",
        }
    }
}

impl ErValidityState {
    pub fn as_str(self) -> &'static str {
        match self {
            ErValidityState::Current => "current",
            ErValidityState::Stale => "stale",
            ErValidityState::Unresolved => "unresolved",
            ErValidityState::Invalid => "invalid",
        }
    }
}

impl ErRelationSide {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErRelationSide::Left => "left",
            ErRelationSide::Right => "right",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "left" => Some(ErRelationSide::Left),
            "right" => Some(ErRelationSide::Right),
            _ => None,
        }
    }
}

impl ErFilterOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErFilterOp::Eq => "eq",
            ErFilterOp::Ne => "ne",
            ErFilterOp::IsNull => "is_null",
            ErFilterOp::IsNotNull => "is_not_null",
            ErFilterOp::In => "in",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "eq" => Some(ErFilterOp::Eq),
            "ne" => Some(ErFilterOp::Ne),
            "is_null" => Some(ErFilterOp::IsNull),
            "is_not_null" => Some(ErFilterOp::IsNotNull),
            "in" => Some(ErFilterOp::In),
            _ => None,
        }
    }
}

impl ErCardinalityBound {
    pub fn as_str(self) -> &'static str {
        match self {
            ErCardinalityBound::Zero => "zero",
            ErCardinalityBound::One => "one",
            ErCardinalityBound::Many => "many",
            ErCardinalityBound::Unknown => "unknown",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "zero" => Some(ErCardinalityBound::Zero),
            "one" => Some(ErCardinalityBound::One),
            "many" => Some(ErCardinalityBound::Many),
            "unknown" => Some(ErCardinalityBound::Unknown),
            _ => None,
        }
    }
}

impl ErCardinalityBasis {
    pub fn as_str(self) -> &'static str {
        match self {
            ErCardinalityBasis::DatabaseConstraint => "database_constraint",
            ErCardinalityBasis::UserAssertion => "user_assertion",
            ErCardinalityBasis::ProfileObservation => "profile_observation",
            ErCardinalityBasis::Unknown => "unknown",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "database_constraint" => Some(ErCardinalityBasis::DatabaseConstraint),
            "user_assertion" => Some(ErCardinalityBasis::UserAssertion),
            "profile_observation" => Some(ErCardinalityBasis::ProfileObservation),
            "unknown" => Some(ErCardinalityBasis::Unknown),
            _ => None,
        }
    }
}


// ER 关系身份、外键派生与列可比较性（er-design §5.2/§5.3/§6.2）。
//
// 只放纯函数与常量：不读数据库、不持有状态。存在这里的规则都由「关系是同一条」这件事
// 唯一定义，桌面 ER 画布、应用层关系/路径查询、结构重绑与 Agent 建议提交共用同一份，
// 避免出现「UI 认为是同一条关系、AI 返回两条」这类分裂判定。
//
// 本文件被 include! 进 core lib.rs（crate root scope）；serde 的 Serialize/Deserialize
// 与既有 include! 文件共享 crate-root 导入，不得在此重复 use（避免 E0252 重复定义）。

/// 由结构化表身份生成实体 ID（`database:schema:name`；schema 为空时不带中段分隔歧义）。
///
/// 这是 `ErRelationship` 端点引用的唯一生成规则：列 ID 以实体 ID 为前缀，
/// 因此列 ID 本身即可反查所属表，不需要额外的外键表。
pub fn er_entity_id(reference: &ErTableRef) -> String {
    format!(
        "{}:{}:{}",
        reference.database,
        reference.schema.as_deref().unwrap_or_default(),
        reference.name
    )
}

/// 由表身份与列名生成列 ID（`实体ID::列名`）。
pub fn er_column_id(reference: &ErTableRef, column: &str) -> String {
    format!("{}::{column}", er_entity_id(reference))
}

/// 列 ID 的末段列名（结构未加载、只能按 ID 展示时的回退名）。
pub fn er_column_id_name(column_id: &str) -> &str {
    column_id
        .rsplit("::")
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(column_id)
}

/// 实体 ID 反解出表身份（`database:schema:name`，前两段是固定分隔，`name` 可含 `:`）。
///
/// 用于关系目录里只有端点 ID 时还原表定位（例如「这条关系的另一端是哪张表」）。
/// 段数不足或表名为空时返回 `None`：宁可显式报未解析，也不猜出一张不存在的表。
pub fn er_entity_id_parts(entity_id: &str) -> Option<(String, Option<String>, String)> {
    let mut segments = entity_id.splitn(3, ':');
    let database = segments.next()?.to_string();
    let schema = segments.next()?.to_string();
    let name = segments.next()?.to_string();
    if name.is_empty() {
        return None;
    }
    Some((database, (!schema.is_empty()).then_some(schema), name))
}

/// ER 关系目录的持久化作用域键（连接 + 库 + schema）。
///
/// ER 页面与 AI 关系工具必须落在同一作用域，否则同一库会出现两份互不可见的目录。
pub fn er_relationship_scope_key(
    connection_id: ConnectionId,
    database: &str,
    schema: Option<&str>,
) -> String {
    format!(
        "{}:{database}:{}",
        connection_id.0,
        schema.unwrap_or_default()
    )
}

/// 关系身份键：忽略左右书写方向，保留角色、完整字段配对与必要过滤条件。
///
/// 用于「物理外键 ↔ 本地逻辑关系」以及「提交建议 ↔ 已有关系」的去重：
/// - 同一关系反向书写（left/right 互换）视为同一条，键相同；
/// - 角色或字段配对不同即视为不同关系，键不同（不同业务角色不得误去重）。
pub fn er_relationship_identity_key(relationship: &ErRelationship) -> String {
    let forward = relationship.left_entity <= relationship.right_entity;
    let (first, second) = if forward {
        (&relationship.left_entity, &relationship.right_entity)
    } else {
        (&relationship.right_entity, &relationship.left_entity)
    };
    // 等值 AND 配对没有顺序语义，但键必须稳定：排序后拼接，保证同一集合恒得同一键。
    let mut pairs: Vec<String> = relationship
        .column_pairs
        .iter()
        .map(|pair| {
            if forward {
                format!("{}={}", pair.left_column, pair.right_column)
            } else {
                format!("{}={}", pair.right_column, pair.left_column)
            }
        })
        .collect();
    pairs.sort();
    let mut filters: Vec<String> = relationship
        .required_filters
        .iter()
        .map(|filter| {
            let side = if forward {
                filter.side.clone()
            } else {
                match filter.side {
                    ErRelationSide::Left => ErRelationSide::Right,
                    ErRelationSide::Right => ErRelationSide::Left,
                }
            };
            format!(
                "{side:?}:{}:{:?}:{:?}",
                filter.column_id, filter.op, filter.literal
            )
        })
        .collect();
    filters.sort();
    format!(
        "{first}|{second}|{}|{}|{}",
        relationship.role.trim().to_lowercase(),
        pairs.join(","),
        filters.join(",")
    )
}

/// 由关系索引的外键边派生「数据库约束关系」。
///
/// 派生结果仍是标准 [`ErRelationship`]：origin=DatabaseConstraint、已确认、带
/// DeclaredForeignKey 约束引用，因此可用性与 JOIN 计划仍由 `usage()`/`join_plan()`
/// 判定，不为 AI 另建一套规则。同一复合外键的逐列边按约束名合并回一条有序关系。
///
/// `enforced=None`：连接器未暴露外键是否真实生效，如实留空，由 `usage()` 产出
/// `constraint_not_enforced` 警告，不谎称完整性成立。
pub fn er_foreign_key_relationships(edges: &[ErForeignKeyEdge]) -> Vec<ErRelationship> {
    let mut order: Vec<(ErTableRef, ErTableRef, String)> = Vec::new();
    let mut grouped: BTreeMap<(ErTableRef, ErTableRef, String), Vec<ErColumnPair>> = BTreeMap::new();
    for edge in edges {
        let key = (
            edge.from_reference.clone(),
            edge.to_reference.clone(),
            edge.name.clone(),
        );
        if !grouped.contains_key(&key) {
            order.push(key.clone());
        }
        grouped.entry(key).or_default().push(ErColumnPair {
            left_column: er_column_id(&edge.from_reference, &edge.from_column),
            right_column: er_column_id(&edge.to_reference, &edge.to_column),
        });
    }
    order
        .into_iter()
        .filter_map(|key| {
            let column_pairs = grouped.remove(&key)?;
            let (from, to, constraint) = key;
            Some(ErRelationship {
                id: format!(
                    "fk:{}->{}#{constraint}",
                    er_entity_id(&from),
                    er_entity_id(&to)
                ),
                revision: 1,
                left_entity: er_entity_id(&from),
                right_entity: er_entity_id(&to),
                role: constraint.clone(),
                column_pairs,
                // 外键只声明两端相等；软删/租户边界等常驻谓词不是外键能表达的，
                // 缺失即由「业务角色歧义」暴露给用户与 AI，不在这里编造过滤条件。
                required_filters: Vec::new(),
                match_cardinality: ErMatchCardinality {
                    // 本端（持有约束）是多端，被引用端至多一条。
                    left_to_right: ErCardinality {
                        min: ErCardinalityBound::Zero,
                        max: ErCardinalityBound::One,
                    },
                    right_to_left: ErCardinality {
                        min: ErCardinalityBound::Zero,
                        max: ErCardinalityBound::Many,
                    },
                    basis: ErCardinalityBasis::DatabaseConstraint,
                },
                origin: ErRelationshipOrigin::DatabaseConstraint,
                review: ErRelationshipReview {
                    state: ErReviewState::Confirmed,
                    confirmed_revision: Some(1),
                    confirmed_by: None,
                },
                enforcement: ErRelationshipEnforcement {
                    kind: ErEnforcementKind::DeclaredForeignKey,
                    constraint_ref: Some(constraint),
                    enforced: None,
                },
                validity: ErValidity {
                    state: ErValidityState::Current,
                    reason: None,
                },
                description: None,
                evidence_refs: Vec::new(),
            })
        })
        .collect()
}

/// 归一化列类型名用于可比较性比较：去掉参数与大小写差异，折叠同义拼写。
fn normalized_column_type(type_name: &str) -> String {
    let base = type_name
        .split('(')
        .next()
        .unwrap_or(type_name)
        .trim()
        .to_lowercase();
    match base.as_str() {
        "int" | "int4" | "integer" | "int32" | "serial" | "serial4" | "tinyint" | "mediumint" => {
            "integer"
        }
        "int8" | "bigint" | "int64" | "bigserial" | "serial8" => "bigint",
        "int2" | "smallint" | "smallserial" | "serial2" => "smallint",
        "bool" | "boolean" => "boolean",
        "varchar" | "character varying" | "nvarchar" | "text" | "char"
        | "character" | "bpchar" | "clob" | "longtext" | "mediumtext" | "tinytext" => "string",
        "numeric" | "decimal" => "decimal",
        "float" | "float4" | "real" => "float",
        "double" | "float8" => "double",
        "timestamptz" => "timestamp",
        _ => base.as_str(),
    }
    .to_string()
}

/// 两列是否可比较（等值 JOIN 的必要条件）。
///
/// 类型未知（`None` 或空串）时返回 false：无法证明可比较就不得判定可用，由
/// `ErUsageContext::pairs_resolvable` 表达为不完整，而不是乐观放行。
pub fn er_column_types_comparable(left: Option<&str>, right: Option<&str>) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    if left.trim().is_empty() || right.trim().is_empty() {
        return false;
    }
    normalized_column_type(left) == normalized_column_type(right)
}

/// 字面量与列类型是否匹配（`required_filters` 的必要条件）。
///
/// `ErLiteral` 只承载字面量常量，In 目前只支持空列表形态，因此这里只做
/// 「字面量族 ↔ 列类型族」的一致性判断，不解析任何表达式。
pub fn er_literal_matches_column(literal: &ErLiteral, column_type: Option<&str>) -> bool {
    let Some(column_type) = column_type.filter(|value| !value.trim().is_empty()) else {
        return false;
    };
    let column = normalized_column_type(column_type);
    match literal {
        // 空值语义（IS NULL / IS NOT NULL）与 In 的空列表：不依赖列类型取值。
        ErLiteral::Null => true,
        ErLiteral::Bool(_) => column == "boolean",
        ErLiteral::Int(_) => matches!(column.as_str(), "integer" | "bigint" | "smallint"),
        ErLiteral::Float(_) => {
            matches!(column.as_str(), "decimal" | "float" | "double" | "integer" | "bigint")
        }
        ErLiteral::Text(_) => matches!(column.as_str(), "string" | "uuid" | "date" | "timestamp" | "time"),
    }
}

/// 取列定义中的类型名（`ErColumn::type_name` 为 Option 的统一入口）。
pub fn er_column_type(column: &ErColumn) -> Option<&str> {
    column.type_name.as_deref()
}

#[cfg(test)]
mod er_relationship_identity_tests {
    use super::*;

    fn table(schema: Option<&str>, name: &str) -> ErTableRef {
        ErTableRef {
            database: "sales".into(),
            schema: schema.map(str::to_string),
            name: name.into(),
        }
    }

    fn edge(
        from: ErTableRef,
        to: ErTableRef,
        constraint: &str,
        from_column: &str,
        to_column: &str,
    ) -> ErForeignKeyEdge {
        ErForeignKeyEdge {
            name: constraint.into(),
            from_table: from.display(),
            from_column: from_column.into(),
            to_table: to.display(),
            to_column: to_column.into(),
            from_reference: from,
            to_reference: to,
        }
    }

    fn relationship(
        left: &ErTableRef,
        right: &ErTableRef,
        role: &str,
        pairs: &[(&str, &str)],
    ) -> ErRelationship {
        let unknown = || ErCardinality {
            min: ErCardinalityBound::Unknown,
            max: ErCardinalityBound::Unknown,
        };
        ErRelationship {
            id: format!("r-{role}"),
            revision: 1,
            left_entity: er_entity_id(left),
            right_entity: er_entity_id(right),
            role: role.into(),
            column_pairs: pairs
                .iter()
                .map(|(left_column, right_column)| ErColumnPair {
                    left_column: er_column_id(left, left_column),
                    right_column: er_column_id(right, right_column),
                })
                .collect(),
            required_filters: Vec::new(),
            match_cardinality: ErMatchCardinality {
                left_to_right: unknown(),
                right_to_left: unknown(),
                basis: ErCardinalityBasis::Unknown,
            },
            origin: ErRelationshipOrigin::User,
            review: ErRelationshipReview {
                state: ErReviewState::Proposed,
                confirmed_revision: None,
                confirmed_by: None,
            },
            enforcement: ErRelationshipEnforcement {
                kind: ErEnforcementKind::None,
                constraint_ref: None,
                enforced: None,
            },
            validity: ErValidity {
                state: ErValidityState::Current,
                reason: None,
            },
            description: None,
            evidence_refs: Vec::new(),
        }
    }

    #[test]
    fn composite_foreign_key_merges_into_one_ordered_relationship() {
        let orders = table(None, "orders");
        let customers = table(None, "customers");
        let edges = vec![
            edge(
                orders.clone(),
                customers.clone(),
                "fk_orders_customer",
                "tenant_id",
                "tenant_id",
            ),
            edge(
                orders.clone(),
                customers.clone(),
                "fk_orders_customer",
                "customer_id",
                "id",
            ),
        ];

        let derived = er_foreign_key_relationships(&edges);

        assert_eq!(derived.len(), 1, "同一复合外键的逐列边必须合并成一条关系");
        let relation = &derived[0];
        assert_eq!(relation.role, "fk_orders_customer");
        assert_eq!(relation.origin, ErRelationshipOrigin::DatabaseConstraint);
        assert_eq!(
            relation.enforcement.kind,
            ErEnforcementKind::DeclaredForeignKey
        );
        assert_eq!(
            relation.enforcement.constraint_ref.as_deref(),
            Some("fk_orders_customer")
        );
        // 连接器未暴露外键是否生效，如实留空而不是谎称已强制。
        assert_eq!(relation.enforcement.enforced, None);
        assert_eq!(relation.column_pairs.len(), 2);
        assert_eq!(
            er_column_id_name(&relation.column_pairs[1].left_column),
            "customer_id",
            "复合配对必须完整保留，不能只留一列"
        );
    }

    #[test]
    fn identity_key_ignores_direction_but_keeps_role_and_pairs() {
        let orders = table(None, "orders");
        let customers = table(None, "customers");
        let forward = relationship(&orders, &customers, "customer", &[("customer_id", "id")]);
        let reversed = relationship(&customers, &orders, "customer", &[("id", "customer_id")]);

        assert_eq!(
            er_relationship_identity_key(&forward),
            er_relationship_identity_key(&reversed),
            "同一条关系反向书写应去重"
        );

        let shipments = table(None, "shipments");
        let other_role = relationship(&orders, &shipments, "ship_to", &[("shipment_id", "id")]);
        assert_ne!(
            er_relationship_identity_key(&forward),
            er_relationship_identity_key(&other_role),
            "不同角色不得误去重"
        );
        let other_pairs = relationship(&orders, &customers, "customer", &[("buyer_id", "id")]);
        assert_ne!(
            er_relationship_identity_key(&forward),
            er_relationship_identity_key(&other_pairs),
            "字段配对不同不得误去重"
        );
    }

    #[test]
    fn foreign_key_and_matching_logical_relationship_share_identity() {
        let orders = table(None, "orders");
        let customers = table(None, "customers");
        let derived = er_foreign_key_relationships(&[edge(
            orders.clone(),
            customers.clone(),
            "fk_orders_customer",
            "customer_id",
            "id",
        )]);
        let mut logical = relationship(
            &orders,
            &customers,
            "fk_orders_customer",
            &[("customer_id", "id")],
        );

        assert_eq!(
            er_relationship_identity_key(&derived[0]),
            er_relationship_identity_key(&logical),
            "同一条约束被登记成本地关系时必须去重"
        );
        logical.role = "customer".into();
        assert_ne!(
            er_relationship_identity_key(&derived[0]),
            er_relationship_identity_key(&logical),
            "本地关系改用业务角色名时是另一条记录，不得合并"
        );
    }

    #[test]
    fn same_table_in_different_schemas_has_distinct_identity() {
        let public_orders = table(Some("public"), "orders");
        let archive_orders = table(Some("archive"), "orders");
        let forward = relationship(
            &public_orders,
            &table(Some("public"), "customers"),
            "c",
            &[("customer_id", "id")],
        );
        let archived = relationship(
            &archive_orders,
            &table(Some("archive"), "customers"),
            "c",
            &[("customer_id", "id")],
        );

        assert_ne!(er_entity_id(&public_orders), er_entity_id(&archive_orders));
        assert_ne!(
            er_relationship_identity_key(&forward),
            er_relationship_identity_key(&archived),
            "跨 schema 同名表必须按结构化身份区分"
        );
    }

    #[test]
    fn column_types_comparable_normalizes_aliases_and_rejects_unknown() {
        assert!(er_column_types_comparable(Some("INT4"), Some("integer")));
        assert!(er_column_types_comparable(Some("VARCHAR(64)"), Some("text")));
        assert!(er_column_types_comparable(Some("BIGINT"), Some("int8")));
        assert!(!er_column_types_comparable(
            Some("varchar"),
            Some("bigint")
        ));
        assert!(
            !er_column_types_comparable(None, Some("integer")),
            "类型未知时不得乐观判定可比较"
        );
    }

    #[test]
    fn literal_must_match_column_type_family() {
        assert!(er_literal_matches_column(
            &ErLiteral::Int(0),
            Some("smallint")
        ));
        assert!(er_literal_matches_column(
            &ErLiteral::Text("x".into()),
            Some("varchar(20)")
        ));
        assert!(!er_literal_matches_column(
            &ErLiteral::Int(0),
            Some("varchar(20)")
        ));
        assert!(!er_literal_matches_column(&ErLiteral::Text("x".into()), None));
        assert!(
            er_literal_matches_column(&ErLiteral::Null, Some("varchar(20)")),
            "IS NULL 语义不依赖列类型取值"
        );
    }

    #[test]
    fn entity_id_round_trips_through_parts() {
        let reference = table(Some("public"), "order.items");
        let (database, schema, name) =
            er_entity_id_parts(&er_entity_id(&reference)).expect("实体 ID 必须可反解");

        assert_eq!(database, "sales");
        assert_eq!(schema.as_deref(), Some("public"));
        assert_eq!(name, "order.items", "表名含分隔符时也不能被拆错");
        assert_eq!(er_entity_id_parts("bad"), None, "段数不足时不猜表");
    }
}

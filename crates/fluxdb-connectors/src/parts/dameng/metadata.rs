/// 达梦会为外键等约束创建 `SREF_CON_*` 内部表，另有 `##` 前缀的临时/系统对象。
/// 它们不对应可浏览的用户关系，补全与对象树统一隐藏，避免生成不可引用的候选。
fn dm_internal_object_name(name: &str) -> bool {
    let name = name.trim().to_ascii_uppercase();
    name.starts_with("SREF_CON_") || name.starts_with("##")
}

#[cfg(test)]
mod dameng_internal_object_tests {
    use super::dm_internal_object_name;

    #[test]
    fn hides_dameng_foreign_key_and_temporary_tables() {
        assert!(dm_internal_object_name("SREF_CON_TAB134218794_LEVEL"));
        assert!(dm_internal_object_name("##DM_TEMP_TABLE"));
        assert!(!dm_internal_object_name("CUSTOMERS"));
        assert!(!dm_internal_object_name("V_CUSTOMER_VALUE"));
    }
}

fn dm_list_objects(
    config: &ConnectionConfig,
    path: Option<&ObjectPath>,
) -> fluxdb_core::Result<Vec<ObjectSummary>> {
    if config.kind != DatabaseKind::Dameng {
        return Err(Error::new(ErrorKind::Connection, "连接类型不匹配"));
    }
    match path {
        None => dm_list_databases(config),
        Some(path) if path.kind == ObjectKind::Database => dm_list_schemas(config, path),
        Some(path) if path.kind == ObjectKind::Schema => dm_list_relations(config, path),
        Some(_) => Err(Error::new(ErrorKind::Internal, "不支持的达梦对象层级")),
    }
}

fn dm_list_databases(config: &ConnectionConfig) -> fluxdb_core::Result<Vec<ObjectSummary>> {
    let mut client = dm_connect(config)?;
    let result = dm_execute(&mut client, "SELECT NAME FROM V$DATABASE", &[])?;
    client.close();
    let ExecuteResult::Query { rows, .. } = result else {
        return Err(Error::new(ErrorKind::Query, "达梦数据库列表返回类型异常"));
    };
    rows.into_iter()
        .filter_map(|row| row.first().and_then(dm_value_text))
        .map(|name| {
            Ok(ObjectSummary {
                path: ObjectPath {
                    connection_id: config.id,
                    database: Some(name.clone()),
                    schema: None,
                    name,
                    kind: ObjectKind::Database,
                },
                rows: None,
                modified_at: None,
                comment: None,
                stable: None,
            })
        })
        .collect()
}

fn dm_list_schemas(
    config: &ConnectionConfig,
    path: &ObjectPath,
) -> fluxdb_core::Result<Vec<ObjectSummary>> {
    let database = path
        .database
        .clone()
        .or_else(|| path.name.clone().into());
    let mut client = dm_connect(config)?;
    let result = dm_execute(
        &mut client,
        "SELECT NAME FROM SYS.SYSOBJECTS WHERE TYPE$='SCH' ORDER BY NAME",
        &[],
    )?;
    client.close();
    let ExecuteResult::Query { rows, .. } = result else {
        return Err(Error::new(ErrorKind::Query, "达梦 schema 列表返回类型异常"));
    };
    rows.into_iter()
        .filter_map(|row| row.first().and_then(dm_value_text))
        .map(|name| {
            Ok(ObjectSummary {
                path: ObjectPath {
                    connection_id: config.id,
                    database: database.clone(),
                    schema: None,
                    name,
                    kind: ObjectKind::Schema,
                },
                rows: None,
                modified_at: None,
                comment: None,
                stable: None,
            })
        })
        .collect()
}

fn dm_list_relations(
    config: &ConnectionConfig,
    path: &ObjectPath,
) -> fluxdb_core::Result<Vec<ObjectSummary>> {
    let database = path.database.clone();
    let schema = path.name.clone();
    let mut client = dm_connect(config)?;
    let result = dm_execute(
        &mut client,
        "SELECT O.OBJECT_NAME, O.OBJECT_TYPE, O.OBJECT_ID, C.COMMENTS \
         FROM ALL_OBJECTS O \
         LEFT JOIN ALL_TAB_COMMENTS C ON C.OWNER=O.OWNER AND C.TABLE_NAME=O.OBJECT_NAME \
         WHERE O.OWNER=? AND O.OBJECT_TYPE IN ('TABLE','VIEW') \
         ORDER BY O.OBJECT_NAME",
        &[Param::Str(schema.clone())],
    )?;
    client.close();
    let ExecuteResult::Query { rows, .. } = result else {
        return Err(Error::new(ErrorKind::Query, "达梦对象列表返回类型异常"));
    };
    let mut objects = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(name) = row.first().and_then(dm_value_text) else {
            continue;
        };
        if dm_internal_object_name(&name) {
            continue;
        }
        let object_type = row.get(1).and_then(dm_value_text).unwrap_or_default();
        let stable = row.get(2).and_then(dm_value_u64);
        let comment = row.get(3).and_then(dm_value_text);
        objects.push(ObjectSummary {
            path: ObjectPath {
                connection_id: config.id,
                database: database.clone(),
                schema: Some(schema.clone()),
                name,
                kind: if object_type.eq_ignore_ascii_case("VIEW") {
                    ObjectKind::View
                } else {
                    ObjectKind::Table
                },
            },
            rows: None,
            modified_at: None,
            comment,
            stable,
        });
    }
    Ok(objects)
}

fn dm_load_columns(
    client: &mut DmClient,
    schema: &str,
    table: &str,
) -> fluxdb_core::Result<Vec<Column>> {
    let result = dm_execute(
        client,
        "SELECT C.COLUMN_NAME, C.DATA_TYPE, C.DATA_LENGTH, C.DATA_PRECISION, C.DATA_SCALE, \
                C.NULLABLE, CC.COMMENTS, \
                CASE WHEN PK.COLUMN_NAME IS NULL THEN 0 ELSE 1 END AS IS_PK \
         FROM ALL_TAB_COLUMNS C \
         LEFT JOIN ALL_COL_COMMENTS CC ON CC.OWNER=C.OWNER AND CC.TABLE_NAME=C.TABLE_NAME \
                                      AND CC.COLUMN_NAME=C.COLUMN_NAME \
         LEFT JOIN (SELECT CC.OWNER, CC.TABLE_NAME, CC.COLUMN_NAME \
                    FROM ALL_CONSTRAINTS CO JOIN ALL_CONS_COLUMNS CC \
                      ON CC.OWNER=CO.OWNER AND CC.CONSTRAINT_NAME=CO.CONSTRAINT_NAME \
                    WHERE CO.CONSTRAINT_TYPE='P') PK \
           ON PK.OWNER=C.OWNER AND PK.TABLE_NAME=C.TABLE_NAME AND PK.COLUMN_NAME=C.COLUMN_NAME \
         WHERE C.OWNER=? AND C.TABLE_NAME=? ORDER BY C.COLUMN_ID",
        &[Param::Str(schema.to_string()), Param::Str(table.to_string())],
    )?;
    let ExecuteResult::Query { rows, .. } = result else {
        return Err(Error::new(ErrorKind::Query, "达梦列元数据返回类型异常"));
    };
    rows.into_iter()
        .map(|row| {
            let name = row.first().and_then(dm_value_text).ok_or_else(|| {
                Error::new(ErrorKind::Query, "达梦列元数据缺少字段名")
            })?;
            let data_type = row.get(1).and_then(dm_value_text).unwrap_or_default();
            let length = row.get(2).and_then(dm_meta_u64).unwrap_or(0);
            let precision = row.get(3).and_then(dm_meta_u64);
            let scale = row.get(4).and_then(dm_meta_u64).unwrap_or(0);
            let upper = data_type.to_ascii_uppercase();
            let type_name = if matches!(upper.as_str(), "VARCHAR" | "VARCHAR2" | "CHAR") {
                format!("{data_type}({length})")
            } else if matches!(upper.as_str(), "DECIMAL" | "NUMERIC") {
                precision
                    .map(|precision| format!("{data_type}({precision},{scale})"))
                    .unwrap_or(data_type)
            } else {
                data_type
            };
            Ok(Column {
                name,
                type_name: Some(type_name),
                nullable: row
                    .get(5)
                    .and_then(dm_value_text)
                    .is_some_and(|value| value.eq_ignore_ascii_case("Y")),
                primary_key: row.get(7).and_then(dm_meta_i64).unwrap_or(0) != 0,
                comment: row.get(6).and_then(dm_value_text),
            })
        })
        .collect()
}

fn dm_indexes_for(
    config: &ConnectionConfig,
    path: &ObjectPath,
) -> fluxdb_core::Result<Vec<IndexInfo>> {
    let schema = dm_request_schema(config, path)?;
    let mut client = dm_connect(config)?;
    let result = dm_execute(
        &mut client,
        "SELECT I.INDEX_NAME, I.UNIQUENESS, I.INDEX_TYPE, IC.COLUMN_NAME, IC.COLUMN_POSITION, \
                CASE WHEN C.CONSTRAINT_TYPE='P' THEN 1 ELSE 0 END AS IS_PK \
         FROM ALL_INDEXES I \
         LEFT JOIN ALL_IND_COLUMNS IC ON IC.INDEX_OWNER=I.OWNER AND IC.INDEX_NAME=I.INDEX_NAME \
         LEFT JOIN ALL_CONSTRAINTS C ON C.OWNER=I.OWNER AND C.INDEX_NAME=I.INDEX_NAME \
         WHERE I.OWNER=? AND I.TABLE_NAME=? ORDER BY I.INDEX_NAME, IC.COLUMN_POSITION",
        &[Param::Str(schema), Param::Str(path.name.clone())],
    )?;
    client.close();
    let ExecuteResult::Query { rows, .. } = result else {
        return Err(Error::new(ErrorKind::Query, "达梦索引元数据返回类型异常"));
    };
    let mut indexes: Vec<IndexInfo> = Vec::new();
    for row in rows {
        let name = row.first().and_then(dm_value_text).unwrap_or_default();
        let unique = row
            .get(1)
            .and_then(dm_value_text)
            .is_some_and(|value| value.eq_ignore_ascii_case("UNIQUE"));
        let index_type = row.get(2).and_then(dm_value_text);
        let column = row.get(3).and_then(dm_value_text);
        let is_primary = row.get(5).and_then(dm_meta_i64).unwrap_or(0) != 0;
        if let Some(index) = indexes.iter_mut().find(|index| index.name == name) {
            if let Some(column) = column {
                index.columns.push(column);
            }
            continue;
        }
        indexes.push(IndexInfo {
            name,
            columns: column.into_iter().collect(),
            is_unique: unique,
            is_primary,
            index_type,
            comment: None,
        });
    }
    Ok(indexes)
}

fn dm_foreign_keys_for(
    config: &ConnectionConfig,
    path: &ObjectPath,
) -> fluxdb_core::Result<Vec<ForeignKeyInfo>> {
    let schema = dm_request_schema(config, path)?;
    let mut client = dm_connect(config)?;
    let result = dm_execute(
        &mut client,
        "SELECT C.CONSTRAINT_NAME, CC.COLUMN_NAME, R.OWNER, R.TABLE_NAME, RCC.COLUMN_NAME, CC.POSITION \
         FROM ALL_CONSTRAINTS C \
         JOIN ALL_CONS_COLUMNS CC ON CC.OWNER=C.OWNER AND CC.CONSTRAINT_NAME=C.CONSTRAINT_NAME \
         JOIN ALL_CONSTRAINTS R ON R.OWNER=C.R_OWNER AND R.CONSTRAINT_NAME=C.R_CONSTRAINT_NAME \
         JOIN ALL_CONS_COLUMNS RCC ON RCC.OWNER=R.OWNER AND RCC.CONSTRAINT_NAME=R.CONSTRAINT_NAME \
                                  AND RCC.POSITION=CC.POSITION \
         WHERE C.OWNER=? AND C.TABLE_NAME=? AND C.CONSTRAINT_TYPE='R' \
         ORDER BY C.CONSTRAINT_NAME, CC.POSITION",
        &[Param::Str(schema), Param::Str(path.name.clone())],
    )?;
    client.close();
    let ExecuteResult::Query { rows, .. } = result else {
        return Err(Error::new(ErrorKind::Query, "达梦外键元数据返回类型异常"));
    };
    let mut foreign_keys: Vec<ForeignKeyInfo> = Vec::new();
    for row in rows {
        let name = row.first().and_then(dm_value_text).unwrap_or_default();
        let column = row.get(1).and_then(dm_value_text).unwrap_or_default();
        let ref_schema = row.get(2).and_then(dm_value_text);
        let ref_table = row.get(3).and_then(dm_value_text).unwrap_or_default();
        let ref_column = row.get(4).and_then(dm_value_text).unwrap_or_default();
        if let Some(fk) = foreign_keys.iter_mut().find(|fk| fk.name == name) {
            fk.columns.push(column.clone());
            fk.ref_columns.push(ref_column);
            fk.column = fk.columns.first().cloned().unwrap_or(column);
            fk.ref_column = fk.ref_columns.first().cloned().unwrap_or_default();
        } else {
            foreign_keys.push(ForeignKeyInfo {
                name,
                column: column.clone(),
                ref_schema,
                ref_table,
                ref_column: ref_column.clone(),
                columns: vec![column],
                ref_columns: vec![ref_column],
            });
        }
    }
    Ok(foreign_keys)
}

fn dm_table_ddl_for(
    config: &ConnectionConfig,
    path: &ObjectPath,
) -> fluxdb_core::Result<String> {
    let schema = dm_request_schema(config, path)?;
    let mut client = dm_connect(config)?;
    let columns = dm_load_columns(&mut client, &schema, &path.name)?;
    let indexes = dm_indexes_for(config, path)?;
    let foreign_keys = dm_foreign_keys_for(config, path)?;
    let comments = dm_execute(
        &mut client,
        "SELECT COMMENTS FROM ALL_TAB_COMMENTS WHERE OWNER=? AND TABLE_NAME=?",
        &[Param::Str(schema.clone()), Param::Str(path.name.clone())],
    )?;
    client.close();
    let table_comment = match comments {
        ExecuteResult::Query { rows, .. } => rows
            .first()
            .and_then(|row| row.first())
            .and_then(dm_value_text),
        _ => None,
    };
    let table = dm_qualified_table(&schema, &path.name);
    let mut lines = Vec::new();
    for column in &columns {
        let mut line = format!(
            "  {} {}",
            fluxdb_core::DamengDialect.quote_identifier(&column.name),
            column.type_name.as_deref().unwrap_or("VARCHAR(1)")
        );
        if !column.nullable {
            line.push_str(" NOT NULL");
        }
        lines.push(line);
    }
    let pk_columns = columns
        .iter()
        .filter(|column| column.primary_key)
        .map(|column| fluxdb_core::DamengDialect.quote_identifier(&column.name))
        .collect::<Vec<_>>();
    if !pk_columns.is_empty() {
        lines.push(format!("  PRIMARY KEY ({})", pk_columns.join(", ")));
    }
    for fk in &foreign_keys {
        if fk.columns.len() != fk.ref_columns.len() || fk.columns.is_empty() {
            continue;
        }
        let source = fk
            .columns
            .iter()
            .map(|column| fluxdb_core::DamengDialect.quote_identifier(column))
            .collect::<Vec<_>>()
            .join(", ");
        let target = fk
            .ref_columns
            .iter()
            .map(|column| fluxdb_core::DamengDialect.quote_identifier(column))
            .collect::<Vec<_>>()
            .join(", ");
        let ref_table = dm_qualified_table(
            fk.ref_schema.as_deref().unwrap_or(&schema),
            &fk.ref_table,
        );
        lines.push(format!(
            "  CONSTRAINT {} FOREIGN KEY ({source}) REFERENCES {ref_table} ({target})",
            fluxdb_core::DamengDialect.quote_identifier(&fk.name)
        ));
    }
    let mut ddl = format!("CREATE TABLE {table} (\n{}\n)", lines.join(",\n"));
    for index in indexes.iter().filter(|index| !index.is_primary) {
        let cols = index
            .columns
            .iter()
            .map(|column| fluxdb_core::DamengDialect.quote_identifier(column))
            .collect::<Vec<_>>()
            .join(", ");
        ddl.push_str(&format!(
            ";\nCREATE {}INDEX {} ON {table} ({cols})",
            if index.is_unique { "UNIQUE " } else { "" },
            fluxdb_core::DamengDialect.quote_identifier(&index.name)
        ));
    }
    if let Some(comment) = table_comment.filter(|comment| !comment.trim().is_empty()) {
        ddl.push_str(&format!(
            ";\nCOMMENT ON TABLE {table} IS {}",
            dm_string_literal(&comment)
        ));
    }
    for column in columns.iter().filter(|column| column.comment.is_some()) {
        ddl.push_str(&format!(
            ";\nCOMMENT ON COLUMN {table}.{} IS {}",
            fluxdb_core::DamengDialect.quote_identifier(&column.name),
            dm_string_literal(column.comment.as_deref().unwrap_or_default())
        ));
    }
    Ok(format!("{ddl};"))
}

fn dm_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

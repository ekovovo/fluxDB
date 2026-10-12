/// 按连接配置创建统一的 Connector。
///
/// 数据库类型到具体实现的映射只保留在 connector 层；上层只依赖 `Connector` trait。
/// demo 连接统一使用内存实现，尚未实现真实连接器的类型在这里明确返回不支持。
pub fn connector_for(config: &ConnectionConfig) -> fluxdb_core::Result<Box<dyn Connector>> {
    if config
        .options
        .get("demo")
        .is_some_and(|value| value == "true")
    {
        // `demo.apply_delay_ms` 让 headless UI 测试能稳定复现「数据库响应慢」：
        // 只作用于内存 demo 连接，生产连接配置不会带这个键。
        let delay = config
            .options
            .get("demo.apply_delay_ms")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        let fail_submit = config
            .options
            .get("demo.fail_submit")
            .is_some_and(|value| value == "true");
        return Ok(Box::new(MockConnector::with_apply_delay(
            config.kind,
            std::time::Duration::from_millis(delay),
        )
        .with_fail_submit(fail_submit)));
    }

    let connector: Box<dyn Connector> = match config.kind {
        DatabaseKind::MySql | DatabaseKind::TiDb => {
            Box::new(MySqlConnector::with_config(config.clone()))
        }
        DatabaseKind::Sqlite => Box::new(SqliteConnector::with_config(config.clone())),
        DatabaseKind::Postgres => Box::new(PostgresConnector::with_config(config.clone())),
        DatabaseKind::Redis => Box::new(RedisConnector::with_config(config.clone())),
        DatabaseKind::MongoDb => {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "MongoDB 连接尚未支持，不展示对象数据",
            ));
        }
    };
    Ok(connector)
}

#[cfg(test)]
mod connector_factory_tests {
    use super::*;

    fn config(kind: DatabaseKind, demo: bool) -> ConnectionConfig {
        let mut options = BTreeMap::new();
        if demo {
            options.insert("demo".to_string(), "true".to_string());
        }
        ConnectionConfig {
            id: ConnectionId(1),
            name: "factory test".to_string(),
            kind,
            endpoint: Endpoint::Tcp {
                host: "127.0.0.1".to_string(),
                port: 0,
                database: None,
            },
            options,
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
        }
    }

    #[test]
    fn creates_connector_for_each_supported_database_kind() {
        for (kind, connector_kind) in [
            (DatabaseKind::MySql, DatabaseKind::MySql),
            // TiDB 复用 MySQL 协议连接器，连接器自身如实报告 MySQL 能力族。
            (DatabaseKind::TiDb, DatabaseKind::MySql),
            (DatabaseKind::Sqlite, DatabaseKind::Sqlite),
            (DatabaseKind::Postgres, DatabaseKind::Postgres),
            (DatabaseKind::Redis, DatabaseKind::Redis),
        ] {
            let config = config(kind, false);
            let connector = connector_for(&config).expect("supported connector");
            assert_eq!(connector.kind(), connector_kind);
        }
    }
}

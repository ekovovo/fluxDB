impl AppController {
    // 不能通过递归 dispatch 补默认参数：大型命令分派帧会重复占栈，
    // 在 macOS GCD 的小栈后台线程中挤掉 SQLx 握手/收包所需空间。
    fn execute_query_text_with_options(
        &mut self,
        tab_id: TabId,
        text: String,
        options: QueryExecutionOptions,
    ) -> AppEvent {
        let request = self.find_tab(tab_id).and_then(|tab| match &tab.kind {
            TabKind::QueryEditor(editor) => Some(QueryRequest {
                connection_id: editor.connection_id,
                database: editor.database.clone(),
                // 查询编辑器标签 = 一个独占 PG 会话（设计 §3.3）：事务/临时表/SET 跨多次执行保持。
                session_id: Some(fluxdb_core::QuerySessionId(tab_id.0)),
                schema: editor.schema.clone(),
                text: sql_text_for_execution(
                    &text,
                    options.page_size,
                    self.connection_kind(editor.connection_id)
                        .unwrap_or(DatabaseKind::MySql),
                ),
                mode: fluxdb_core::QueryMode::Selection,
                options,
            }),
            _ => None,
        });

        let Some(request) = request else {
            return self.fail(Error::new(ErrorKind::Internal, "查询编辑器标签页不存在"));
        };

        let cancel_flag = self.query_cancel_flag(tab_id);
        let result =
            self.execute_query_with_cancel(&request, &|| query_cancel_requested(&cancel_flag));
        self.clear_query_cancel_flag(tab_id);
        match result {
            Ok(execution) => {
                let result_editors = query_result_editors(self, &request, &execution);
                let active_result_editor = result_editors.keys().next().copied();
                if let Some(tab) = self.find_tab_mut(tab_id)
                    && let TabKind::QueryEditor(editor) = &mut tab.kind
                {
                    editor.running = false;
                    editor.results = execution.results.clone();
                    editor.result_editors = result_editors;
                    editor.active_result_editor = active_result_editor;
                    editor.summaries = execution.summaries.clone();
                    editor.error = None;
                    tab.dirty = false;
                }
                self.record_query_execution_history(&request, &execution);
                AppEvent::QueryFinished(tab_id, execution)
            }
            Err(error) => {
                self.record_failed_query_history(&request);
                let user_error = UserFacingError::from(error);
                if let Some(tab) = self.find_tab_mut(tab_id)
                    && let TabKind::QueryEditor(editor) = &mut tab.kind
                {
                    editor.running = false;
                    editor.summaries = Vec::new();
                    editor.result_editors.clear();
                    editor.active_result_editor = None;
                    editor.error = Some(user_error.clone());
                }
                self.state.last_error = Some(user_error.clone());
                AppEvent::Failed(user_error)
            }
        }
    }
}

#[cfg(test)]
mod query_execution_stack_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    #[test]
    fn text_commands_preserve_options_and_publish_results() {
        for explicit in [false, true] {
            let mut controller = AppController::new();
            let config = ConnectionConfig {
                id: ConnectionId(1),
                name: "query-options-test".into(),
                kind: DatabaseKind::Sqlite,
                endpoint: fluxdb_core::Endpoint::SqliteFile {
                    path: ":memory:".into(),
                    read_only: false,
                },
                options: BTreeMap::new(),
                mysql_profile: None,
                postgres_profile: None,
                dameng_profile: None,
                redis_profile: None,
            };
            controller.dispatch(AppCommand::ReplaceConnections(vec![config]));
            controller.dispatch(AppCommand::OpenQueryEditor(ConnectionId(1)));
            let tab_id = controller.state().active_tab.unwrap();
            controller.dispatch(AppCommand::StartQueryExecution(tab_id));
            let text = "SELECT 1 AS value UNION ALL SELECT 2 UNION ALL SELECT 3".to_string();
            let command = if explicit {
                AppCommand::ExecuteQueryTextWithOptions {
                    tab_id,
                    text,
                    options: QueryExecutionOptions {
                        page_size: 2,
                        read_only: true,
                        ..QueryExecutionOptions::default()
                    },
                }
            } else {
                AppCommand::ExecuteQueryText { tab_id, text }
            };

            let AppEvent::QueryFinished(result_tab, execution) = controller.dispatch(command)
            else {
                panic!("in-memory SELECT must return a query result");
            };

            assert_eq!(result_tab, tab_id);
            assert_eq!(execution.results.len(), 1);
            let page = &execution.results[0];
            assert_eq!(page.columns[0].name, "value");
            let values: Vec<_> = page.rows.iter().map(|row| row.values[0].clone()).collect();
            let expected = if explicit { vec![1, 2] } else { vec![1, 2, 3] };
            assert_eq!(
                values,
                expected.into_iter().map(CellValue::I64).collect::<Vec<_>>()
            );
            if explicit {
                assert_eq!(page.limit, 2);
            }
            let TabKind::QueryEditor(editor) = &controller.state().active_tab().unwrap().kind
            else {
                panic!("query tab must remain open");
            };
            assert!(!editor.running);
            assert!(editor.error.is_none());
            assert_eq!(editor.results, execution.results);
            assert_eq!(editor.summaries, execution.summaries);
            assert_eq!(controller.state().query_history.len(), 1);
            assert!(controller.query_cancel_flag(tab_id).is_none());
        }
    }

    #[test]
    fn text_commands_on_small_stack_handle_mysql_handshake_failure() {
        const CHILD_PORT: &str = "FLUXDB_QUERY_STACK_TEST_PORT";
        const CHILD_MODE: &str = "FLUXDB_QUERY_STACK_TEST_MODE";
        const TEST_NAME: &str = "query_execution_stack_tests::text_commands_on_small_stack_handle_mysql_handshake_failure";

        if let Ok(port) = std::env::var(CHILD_PORT) {
            let mut controller = AppController::new();
            let config = ConnectionConfig {
                id: ConnectionId(1),
                name: "query-stack-test".into(),
                kind: DatabaseKind::MySql,
                endpoint: fluxdb_core::Endpoint::Tcp {
                    host: "127.0.0.1".into(),
                    port: port.parse().unwrap(),
                    database: None,
                },
                options: BTreeMap::new(),
                mysql_profile: None,
                postgres_profile: None,
                dameng_profile: None,
                redis_profile: None,
            };
            controller.dispatch(AppCommand::ReplaceConnections(vec![config]));
            controller.dispatch(AppCommand::OpenQueryEditor(ConnectionId(1)));
            let tab_id = controller.state().active_tab.unwrap();
            controller.dispatch(AppCommand::StartQueryExecution(tab_id));
            let command = if std::env::var(CHILD_MODE).unwrap() == "default" {
                AppCommand::ExecuteQueryText {
                    tab_id,
                    text: "SELECT 1".into(),
                }
            } else {
                AppCommand::ExecuteQueryTextWithOptions {
                    tab_id,
                    text: "SELECT 1".into(),
                    options: QueryExecutionOptions::default(),
                }
            };
            // 与 macOS GCD 一样使用小栈；溢出会终止子进程，不能拖垮其他用例。
            std::thread::Builder::new()
                .stack_size(512 * 1024)
                .spawn(move || {
                    let AppEvent::Failed(error) = controller.dispatch(command) else {
                        panic!("incomplete TLS handshake must report a connection error");
                    };
                    assert_eq!(error.title, "连接失败");
                    assert!(error.retryable);
                    assert_eq!(controller.state().last_error.as_ref(), Some(&error));
                    let TabKind::QueryEditor(editor) =
                        &controller.state().active_tab().unwrap().kind
                    else {
                        panic!("query tab must remain open");
                    };
                    assert!(!editor.running);
                    assert_eq!(editor.error.as_ref(), Some(&error));
                    assert!(editor.results.is_empty());
                    assert!(editor.summaries.is_empty());
                })
                .unwrap()
                .join()
                .unwrap();
            return;
        }

        for mode in ["default", "explicit"] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST_NAME, "--nocapture"])
                .env(
                    CHILD_PORT,
                    listener.local_addr().unwrap().port().to_string(),
                )
                .env(CHILD_MODE, mode)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
                            let _ = child.kill();
                            let output = child.wait_with_output().unwrap();
                            panic!("{mode}: no handshake connection: {output:?}");
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{mode}: accept failed: {error}"),
                }
            };
            // macOS 的 accept 会继承 listener 的非阻塞标志，读包前显式恢复。
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();

            // 仅模拟支持 TLS 的 MySQL greeting；不认证，也不接收或执行 SQL。
            let capabilities: u32 = 1 | 4 | 512 | 2048 | 8192 | 32768 | 131072 | 524288;
            let mut greeting = b"\x0a8.0.36\0\x01\0\0\0abcdefgh\0".to_vec();
            greeting.extend_from_slice(&(capabilities as u16).to_le_bytes());
            greeting.extend_from_slice(b"\x2d\x02\0");
            greeting.extend_from_slice(&((capabilities >> 16) as u16).to_le_bytes());
            greeting.push(21);
            greeting.extend_from_slice(&[0; 10]);
            greeting.extend_from_slice(b"ijklmnopqrst\0mysql_native_password\0");
            let mut packet = (greeting.len() as u32).to_le_bytes();
            packet[3] = 0;
            stream.write_all(&packet).unwrap();
            stream.write_all(&greeting).unwrap();
            let mut ssl_request = [0; 36];
            let handshake = stream.read_exact(&mut ssl_request);
            drop(stream);

            let deadline = Instant::now() + Duration::from_secs(30);
            while child.try_wait().unwrap().is_none() {
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("{mode}: query child timed out");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "{mode}: {output:?}");
            handshake.unwrap();
            assert_eq!(&ssl_request[..3], &[32, 0, 0]);
            let flags = u32::from_le_bytes(ssl_request[4..8].try_into().unwrap());
            assert_ne!(flags & 2048, 0, "{mode}: client must request TLS");
        }
    }
}

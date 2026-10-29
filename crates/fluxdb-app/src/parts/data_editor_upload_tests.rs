// 上传替换的阈值与暂存决策：文件大小决定走内存还是走流式，能力不支持时提前拒绝。
//
// 这里只验证决策结果，不碰真实数据库：`update_binary_cell` 的产物是改动集里的
// `CellUpdateValue`，落库行为由连接器测试覆盖。

use super::*;
use crate::cell_content_store::{CellContentFileStore, lock_content_store_for_test};
use fluxdb_core::CellContentStore as _;

/// 临时根目录：只清理本用例创建的内容。
struct TempRoot(std::path::PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fluxdb-upload-staging-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp root");
        Self(dir)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn editor(type_name: &str) -> DataEditorState {
    let page = DataPage {
        columns: vec![
            fluxdb_core::Column {
                name: "ID".into(),
                type_name: Some("INTEGER".into()),
                nullable: false,
                primary_key: true,
                comment: None,
            },
            fluxdb_core::Column {
                name: "DATA".into(),
                type_name: Some(type_name.into()),
                nullable: true,
                primary_key: false,
                comment: None,
            },
        ],
        rows: vec![fluxdb_core::Row {
            values: vec![
                CellValue::I64(1),
                CellValue::Bytes(vec![0xAA, 0xBB]),
            ],
        }],
        offset: 0,
        limit: 100,
        has_more: false,
        incomplete: None,
    };
    DataEditorState {
        object: ObjectPath {
            connection_id: fluxdb_core::ConnectionId(9_921),
            database: Some("main".into()),
            schema: None,
            name: "blobs".into(),
            kind: fluxdb_core::ObjectKind::Table,
        },
        page: Some(page.clone()),
        original_page: Some(page),
        pagination: Pagination {
            offset: 0,
            limit: 100,
        },
        load_generation: 0,
        changes: None,
        editing_cell: None,
        cell_detail_panel: CellDetailPanelState::default(),
        table_info: TableInfoState::default(),
        loading: false,
        submitting: false,
        error: None,
    }
}

/// 内容存储目录下是否有遗留文件（暂存必须先拒绝、后落盘）。
fn has_staged_files(root: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root.join("cell-contents")) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        path.is_dir() && std::fs::read_dir(&path).is_ok_and(|mut inner| inner.next().is_some())
    })
}

fn write_file(root: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = root.join(name);
    std::fs::write(&path, bytes).expect("write upload file");
    path
}

/// 阈值以下：整份读进内存，走普通参数绑定，不产生暂存内容。
#[test]
fn small_file_stays_in_memory() {
    let _guard = lock_content_store_for_test();
    let root = TempRoot::new("small");
    let store = std::sync::Arc::new(CellContentFileStore::new(&root.0));
    fluxdb_core::register_cell_content_store(store.clone());
    let mut editor = editor("LONGBLOB");
    let path = write_file(&root.0, "small.bin", &[1u8; 1024]);

    update_binary_cell(
        &mut editor,
        0,
        1,
        fluxdb_core::UploadPayload::FilePath(path),
        true,
    )
    .expect("小文件上传");

    let update = &editor.changes.as_ref().expect("改动集").updates[0].cells[0];
    assert_eq!(
        update.value,
        CellUpdateValue::Value(CellValue::Bytes(vec![1u8; 1024])),
        "阈值以下必须整值写入"
    );
}

/// 阈值以上：内容落到暂存存储，改动集里只留不透明引用。
#[test]
fn large_file_is_staged_without_loading_into_memory() {
    let _guard = lock_content_store_for_test();
    let root = TempRoot::new("large");
    let store = std::sync::Arc::new(CellContentFileStore::new(&root.0));
    fluxdb_core::register_cell_content_store(store.clone());
    let mut editor = editor("LONGBLOB");
    let length = fluxdb_core::BLOB_UPLOAD_STREAM_THRESHOLD_BYTES as usize + 1;
    let path = write_file(&root.0, "large.bin", &vec![7u8; length]);

    update_binary_cell(
        &mut editor,
        0,
        1,
        fluxdb_core::UploadPayload::FilePath(path),
        true,
    )
    .expect("大文件上传");

    let update = &editor.changes.as_ref().expect("改动集").updates[0].cells[0];
    let stream = update.value.stream().expect("大文件必须走流式");
    assert_eq!(stream.byte_length, length as u64);
    // 页面显示占位值：只有类型与大小，没有正文。
    assert_eq!(
        update.value.placeholder(),
        &CellValue::BinarySummary(fluxdb_core::BinaryCellSummary {
            type_name: "LONGBLOB".into(),
            is_null: false,
            byte_length: length as u64,
            preview_hex: None,
        })
    );

    // 暂存内容真的落了盘，且按块读回来与源文件一致。
    let meta = store.stat(stream.content_id()).expect("暂存内容必须存在");
    assert_eq!(meta.byte_length, length as u64);
    let first = store
        .read(stream.content_id(), 0, 16)
        .expect("读暂存内容");
    assert_eq!(first, vec![7u8; 16]);
}

/// 存储不支持时，必须先拒绝，不能暂存一份几百 MiB 再在提交阶段失败。
#[test]
fn unsupported_engine_rejects_large_file_before_staging() {
    let _guard = lock_content_store_for_test();
    let root = TempRoot::new("unsupported");
    fluxdb_core::register_cell_content_store(std::sync::Arc::new(CellContentFileStore::new(
        &root.0,
    )));
    let mut editor = editor("LONGBLOB");
    let path = write_file(
        &root.0,
        "large.bin",
        &vec![7u8; fluxdb_core::BLOB_UPLOAD_STREAM_THRESHOLD_BYTES as usize],
    );

    let error = update_binary_cell(
        &mut editor,
        0,
        1,
        fluxdb_core::UploadPayload::FilePath(path),
        false,
    )
    .expect_err("不支持流式的库必须拒绝");
    assert!(error.message.contains("不支持流式写入大字段"), "{}", error.message);
    assert!(editor.changes.is_none(), "拒绝后不能留下改动");
    // 拒绝必须发生在落盘之前：存储里不能出现任何内容。
    assert!(!has_staged_files(&root.0), "拒绝后不能留下暂存文件");
}

/// 超出列类型容量时按列的真实上限拒绝，而不是等到数据库报错。
#[test]
fn column_type_capacity_is_enforced() {
    let _guard = lock_content_store_for_test();
    let root = TempRoot::new("capacity");
    let store = std::sync::Arc::new(CellContentFileStore::new(&root.0));
    fluxdb_core::register_cell_content_store(store.clone());
    let mut editor = editor("BLOB");
    // BLOB 上限 65535，这里写到 65536。
    let path = write_file(&root.0, "too.bin", &vec![1u8; 65_536]);

    let error = update_binary_cell(
        &mut editor,
        0,
        1,
        fluxdb_core::UploadPayload::FilePath(path),
        true,
    )
    .expect_err("超出 BLOB 容量必须拒绝");
    assert!(error.message.contains("上传失败"), "{}", error.message);
    assert!(error.message.contains("BLOB"), "{}", error.message);
    // 报的是列类型的真实上限，不是笼统的「文件太大」。
    assert!(
        error.message.contains(&fluxdb_core::format_byte_length(65_535)),
        "{}",
        error.message
    );
}

/// 设为 NULL 不需要暂存，也不需要流式能力。
/// 大文本列同样走流式：占位值是可读的「类型 + 大小」，正文只在内容存储里。
#[test]
fn large_text_file_is_staged_as_text() {
    let _guard = lock_content_store_for_test();
    let root = TempRoot::new("large-text");
    let store = std::sync::Arc::new(CellContentFileStore::new(&root.0));
    fluxdb_core::register_cell_content_store(store.clone());
    let mut editor = editor("LONGTEXT");
    let length = fluxdb_core::BLOB_UPLOAD_STREAM_THRESHOLD_BYTES as usize + 1;
    let payload = vec![b'a'; length];
    let path = write_file(&root.0, "large.txt", &payload);

    update_binary_cell(
        &mut editor,
        0,
        1,
        fluxdb_core::UploadPayload::FilePath(path),
        true,
    )
    .expect("大文本上传");

    let update = &editor.changes.as_ref().expect("改动集").updates[0].cells[0];
    let stream = update.value.stream().expect("大文本必须走流式");
    assert_eq!(stream.kind, fluxdb_core::LargeValueKind::Text);
    assert_eq!(stream.byte_length, length as u64);

    // 占位值必须是 `LargeValue` 而不是 `Text`：`Text` 会被界面当成真实正文，
    // 于是给出编辑/复制按钮、长度算成占位串的字符数。
    let CellValue::LargeValue(placeholder) = update.value.placeholder() else {
        panic!("文本占位值必须是 LargeValue：{:?}", update.value.placeholder());
    };
    assert_eq!(placeholder.kind, fluxdb_core::LargeValueKind::Text);
    assert_eq!(placeholder.byte_length, Some(length as u64));
    assert_eq!(placeholder.content_id, stream.content_id());
    // 预览位只是类型名，不含正文。
    assert!(!placeholder.preview.contains("aaaa"), "{}", placeholder.preview);
    // 界面据此禁用编辑/复制与导出。
    assert!(update.value.placeholder().is_truncated());
}

/// 小文本文件整值写入，值类型是 `Text`（不是 `Bytes`，否则文本列会拿到二进制参数）。
#[test]
fn small_text_file_is_written_as_text() {
    let _guard = lock_content_store_for_test();
    let mut editor = editor("TEXT");
    let root = TempRoot::new("small-text");
    let path = write_file(&root.0, "small.txt", "你好".as_bytes());

    update_binary_cell(
        &mut editor,
        0,
        1,
        fluxdb_core::UploadPayload::FilePath(path),
        true,
    )
    .expect("小文本上传");

    let update = &editor.changes.as_ref().expect("改动集").updates[0].cells[0];
    assert_eq!(
        update.value,
        CellUpdateValue::Value(CellValue::Text("你好".to_string()))
    );
}

/// 非 UTF-8 文件写文本列必须拒绝，不能悄悄用替换字符改数据。
#[test]
fn invalid_utf8_file_is_rejected_for_text_column() {
    let _guard = lock_content_store_for_test();
    let mut editor = editor("TEXT");
    let root = TempRoot::new("bad-utf8");
    let path = write_file(&root.0, "bad.txt", &[0xFF, 0xFE, 0x00]);

    let error = update_binary_cell(
        &mut editor,
        0,
        1,
        fluxdb_core::UploadPayload::FilePath(path),
        true,
    )
    .expect_err("非法 UTF-8 必须拒绝");
    assert!(error.message.contains("UTF-8"), "{}", error.message);
    assert!(editor.changes.is_none());
}

/// JSON 列不开放上传替换：分块追加无法保证每块都是合法 JSON。
#[test]
fn json_column_rejects_upload() {
    let _guard = lock_content_store_for_test();
    let mut editor = editor("JSON");
    let root = TempRoot::new("json");
    let path = write_file(&root.0, "small.json", b"{}");

    let error = update_binary_cell(
        &mut editor,
        0,
        1,
        fluxdb_core::UploadPayload::FilePath(path),
        true,
    )
    .expect_err("JSON 列不支持上传替换");
    assert!(error.message.contains("不支持上传替换"), "{}", error.message);
}

#[test]
fn set_null_skips_staging() {
    let _guard = lock_content_store_for_test();
    let mut editor = editor("LONGBLOB");
    update_binary_cell(
        &mut editor,
        0,
        1,
        fluxdb_core::UploadPayload::SetNull,
        false,
    )
    .expect("设为 NULL");
    let update = &editor.changes.as_ref().expect("改动集").updates[0].cells[0];
    assert_eq!(update.value, CellUpdateValue::Value(CellValue::Null));
}

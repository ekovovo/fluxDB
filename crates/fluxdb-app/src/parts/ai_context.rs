// 自动界面上下文捕获与 Turn 目标固定（设计 §13，T021）。
//
// 原则：
// - 宿主（桌面 UI）只把**小 DTO** 交给本模块：不含连接密码/endpoint 等敏感配置；
// - 捕获分级：选区优先 → 否则整段按限额截断（头 60% + 尾 40% + 省略标记）；
// - Turn 开始即固定 `TargetRef`：之后切标签页不改变已启动任务的目标；
// - 显式 retarget 会换掉 target_json，从而改变 plan_hash → 旧审计/审批自然失效；
// - 没有目标时给一次性说明，任务仍可进行（不做确认弹框）。

/// 捕获限额（默认对齐设计 §13.2：单段 16 KiB / 400 行）。
#[derive(Clone, Copy, Debug)]
pub struct ContextLimits {
    pub max_excerpt_bytes: usize,
    pub max_excerpt_lines: usize,
    /// 超限时头部占比（设计默认 0.6，尾部 0.4）。
    pub head_ratio: f64,
}

impl Default for ContextLimits {
    fn default() -> Self {
        Self {
            max_excerpt_bytes: 16 * 1024,
            max_excerpt_lines: 400,
            head_ratio: 0.6,
        }
    }
}

/// 执行目标（不含任何凭据/endpoint）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TargetRef {
    pub connection_id: Option<i64>,
    pub database: Option<String>,
    pub schema: Option<String>,
    /// 表/Key 等对象标识（可空）。
    pub object: Option<String>,
    /// Redis DB（SQL 目标为 None）。
    pub redis_db: Option<u32>,
}

impl TargetRef {
    /// 目标是否为空（无连接 = 无法绑定执行目标）。
    pub fn is_empty(&self) -> bool {
        self.connection_id.is_none() && self.database.is_none()
    }

    /// 作为 plan_hash 的 target 部分：**只包含非敏感定位信息**。
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "connection_id": self.connection_id,
            "database": self.database,
            "schema": self.schema,
            "object": self.object,
            "redis_db": self.redis_db,
        })
        .to_string()
    }
}

/// 宿主提供的活动标签捕获（小 DTO）。
#[derive(Clone, Debug)]
pub enum CapturedTab {
    SqlEditor {
        tab_id: u64,
        revision: u64,
        connection_id: i64,
        database: Option<String>,
        schema: Option<String>,
        /// 用户选区（最相关，优先发送）。
        selection: Option<String>,
        /// 编辑器全文（仅在无选区时按限额截断）。
        full_text: String,
        /// 光标所在语句（可为空）。
        statement_at_cursor: Option<String>,
    },
    TableData {
        tab_id: u64,
        revision: u64,
        connection_id: i64,
        database: Option<String>,
        schema: Option<String>,
        object_name: String,
        columns: Vec<String>,
        filter: Option<String>,
        sort: Option<String>,
        sample_rows: Vec<String>,
        result_ref: Option<String>,
    },
    RedisKey {
        tab_id: u64,
        revision: u64,
        connection_id: i64,
        redis_db: u32,
        key: String,
        key_type: String,
        ttl_seconds: Option<i64>,
        preview: String,
    },
    Design {
        tab_id: u64,
        revision: u64,
        connection_id: i64,
        database: Option<String>,
        schema: Option<String>,
        object_path: String,
        summary: String,
    },
    /// 无标签页：用侧栏当前选中连接 + 最近活动对象。
    NoTab {
        connection_id: Option<i64>,
        database: Option<String>,
        schema: Option<String>,
        last_object: Option<String>,
    },
}

/// 有界摘录（超限时头尾保留）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundedExcerpt {
    pub text: String,
    pub truncated: bool,
    /// 被省略的行数（未截断时为 0）。
    pub omitted_lines: usize,
    /// 继续读取时建议的起始偏移（行号，0 起）。
    pub next_offset: usize,
}

/// 捕获快照（发送给模型/写进 Turn 快照的公开内容）。
#[derive(Clone, Debug)]
pub struct UiContextSnapshot {
    pub tab_id: Option<u64>,
    pub tab_revision: u64,
    pub target: TargetRef,
    /// 选区（优先发送；有选区时 `excerpt` 为 None）。
    pub selected_text: Option<String>,
    pub excerpt: Option<BoundedExcerpt>,
    pub statement_at_cursor: Option<String>,
    /// 结构化摘要（表结构/筛选/排序/Redis 类型 TTL/设计页摘要）。
    pub summary: Option<String>,
    pub last_error: Option<String>,
    pub result_ref: Option<String>,
    /// 不可用时的**一次性说明**（不是确认弹框）。
    pub notes: Vec<String>,
}

impl UiContextSnapshot {
    /// 捕获不可用（无目标）时任务仍可继续：只是没有执行目标。
    pub fn can_proceed(&self) -> bool {
        true
    }
}

/// 按限额截断文本：头 `head_ratio` + 尾 `1-head_ratio`，中间插入省略标记。
pub fn bound_excerpt(text: &str, limits: ContextLimits) -> BoundedExcerpt {
    let lines: Vec<&str> = text.lines().collect();
    let byte_len = text.len();
    let over_lines = lines.len() > limits.max_excerpt_lines;
    let over_bytes = byte_len > limits.max_excerpt_bytes;
    if !over_lines && !over_bytes {
        return BoundedExcerpt {
            text: text.to_string(),
            truncated: false,
            omitted_lines: 0,
            next_offset: lines.len(),
        };
    }

    // 行数超限优先按行切（SQL 语义按行最实用）。
    let head_lines = if over_lines {
        ((limits.max_excerpt_lines as f64) * limits.head_ratio).floor() as usize
    } else {
        lines.len()
    };
    let tail_lines = if over_lines {
        limits.max_excerpt_lines.saturating_sub(head_lines)
    } else {
        0
    };
    let head_end = head_lines.min(lines.len());
    let tail_start = lines.len().saturating_sub(tail_lines);

    let mut head = lines[..head_end].join("\n");
    let mut tail = if tail_start > head_end {
        lines[tail_start..].join("\n")
    } else {
        String::new()
    };
    let omitted = lines.len().saturating_sub(head_end + tail_lines);

    // 字节上限兜底：如果头尾仍然超限，就按字节裁剪（保持 UTF-8 边界）。
    if head.len() + tail.len() > limits.max_excerpt_bytes {
        let head_budget = ((limits.max_excerpt_bytes as f64) * limits.head_ratio).floor() as usize;
        let tail_budget = limits.max_excerpt_bytes.saturating_sub(head_budget);
        head = truncate_bytes(&head, head_budget);
        tail = truncate_bytes_tail(&tail, tail_budget);
    }

    let marker = if omitted > 0 {
        format!("… [省略 {omitted} 行；后续读取能力见当前引用] …")
    } else {
        "… [内容超限已截断；后续读取能力见当前引用] …".to_string()
    };
    let text = if tail.is_empty() {
        format!("{head}\n{marker}")
    } else {
        format!("{head}\n{marker}\n{tail}")
    };
    BoundedExcerpt {
        text,
        truncated: true,
        omitted_lines: omitted,
        next_offset: head_end,
    }
}

fn truncate_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn truncate_bytes_tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

/// 从宿主捕获生成快照（选区优先；无选区才用整段截断）。
pub fn capture(input: &CapturedTab, limits: ContextLimits) -> UiContextSnapshot {
    match input {
        CapturedTab::SqlEditor {
            tab_id,
            revision,
            connection_id,
            database,
            schema,
            selection,
            full_text,
            statement_at_cursor,
        } => {
            let has_selection = selection
                .as_deref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            let excerpt = (!has_selection).then(|| bound_excerpt(full_text, limits));
            UiContextSnapshot {
                tab_id: Some(*tab_id),
                tab_revision: *revision,
                target: TargetRef {
                    connection_id: Some(*connection_id),
                    database: database.clone(),
                    schema: schema.clone(),
                    object: None,
                    redis_db: None,
                },
                selected_text: if has_selection { selection.clone() } else { None },
                excerpt,
                statement_at_cursor: statement_at_cursor.clone(),
                summary: None,
                last_error: None,
                result_ref: None,
                notes: Vec::new(),
            }
        }
        CapturedTab::TableData {
            tab_id,
            revision,
            connection_id,
            database,
            schema,
            object_name,
            columns,
            filter,
            sort,
            sample_rows,
            result_ref,
        } => {
            let summary = serde_json::json!({
                "object": object_name,
                "columns": columns,
                "filter": filter,
                "sort": sort,
                "sample_rows": sample_rows,
            })
            .to_string();
            UiContextSnapshot {
                tab_id: Some(*tab_id),
                tab_revision: *revision,
                target: TargetRef {
                    connection_id: Some(*connection_id),
                    database: database.clone(),
                    schema: schema.clone(),
                    object: Some(object_name.clone()),
                    redis_db: None,
                },
                selected_text: None,
                excerpt: None,
                statement_at_cursor: None,
                summary: Some(summary),
                last_error: None,
                result_ref: result_ref.clone(),
                notes: Vec::new(),
            }
        }
        CapturedTab::RedisKey {
            tab_id,
            revision,
            connection_id,
            redis_db,
            key,
            key_type,
            ttl_seconds,
            preview,
        } => {
            let summary = serde_json::json!({
                "key": key,
                "type": key_type,
                "ttl_seconds": ttl_seconds,
            })
            .to_string();
            UiContextSnapshot {
                tab_id: Some(*tab_id),
                tab_revision: *revision,
                target: TargetRef {
                    connection_id: Some(*connection_id),
                    database: None,
                    schema: None,
                    object: Some(key.clone()),
                    redis_db: Some(*redis_db),
                },
                selected_text: None,
                excerpt: Some(bound_excerpt(preview, limits)),
                statement_at_cursor: None,
                summary: Some(summary),
                last_error: None,
                result_ref: None,
                notes: Vec::new(),
            }
        }
        CapturedTab::Design {
            tab_id,
            revision,
            connection_id,
            database,
            schema,
            object_path,
            summary,
        } => UiContextSnapshot {
            tab_id: Some(*tab_id),
            tab_revision: *revision,
            target: TargetRef {
                connection_id: Some(*connection_id),
                database: database.clone(),
                schema: schema.clone(),
                object: Some(object_path.clone()),
                redis_db: None,
            },
            selected_text: None,
            excerpt: Some(bound_excerpt(summary, limits)),
            statement_at_cursor: None,
            summary: None,
            last_error: None,
            result_ref: None,
            notes: Vec::new(),
        },
        CapturedTab::NoTab {
            connection_id,
            database,
            schema,
            last_object,
        } => {
            let target = TargetRef {
                connection_id: *connection_id,
                database: database.clone(),
                schema: schema.clone(),
                object: last_object.clone(),
                redis_db: None,
            };
            let notes = if target.is_empty() {
                vec!["当前没有可绑定的连接/标签：可以先做解释、规划一类不需要执行目标的工作。".to_string()]
            } else {
                Vec::new()
            };
            UiContextSnapshot {
                tab_id: None,
                tab_revision: 0,
                target,
                selected_text: None,
                excerpt: None,
                statement_at_cursor: None,
                summary: None,
                last_error: None,
                result_ref: None,
                notes,
            }
        }
    }
}

/// Turn 目标固定（发送时锁定，切标签页不改）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnTargetPin {
    pub target: TargetRef,
    pub captured_tab_id: Option<u64>,
    pub captured_tab_revision: u64,
    /// 作为 plan_hash 的 target 部分（不含敏感配置）。
    pub target_json: String,
}

/// 固定目标。
pub fn pin_target(snapshot: &UiContextSnapshot) -> TurnTargetPin {
    TurnTargetPin {
        target: snapshot.target.clone(),
        captured_tab_id: snapshot.tab_id,
        captured_tab_revision: snapshot.tab_revision,
        target_json: snapshot.target.to_json(),
    }
}

/// 显式换目标的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetargetOutcome {
    /// 目标没变：旧审批继续有效。
    Unchanged { pin: TurnTargetPin },
    /// 目标变了：调用方必须用 `invalidated_target_json` 对应的旧审批做失效处理
    /// （plan_hash 会随 target 变化，旧批准不再匹配）。
    Changed {
        pin: TurnTargetPin,
        invalidated_target_json: String,
    },
}

/// 显式换目标：只有用户显式操作才会调用；自动切标签页**不得**调用它。
pub fn retarget(pin: &TurnTargetPin, new_target: TargetRef) -> RetargetOutcome {
    let new_json = new_target.to_json();
    if new_json == pin.target_json {
        return RetargetOutcome::Unchanged { pin: pin.clone() };
    }
    RetargetOutcome::Changed {
        pin: TurnTargetPin {
            target: new_target,
            captured_tab_id: None,
            captured_tab_revision: 0,
            target_json: new_json,
        },
        invalidated_target_json: pin.target_json.clone(),
    }
}

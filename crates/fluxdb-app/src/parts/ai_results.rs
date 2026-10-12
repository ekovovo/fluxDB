// 结果句柄与有界快照仓库：归属、spill、分页及产物事实读取。
/// 结果引用（分页/过期的唯一句柄）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultRef {
    pub id: String,
    pub tool_id: String,
    /// 产出该结果的工具调用 ID（`provider_call_id`），是反查 `ai_results` 行的依据。
    /// 只用于宿主侧溯源，不进入模型可见的 `result_ref` 投影。
    pub call_id: String,
    pub created_at_ms: i64,
    /// 句柄归属：只有同 session/turn 才能读取，防止跨会话取数据。
    pub session_id: String,
    pub turn_id: String,
}

/// 结果页：有界 + 截断标记 + 游标（不能用截断结果宣称全集）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultPage {
    pub result: ResultRef,
    pub items: Vec<serde_json::Value>,
    /// 本次是否被行数/字节上限截断。
    pub truncated: bool,
    /// 继续读取的游标；`None` 表示已经到底。
    pub next_cursor: Option<String>,
    pub total_items: usize,
}

/// 供产物校验使用的只读事实快照：只读已存快照，不重新执行来源工具，也不改任何状态。
///
/// `sample` 把位置形状的原始行按列名成形为对象（同名列取最后一个值），产物因此能按
/// 字段名校验/取点；分页读取仍返回原始位置形状，不受这里影响。
#[derive(Clone, Debug, PartialEq)]
pub struct ResultFacts {
    pub result: ResultRef,
    /// 总条目数（内存 + spill）。
    pub rows: usize,
    /// 是否有未加载的 spill 条目：产物据此标记「抽样/截断」。
    pub truncated: bool,
    /// 内存条目里出现过的顶层字段名（保持首次出现顺序去重，上限 256）。
    pub fields: Vec<String>,
    /// 内存条目里所有已出现的非 null 值都是数值的字段（至少出现过一个数值）。
    pub numeric_fields: Vec<String>,
    /// 内存中的条目（受仓库内存预算约束的有界快照）；有列名时是对象形状。
    pub sample: Vec<serde_json::Value>,
}

/// 只读事实里统计的字段名条数上限：产物校验只需要结构轮廓，不要无界增长。
const MAX_FACT_FIELDS: usize = 256;

/// 结果读取错误（过期/已释放/未知/游标非法都可见）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultError {
    pub code: &'static str,
    pub message: String,
}

impl ResultError {
    fn unknown(id: &str) -> Self {
        Self {
            code: "unknown_result",
            message: format!("结果句柄不存在: {id}"),
        }
    }

    fn expired(id: &str) -> Self {
        Self {
            code: "result_expired",
            message: format!("结果句柄已过期: {id}"),
        }
    }

    fn bad_cursor(cursor: &str) -> Self {
        Self {
            code: "invalid_cursor",
            message: format!("结果游标非法: {cursor}"),
        }
    }

    /// spill 文件缺失或损坏：读取必须显式失败，不能返回一个假的空页或游标。
    fn damaged(message: impl Into<String>) -> Self {
        Self {
            code: "result_damaged",
            message: message.into(),
        }
    }
}

/// 结果仓库（T027）：内存保存首段，超出内存预算的条目写受控 spill 文件。
///
/// 仓库实例本身绑定一次会话（`for_turn`），因此跨 session/turn 的 ResultRef
/// 无法在此仓库读到数据；`read_page` 只读已取得的快照切片，不重新执行来源工具。
pub struct ResultStore {
    entries: HashMap<String, StoredResult>,
    next_id: u64,
    /// 单个结果的内存字节上限。
    pub byte_limit: usize,
    /// 句柄归属会话。
    owner_session_id: String,
    owner_turn_id: String,
    /// 受控落盘根；`None` 表示不 spill（超限条目回落内存，仍不丢数据）。
    spill_root: Option<std::path::PathBuf>,
}

struct StoredResult {
    result: ResultRef,
    /// 结果列名（按位置对应 `items` 里的每一行）；空表示不是表格形状。
    columns: Vec<String>,
    items: Vec<serde_json::Value>,
    /// 超出内存预算、已写入受控 spill 文件的条目数；分页时按需回读。
    spilled_len: usize,
    /// spill 文件相对受控根的路径（无 spill 时为空）。
    spill_path: Option<std::path::PathBuf>,
    /// spill 文件相对受控根的路径字符串与内容指纹；溢写事实需与 `ai_results` 对账。
    spill_fact: Option<SpillFact>,
    status: ResultStatus,
}

/// 已落盘结果的可核验事实：产出工具、受控根内相对路径、字节数、内容哈希。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpillFact {
    /// 产出该落盘结果的工具 ID。回执里的 `result_ref` 只是一次引用，
    /// 可能是别的工具（如 `result.read_page`）回显的来源句柄；
    /// 只有产出工具与本次调用的工具一致，这条事实才属于本次调用。
    pub tool_id: String,
    pub relative_path: String,
    pub byte_count: i64,
    pub content_hash: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResultStatus {
    Available,
    Released,
    Expired,
}

/// spill 落盘事实：相对路径 / 字节数 / 内容指纹，供 `ai_results` 登记与校验。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpillResult {
    /// spill 文件相对受控根的路径；未发生 spill 时为空。
    pub relative_path: Option<String>,
    pub byte_count: i64,
    pub content_hash: String,
}

impl SpillResult {
    /// 是否真的写了 spill 文件。
    pub fn spilled(&self) -> bool {
        self.relative_path.is_some()
    }
}

fn result_io_error(message: &str) -> impl Fn(std::io::Error) -> fluxdb_core::Error + '_ {
    move |error| {
        fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            format!("{message}: {error}"),
        )
    }
}

impl ResultStore {
    /// 内存仓库（测试与无落盘场景）：超限条目回落内存，不写文件。
    pub fn new(byte_limit: usize) -> Self {
        Self {
            entries: HashMap::new(),
            next_id: 1,
            byte_limit,
            owner_session_id: String::new(),
            owner_turn_id: String::new(),
            spill_root: None,
        }
    }

    /// 生产仓库：绑定会话归属；`spill_root` 为受控结果根目录。
    pub fn for_turn(
        byte_limit: usize,
        session_id: &str,
        turn_id: &str,
        spill_root: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            entries: HashMap::new(),
            next_id: 1,
            byte_limit,
            owner_session_id: session_id.to_string(),
            owner_turn_id: turn_id.to_string(),
            spill_root,
        }
    }

    /// 保存一次工具结果，返回句柄与 spill 事实。
    ///
    /// 内存预算内的条目留在内存；超出部分在有受控根时写入 `results/<uuid>.json`，
    /// 否则回落内存，保证模型可读到的条目不会因为没有目录而缺失。
    ///
    /// `call_id` 是产出该结果的工具调用 ID，随句柄一起保存，供宿主侧反查 `ai_results` 行。
    /// `columns` 是结果的列名（按位置对应 `items` 的每一行）；不是表格形状时传空切片。
    /// 列名让产物能按字段名引用真实结果，而不是靠位置猜。
    pub fn put(
        &mut self,
        tool_id: &str,
        call_id: &str,
        columns: &[String],
        now_ms: i64,
        items: impl IntoIterator<Item = serde_json::Value>,
    ) -> fluxdb_core::Result<(ResultRef, SpillResult)> {
        let mut memory = Vec::new();
        let mut overflow = Vec::new();
        let mut memory_bytes = 0usize;
        for item in items {
            let size = serde_json::to_string(&item).map(|text| text.len()).unwrap_or(0);
            // 一旦进入 spill，余下条目也进入 spill，保留查询的原始行序。
            if overflow.is_empty() && memory_bytes + size <= self.byte_limit {
                memory_bytes += size;
                memory.push(item);
            } else {
                overflow.push(item);
            }
        }
        let mut spill = SpillResult::default();
        let mut spilled_len = 0usize;
        if !overflow.is_empty() {
            let payload = serde_json::to_vec(&overflow).map_err(|error| {
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    format!("结果序列化失败: {error}"),
                )
            })?;
            let content = String::from_utf8_lossy(&payload).into_owned();
            let hash = crate::source_hash(&[&content]);
            match self.spill_root.clone() {
                Some(root) => {
                    let dir = root.join("results");
                    std::fs::create_dir_all(&dir).map_err(result_io_error("创建结果目录失败"))?;
                    let path = dir.join(format!("{}.json", uuid::Uuid::new_v4()));
                    std::fs::write(&path, &payload).map_err(result_io_error("写结果 spill 文件失败"))?;
                    spill.relative_path = path
                        .strip_prefix(&root)
                        .ok()
                        .map(|rest| rest.to_string_lossy().into_owned());
                    spill.byte_count = payload.len() as i64;
                    spill.content_hash = hash;
                    spilled_len = overflow.len();
                }
                None => {
                    // 没有受控目录时不能丢数据：回填内存，避免模型读到不完整结果。
                    spilled_len = 0;
                    memory.append(&mut overflow);
                }
            }
        }
        let id = format!("result-{}", self.next_id);
        self.next_id += 1;
        let result = ResultRef {
            id: id.clone(),
            tool_id: tool_id.to_string(),
            call_id: call_id.to_string(),
            created_at_ms: now_ms,
            session_id: self.owner_session_id.clone(),
            turn_id: self.owner_turn_id.clone(),
        };
        let spill_path = self
            .spill_root
            .as_ref()
            .zip(spill.relative_path.as_ref())
            .map(|(root, relative)| root.join(relative));
        self.entries.insert(
            id,
            StoredResult {
                result: result.clone(),
                columns: columns.to_vec(),
                items: memory,
                spilled_len,
                spill_path,
                spill_fact: spill
                    .relative_path
                    .as_ref()
                    .map(|relative_path| SpillFact {
                        tool_id: result.tool_id.clone(),
                        relative_path: relative_path.clone(),
                        byte_count: spill.byte_count,
                        content_hash: spill.content_hash.clone(),
                    }),
                status: ResultStatus::Available,
            },
        );
        Ok((result, spill))
    }

    /// 取句柄的 spill 事实；未溢写或句柄不存在返回 `None`。
    pub fn spill_fact(&self, id: &str) -> Option<SpillFact> {
        self.entries.get(id)?.spill_fact.clone()
    }

    /// 释放句柄（释放后读取必须报错，而不是返回空页）。
    pub fn release(&mut self, id: &str) -> bool {
        match self.entries.get_mut(id) {
            Some(entry) => {
                entry.status = ResultStatus::Released;
                true
            }
            None => false,
        }
    }

    /// 标记过期（TTL 由调用方判定；过期后读取必须报错）。
    pub fn expire(&mut self, id: &str) -> bool {
        match self.entries.get_mut(id) {
            Some(entry) => {
                entry.status = ResultStatus::Expired;
                true
            }
            None => false,
        }
    }

    /// 会话结束时释放该会话的全部句柄（memory 结果过期语义）。
    pub fn expire_session(&mut self) {
        for entry in self.entries.values_mut() {
            entry.status = ResultStatus::Expired;
        }
    }

    /// 读取一页：行数与字节双上限，返回截断标记与下一页游标。
    pub fn read_page(
        &self,
        id: &str,
        cursor: Option<&str>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<ResultPage, ResultError> {
        let Some(entry) = self.entries.get(id) else {
            return Err(ResultError::unknown(id));
        };
        match entry.status {
            ResultStatus::Available => {}
            ResultStatus::Released => return Err(ResultError::expired(id)),
            ResultStatus::Expired => return Err(ResultError::unknown(id)),
        }
        if entry.result.session_id != self.owner_session_id
            || entry.result.turn_id != self.owner_turn_id
        {
            // 句柄不属于本仓库归属的会话：按未知处理，不泄露其它会话的数据。
            return Err(ResultError::unknown(id));
        }
        let start = match cursor {
            None => 0usize,
            Some(raw) => {
                let prefix = format!("{id}:");
                let offset = raw
                    .strip_prefix(&prefix)
                    .ok_or_else(|| ResultError::bad_cursor(raw))?;
                offset
                    .parse::<usize>()
                    .map_err(|_| ResultError::bad_cursor(raw))?
            }
        };
        let total_items = entry.items.len() + entry.spilled_len;
        if start > total_items {
            return Err(ResultError::bad_cursor(&format!("{id}:{start}")));
        }
        // spill 条目按需从受控文件回读；文件缺失或损坏必须显式报错，
        // 不能只报一个"还有更多"的游标让模型以为能读到数据。
        let spilled_items = match (entry.spill_path.as_ref(), entry.spilled_len) {
            (Some(_), 0) | (None, 0) => Vec::new(),
            (Some(path), expected) => {
                let raw = std::fs::read(path).map_err(|error| {
                    ResultError::damaged(format!("结果 spill 文件不可读: {error}"))
                })?;
                if let Some(fact) = &entry.spill_fact {
                    if raw.len() as i64 != fact.byte_count
                        || crate::source_hash(&[&String::from_utf8_lossy(&raw)]) != fact.content_hash
                    {
                        return Err(ResultError::damaged("结果 spill 文件内容与登记指纹不一致"));
                    }
                }
                let decoded: Vec<serde_json::Value> = serde_json::from_slice(&raw).map_err(|error| {
                    ResultError::damaged(format!("结果 spill 文件已损坏: {error}"))
                })?;
                if decoded.len() != expected {
                    return Err(ResultError::damaged("结果 spill 文件条目数与登记不一致"));
                }
                decoded
            }
            (None, expected) => {
                return Err(ResultError::damaged(format!(
                    "结果缺少 spill 文件，无法读取剩余 {expected} 条"
                )));
            }
        };
        let limit = limit.max(1);
        let mut items = Vec::new();
        let mut used = 0usize;
        let mut index = start;
        while index < total_items && items.len() < limit {
            let item = match index < entry.items.len() {
                true => &entry.items[index],
                false => &spilled_items[index - entry.items.len()],
            };
            let size = serde_json::to_string(item).map(|text| text.len()).unwrap_or(0);
            if !items.is_empty() && used + size > max_bytes {
                break;
            }
            used += size;
            items.push(item.clone());
            index += 1;
        }
        let truncated = index < total_items;
        Ok(ResultPage {
            result: entry.result.clone(),
            items,
            truncated,
            next_cursor: truncated.then(|| format!("{id}:{index}")),
            total_items,
        })
    }

    /// 读取句柄的最小事实；归属/状态校验与 [`ResultStore::read_page`] 一致：
    /// 非本仓库归属会话按 unknown 处理，已释放按 expired，已过期按 unknown，
    /// spill 损坏不在这里判（事实只读内存快照，不触盘）。
    pub fn facts(&self, id: &str) -> Result<ResultFacts, ResultError> {
        let Some(entry) = self.entries.get(id) else {
            return Err(ResultError::unknown(id));
        };
        match entry.status {
            ResultStatus::Available => {}
            ResultStatus::Released => return Err(ResultError::expired(id)),
            ResultStatus::Expired => return Err(ResultError::unknown(id)),
        }
        if entry.result.session_id != self.owner_session_id
            || entry.result.turn_id != self.owner_turn_id
        {
            // 句柄不属于本仓库归属的会话：按未知处理，不泄露其它会话的数据。
            return Err(ResultError::unknown(id));
        }
        // 有列名时先把位置行按列名成形为对象（同名列取后者，与 `Map::insert` 一致），
        // 让产物能按字段名取数与校验；没有列名（非表格形状）就按原样统计。
        let sample: Vec<serde_json::Value> = if entry.columns.is_empty() {
            entry.items.clone()
        } else {
            entry
                .items
                .iter()
                .map(|item| match item.as_array() {
                    Some(cells) => {
                        let mut object = serde_json::Map::new();
                        for (column, value) in entry.columns.iter().zip(cells.iter()) {
                            object.insert(column.clone(), value.clone());
                        }
                        serde_json::Value::Object(object)
                    }
                    None => item.clone(),
                })
                .collect()
        };
        let (fields, numeric_fields) = result_field_facts(&sample);
        Ok(ResultFacts {
            result: entry.result.clone(),
            rows: entry.items.len() + entry.spilled_len,
            truncated: entry.spilled_len > 0,
            fields,
            numeric_fields,
            sample,
        })
    }
}


/// 从有界对象行推导字段与严格数值列，仓库和产物校验复用同一规则。
fn result_field_facts(sample: &[serde_json::Value]) -> (Vec<String>, Vec<String>) {
    // 只扫描内存条目里的 JSON 对象顶层键；非对象条目忽略。
    let mut fields: Vec<String> = Vec::new();
    // 与 `fields` 同下标：该字段出现过的非 null 次数，以及其中是 JSON number 的次数。
    let mut stats: Vec<(usize, usize)> = Vec::new();
    for item in sample {
        let Some(object) = item.as_object() else {
            continue;
        };
        for (key, value) in object {
            let index = match fields.iter().position(|field| field == key) {
                Some(index) => index,
                None => {
                    if fields.len() >= MAX_FACT_FIELDS {
                        // 字段名条数有上限：超限字段不再统计，避免无界增长。
                        continue;
                    }
                    fields.push(key.clone());
                    stats.push((0, 0));
                    fields.len() - 1
                }
            };
            if !value.is_null() {
                stats[index].0 += 1;
                if value.is_number() {
                    stats[index].1 += 1;
                }
            }
        }
    }
    // 严格判定：字段必须至少出现过一个数值，且所有已出现的非 null 值都是 JSON number。
    // 字符串数字（如 `"12"`）不算数值，避免产物把文本列当数值列聚合。
    let numeric_fields = fields
        .iter()
        .zip(stats.iter())
        .filter(|(_field, (non_null, numbers))| numbers > &0 && numbers == non_null)
        .map(|(field, _stats)| field.clone())
        .collect();
    (fields, numeric_fields)
}

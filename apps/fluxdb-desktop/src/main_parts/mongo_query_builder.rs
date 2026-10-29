// MongoDB 查询面板的「条件构建」：字段 / 运算符 / 值几行，直接改写成 Filter JSON。
//
// 它只是 Filter 的另一种输入法，不持有查询：每改一次就把 JSON 写回 Filter 编辑器，
// 真正执行的仍是面板里那一份文本。所以构建器与手写 JSON 永远一致，
// 不会出现「构建器里一套、实际查的是另一套」。
//
// 反向也走同一条路：展开构建器时解析当前 Filter。只认多条件平铺对象里的单个比较运算符，
// 复杂表达式（`$or`、嵌套对象、`$regex` 等）一律拒绝并提示继续手写 JSON——
// 猜错了改写语义，比不做更糟。

/// 条件构建器支持的运算符，顺序即下拉菜单顺序（对齐交互稿）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MongoQueryOperator {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    In,
    Exists,
}

impl MongoQueryOperator {
    const ALL: [Self; 8] = [
        Self::Eq,
        Self::Ne,
        Self::Gt,
        Self::Gte,
        Self::Lt,
        Self::Lte,
        Self::In,
        Self::Exists,
    ];

    /// 文案 key 后缀，与语言包里 `app.mongo.query.op.*` 一一对应。
    fn label_key(self) -> &'static str {
        match self {
            Self::Eq => "app.mongo.query.op.eq",
            Self::Ne => "app.mongo.query.op.ne",
            Self::Gt => "app.mongo.query.op.gt",
            Self::Gte => "app.mongo.query.op.gte",
            Self::Lt => "app.mongo.query.op.lt",
            Self::Lte => "app.mongo.query.op.lte",
            Self::In => "app.mongo.query.op.in",
            Self::Exists => "app.mongo.query.op.exists",
        }
    }

    /// 写进 JSON 的查询运算符；`eq` 平铺成 `{ "field": value }`，没有 `$eq`。
    fn json_operator(self) -> Option<&'static str> {
        match self {
            Self::Eq => None,
            Self::Ne => Some("$ne"),
            Self::Gt => Some("$gt"),
            Self::Gte => Some("$gte"),
            Self::Lt => Some("$lt"),
            Self::Lte => Some("$lte"),
            Self::In => Some("$in"),
            Self::Exists => Some("$exists"),
        }
    }

    /// 下拉里的符号，与中文名拼成 `等于 · =` / `不等于 · $ne`。
    fn symbol(self) -> &'static str {
        self.json_operator().unwrap_or("=")
    }

    fn label(self) -> SharedString {
        SharedString::from(format!("{} · {}", tr(self.label_key()), self.symbol()))
    }

    fn from_json_operator(operator: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.json_operator() == Some(operator))
    }
}

/// 条件上限：再多就该直接写 JSON。与交互稿一致。
const MONGO_QUERY_BUILDER_MAX_ROWS: usize = 6;
/// 构建器控件高度。
///
/// 交互稿是 35px，这里压到 30px：条件行最多六条，35 一行在常驻面板里太占地方，
/// 而整块构建器的存在感应当低于它下面的结果区。字段/值两格是代码编辑器（要挂补全），
/// 但它按行高算最小高度、外面给 `h_full` 就能填满这一格，因此同样能压到 30px。
const MONGO_QUERY_BUILDER_CONTROL_HEIGHT: f32 = 30.;
/// 运算符列的固定宽度，与字段/值两侧的自适应列分开。
const MONGO_QUERY_BUILDER_OPERATOR_WIDTH: f32 = 150.;
/// 字段格最小宽度：再窄连 `customer.tier` 这种路径都看不全。
const MONGO_QUERY_BUILDER_FIELD_MIN_WIDTH: f32 = 140.;
/// 值格最小宽度：等宽小字下约十来个字符，够看清 `"paid"` 或 `500`。
const MONGO_QUERY_BUILDER_VALUE_MIN_WIDTH: f32 = 120.;

/// 构建器里的一行条件。
struct MongoQueryBuilderRow {
    /// 字段路径：允许任意文档路径（字段目录只是候选），因此是输入框而不是固定下拉。
    ///
    /// 用 `EditorState` 而非单行 `InputState`：只有编辑器能挂补全 provider
    /// （`Lsp.completion_provider`），单行输入框没有这个入口。
    field: Entity<EditorState>,
    operator: MongoQueryOperator,
    value: Entity<EditorState>,
    /// 两个输入框的改动都要回写 JSON；订阅随行一起释放。
    _field_subscription: Subscription,
    _value_subscription: Subscription,
}

/// 一个集合标签页的构建器状态（输入框实体随结构一起 drop）。
struct MongoQueryBuilder {
    rows: Vec<MongoQueryBuilderRow>,
    /// Filter 解析不出构建器形态时的提示；成功写回即清除。
    error: Option<SharedString>,
}

impl MongoQueryBuilder {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            error: None,
        }
    }

    fn rows(&self) -> &[MongoQueryBuilderRow] {
        &self.rows
    }

    fn error(&self) -> Option<SharedString> {
        self.error.clone()
    }
}

impl NavicatMain {
    /// 展开/收起条件构建器。
    ///
    /// 展开时重新解析 Filter：解析不了（语法错、复杂表达式）只报错、不改 JSON，
    /// 用户继续手写即可。收起不动 JSON——构建器从来没持有过独立状态。
    pub(crate) fn toggle_mongo_query_builder(
        &mut self,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .mongo_query_builder_expanded
            .get(&tab_id)
            .copied()
            .unwrap_or(false)
        {
            self.mongo_query_builder_expanded.insert(tab_id, false);
            cx.notify();
            return;
        }
        let Some(filter) = self
            .mongo_query_inputs
            .get(&tab_id)
            .map(|inputs| inputs.read(cx).filter.clone())
        else {
            return;
        };
        let text = filter.read(cx).value().to_string();
        match parse_mongo_builder_conditions(&text) {
            Ok(conditions) => {
                let rows = conditions
                    .into_iter()
                    .map(|(field, operator, value)| {
                        self.new_mongo_builder_row(tab_id, field, operator, value, window, cx)
                    })
                    .collect();
                let builder = self
                    .mongo_query_builders
                    .entry(tab_id)
                    .or_insert_with(MongoQueryBuilder::new);
                builder.rows = rows;
                builder.error = None;
                self.mongo_query_builder_expanded.insert(tab_id, true);
            }
            Err(error) => {
                // 展开并把提示摆在面板里：报错却什么都不弹，用户只会以为按钮坏了。
                // 条件行保持为空、JSON 一个字都不动——拒绝改写的承诺要看得见。
                let builder = self
                    .mongo_query_builders
                    .entry(tab_id)
                    .or_insert_with(MongoQueryBuilder::new);
                builder.rows.clear();
                builder.error = Some(error);
                self.mongo_query_builder_expanded.insert(tab_id, true);
            }
        }
        cx.notify();
    }

    /// 新建一行条件；两个输入框的改动都立刻回写 Filter。
    ///
    /// 两格都挂补全：字段格按字段目录补路径，值格按**本行字段**补它出现过的取值
    /// （`{"status": "pai` 在 Compass 里就这么补，用户不用去结果区翻写法）。
    fn new_mongo_builder_row(
        &mut self,
        tab_id: TabId,
        field: String,
        operator: MongoQueryOperator,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> MongoQueryBuilderRow {
        let view = cx.entity().downgrade();
        let field_input = cx.new(|cx| {
            let mut editor = EditorState::new(window, cx)
                .placeholder(tr("app.mongo.query.builder_field_placeholder"))
                .default_value(field)
                // `submit_on_enter`：回车只结束编辑，不往条件格里塞换行。
                .soft_wrap(false)
                .submit_on_enter(true);
            configure_mongo_builder_completion(&mut editor, None, view.clone(), tab_id, window, cx);
            editor
        });
        // 值格的候选要问同行字段格「你是什么字段」，所以先把字段格实体降级传进去。
        let field_weak = field_input.downgrade();
        let value_input = cx.new(|cx| {
            let mut editor = EditorState::new(window, cx)
                .placeholder(tr("app.mongo.query.builder_value_placeholder"))
                .default_value(value)
                .soft_wrap(false)
                .submit_on_enter(true);
            configure_mongo_builder_completion(
                &mut editor,
                Some(field_weak),
                view,
                tab_id,
                window,
                cx,
            );
            editor
        });
        let field_subscription = cx.subscribe_in(
            &field_input,
            window,
            move |this: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.sync_mongo_query_builder(tab_id, window, cx);
                }
            },
        );
        let value_subscription = cx.subscribe_in(
            &value_input,
            window,
            move |this: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.sync_mongo_query_builder(tab_id, window, cx);
                }
            },
        );
        MongoQueryBuilderRow {
            field: field_input,
            operator,
            value: value_input,
            _field_subscription: field_subscription,
            _value_subscription: value_subscription,
        }
    }

    /// 添加一行条件并聚焦到它的值输入框（新行的字段通常还没填，光标停在值上也合理——
    /// 与交互稿「添加后聚焦新值输入」一致）。
    pub(crate) fn add_mongo_builder_condition(
        &mut self,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .mongo_query_builders
            .get(&tab_id)
            .is_some_and(|builder| builder.rows.len() >= MONGO_QUERY_BUILDER_MAX_ROWS)
        {
            return;
        }
        let row = self.new_mongo_builder_row(
            tab_id,
            String::new(),
            MongoQueryOperator::Eq,
            String::new(),
            window,
            cx,
        );
        row.value
            .update(cx, |input, cx| input.focus_handle(cx).focus(window, cx));
        self.mongo_query_builders
            .entry(tab_id)
            .or_insert_with(MongoQueryBuilder::new)
            .rows
            .push(row);
        cx.notify();
    }

    /// 移除一行；JSON 随之重算（被移除字段的过滤条件同时消失）。
    pub(crate) fn remove_mongo_builder_condition(
        &mut self,
        tab_id: TabId,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(builder) = self.mongo_query_builders.get_mut(&tab_id)
            && index < builder.rows.len()
        {
            builder.rows.remove(index);
        }
        self.sync_mongo_query_builder(tab_id, window, cx);
    }

    /// 字段候选（字段目录）选中后写进该行的字段输入框。
    ///
    /// `set_value` 不触发 `InputEvent::Change`，所以这里手动补一次同步。
    pub(crate) fn set_mongo_builder_field(
        &mut self,
        tab_id: TabId,
        index: usize,
        field: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self
            .mongo_query_builders
            .get(&tab_id)
            .and_then(|builder| builder.rows.get(index))
            .map(|row| row.field.clone())
        else {
            return;
        };
        input.update(cx, |input, cx| input.set_value(field, window, cx));
        self.sync_mongo_query_builder(tab_id, window, cx);
    }

    pub(crate) fn set_mongo_builder_operator(
        &mut self,
        tab_id: TabId,
        index: usize,
        operator: MongoQueryOperator,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(row) = self
            .mongo_query_builders
            .get_mut(&tab_id)
            .and_then(|builder| builder.rows.get_mut(index))
        {
            row.operator = operator;
        }
        self.sync_mongo_query_builder(tab_id, window, cx);
    }

    /// 把条件行拼成 Filter JSON 写回编辑器。
    ///
    /// 没填字段或没填值的行直接跳过（`$exists` 除外）：用户加了行还没填完时，
    /// 不该出现 `{ "field": "" }` 这种把结果查空的过滤条件。
    fn sync_mongo_query_builder(
        &mut self,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(filter) = self
            .mongo_query_inputs
            .get(&tab_id)
            .map(|inputs| inputs.read(cx).filter.clone())
        else {
            return;
        };
        let Some(builder) = self.mongo_query_builders.get(&tab_id) else {
            return;
        };
        let mut document = serde_json::Map::new();
        for row in builder.rows() {
            let field = row.field.read(cx).value().to_string();
            let field = field.trim();
            if field.is_empty() {
                continue;
            }
            let text = row.value.read(cx).value().to_string();
            let Some(value) = mongo_builder_condition_value(row.operator, &text) else {
                continue;
            };
            document.insert(
                field.to_string(),
                mongo_builder_expression(row.operator, value),
            );
        }
        let json = serde_json::Value::Object(document).to_string();
        filter.update(cx, |editor, cx| editor.set_value(json, window, cx));
        let had_error = self
            .mongo_query_builders
            .get(&tab_id)
            .is_some_and(|builder| builder.error.is_some());
        if let Some(builder) = self.mongo_query_builders.get_mut(&tab_id) {
            builder.error = None;
        }
        // 只在错误提示真的消失时才重绘本视图。
        //
        // 这里每条按键都会走到，而条件行与 Filter 都是独立实体，各自 notify 就够重绘自己；
        // 无条件 `cx.notify()` 会让整个主视图（侧栏树、标签条、文档虚拟列表、查询面板）
        // 每次按键多排一帧——实测每键多 ~1.4ms，正是「打开条件构建器后帧率往下掉」的来源。
        if had_error {
            cx.notify();
        }
    }
}

/// 一行条件对应的值；返回 `None` 表示这行还没填完，跳过。
fn mongo_builder_condition_value(operator: MongoQueryOperator, text: &str) -> Option<serde_json::Value> {
    match operator {
        // `$exists` 只关心字段在不在：值恒为布尔，与手写 `{ "a": { "$exists": false } }` 一致。
        MongoQueryOperator::Exists => Some(serde_json::Value::Bool(text.trim() != "false")),
        _ if text.trim().is_empty() => None,
        MongoQueryOperator::In => Some(serde_json::Value::Array(
            text.split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(parse_mongo_builder_scalar)
                .collect(),
        )),
        _ => Some(parse_mongo_builder_scalar(text.trim())),
    }
}

/// 把一行条件包成它对应的 JSON 表达式：等值平铺，其余包一层 `$运算符`。
fn mongo_builder_expression(
    operator: MongoQueryOperator,
    value: serde_json::Value,
) -> serde_json::Value {
    match operator.json_operator() {
        Some(operator) => {
            let mut wrapper = serde_json::Map::new();
            wrapper.insert(operator.to_string(), value);
            serde_json::Value::Object(wrapper)
        }
        None => value,
    }
}

/// 输入框文本转 JSON 标量：`true`/`false`/`null`/数字原样，带引号的去掉引号，
/// 其余按字符串。用户不必为了写字符串再套一层引号。
fn parse_mongo_builder_scalar(text: &str) -> serde_json::Value {
    match text {
        "true" => serde_json::Value::Bool(true),
        "false" => serde_json::Value::Bool(false),
        "null" => serde_json::Value::Null,
        "" => serde_json::Value::String(String::new()),
        _ => {
            if let Ok(number) = text.parse::<i64>() {
                return serde_json::Value::from(number);
            }
            if let Ok(number) = text.parse::<f64>() {
                return serde_json::Value::from(number);
            }
            let unquoted = text
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .or_else(|| {
                    text.strip_prefix('\'')
                        .and_then(|rest| rest.strip_suffix('\''))
                });
            serde_json::Value::String(unquoted.unwrap_or(text).to_string())
        }
    }
}

/// 按「先严格 JSON、再 JSON5」解析，与连接器 `mongo_parse_json` 同规则。
///
/// 先试严格 JSON 不只是省事：JSON5 把所有数字读成 f64，`{"age": 41}` 会落成
/// Double 而匹配不到存成 Int32 的字段；写回新 JSON 时那一位就悄悄改了类型。
///
/// `NaN` / `Infinity` 直接拒：serde_json 会把它读成 `null`，等于把条件换成
/// 「字段等于 null」——那种改写比报错糟得多。
fn parse_mongo_relaxed_json(text: &str) -> Option<serde_json::Value> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        return Some(value);
    }
    if text.contains("NaN") || text.contains("Infinity") {
        return None;
    }
    json5::from_str::<serde_json::Value>(text).ok()
}

/// JSON 标量回填到输入框：字符串保留引号。
///
/// 去掉引号再写回会把 `"500"` 读成数字 500——一次往返就悄悄改了字段类型。
fn format_mongo_builder_scalar(value: &serde_json::Value) -> String {
    value.to_string()
}

/// Filter JSON → 条件行；只接受能无损表达成「字段 / 运算符 / 值」的形态。
///
/// 语法与连接器执行时的 [`mongo_parse_json`] 同规则：先严格 JSON，失败再按 JSON5
/// 收（无引号键、单引号、注释、尾逗号）。否则会出现「能查、构建器却报语法错」——
/// Compass 查询栏里常见的 `{event_type: "page_view"}` 正是这种写法。
///
/// [`mongo_parse_json`]: ../../../../crates/fluxdb-connectors/src/parts/mongo/client.rs
fn parse_mongo_builder_conditions(
    text: &str,
) -> Result<Vec<(String, MongoQueryOperator, String)>, SharedString> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let value = parse_mongo_relaxed_json(trimmed)
        .ok_or_else(|| tr("app.mongo.query.builder_json_error"))?;
    let Some(document) = value.as_object() else {
        return Err(tr("app.mongo.query.builder_json_error"));
    };
    let complex = || tr("app.mongo.query.builder_complex_error");
    let mut conditions = Vec::new();
    for (field, expression) in document {
        if field.starts_with('$') {
            return Err(complex());
        }
        let (operator, value) = match expression {
            // 包装对象里必须恰好一个受支持的 `$op`：`$or`、`$regex`、多运算符组合都拒绝。
            serde_json::Value::Object(wrapper) => {
                let mut entries = wrapper.iter();
                let (Some((key, value)), None) = (entries.next(), entries.next()) else {
                    return Err(complex());
                };
                let Some(operator) = MongoQueryOperator::from_json_operator(key) else {
                    return Err(complex());
                };
                if operator == MongoQueryOperator::Exists && !value.is_boolean() {
                    return Err(complex());
                }
                (operator, value)
            }
            other => (MongoQueryOperator::Eq, other),
        };
        let text = match (operator, value) {
            (MongoQueryOperator::In, serde_json::Value::Array(items)) => items
                .iter()
                .map(format_mongo_builder_scalar)
                .collect::<Vec<_>>()
                .join(", "),
            // 等值条件挂数组/对象时，一格输入框表达不了，别硬拆。
            (_, value) if value.is_array() || value.is_object() => return Err(complex()),
            (_, value) => format_mongo_builder_scalar(value),
        };
        conditions.push((field.clone(), operator, text));
    }
    Ok(conditions)
}

/// 构建器渲染快照：只带这一帧要用的句柄。
///
/// 渲染期间不回头读视图状态（`Context` 已被借用），所以先把行摊平成值。
struct MongoQueryBuilderView {
    /// 每行：字段编辑器、运算符、值编辑器。
    rows: Vec<(Entity<EditorState>, MongoQueryOperator, Entity<EditorState>)>,
    error: Option<SharedString>,
}

fn mongo_query_builder_view(
    builders: &BTreeMap<TabId, MongoQueryBuilder>,
    tab_id: TabId,
) -> MongoQueryBuilderView {
    builders.get(&tab_id).map_or_else(
        || MongoQueryBuilderView {
            rows: Vec::new(),
            error: None,
        },
        |builder| MongoQueryBuilderView {
            rows: builder
                .rows()
                .iter()
                .map(|row| (row.field.clone(), row.operator, row.value.clone()))
                .collect(),
            error: builder.error(),
        },
    )
}

/// 交互稿的薄荷底与绿色系：面板里两层展开区共用这一组色，明暗各一份。
#[derive(Clone, Copy)]
struct MongoQueryTone {
    /// 展开区底色（交互稿 `--mint`）：比卡片底色浅一档，分层不靠描边。
    mint: gpui::Rgba,
    /// 绿色浅底（交互稿 `--green-soft`）：角标、展开态按钮、AND 徽标。
    green_soft: gpui::Rgba,
    /// 绿色描边（交互稿 `--green-line`）。
    green_line: gpui::Rgba,
}

fn mongo_query_tone(colors: UiColors) -> MongoQueryTone {
    if colors.is_dark {
        MongoQueryTone {
            mint: rgb(0x222c26),
            green_soft: rgb(0x263e2f),
            green_line: rgb(0x3b5b45),
        }
    } else {
        MongoQueryTone {
            mint: rgb(0xf6faf7),
            green_soft: rgb(0xeaf4ed),
            green_line: rgb(0xc6ddce),
        }
    }
}

/// 构建器分节：条件行，「添加条件」挂在最后一行末尾。
///
/// 没有单独的列名行：字段格自己写「字段路径，如 customer.tier」，运算符显示中文名，
/// 值格写「值，如 "paid" 或 500」——三列各自说清楚是什么，再挂一条列名行就是重复，
/// 还白吃掉一条 ~14px 的横向带。
///
/// 「添加条件」也不单独占一行：它跟着最后一个条件行走（没有条件时与空提示同行），
/// 否则一条 28px 的横向带只放得下一个按钮。整块只有条件行本身的高度。
fn mongo_query_builder_section(
    tab_id: TabId,
    builder: &MongoQueryBuilderView,
    field_paths: &[SharedString],
    colors: UiColors,
    tone: MongoQueryTone,
    window: &Window,
    cx: &mut Context<NavicatMain>,
) -> Div {
    // 字段目录整块共享一次：每行各持一份会让 6 行 × 每帧各克隆一遍全量路径，
    // 目录越大越贵（实测 52 字段时每帧多 ~0.5ms）。
    let field_paths: Rc<[SharedString]> = Rc::from(field_paths);
    let mut rows = div().flex().flex_col().gap(px(6.));
    let count = builder.rows.len();
    let can_add = count < MONGO_QUERY_BUILDER_MAX_ROWS;
    for (index, (field, operator, value)) in builder.rows.iter().enumerate() {
        let row = mongo_query_builder_row(
            tab_id,
            index,
            field,
            *operator,
            value,
            &field_paths,
            colors,
            tone,
            window,
            cx,
        );
        rows = if index + 1 == count {
            // 末行：条件行吃掉剩余宽度，右侧接「添加条件」与快捷键提示。
            rows.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(div().flex_1().min_w(px(0.)).child(row))
                    .child(mongo_query_builder_add_button(
                        tab_id, can_add, colors, cx,
                    ))
                    .child(mongo_query_builder_run_hint(colors)),
            )
        } else {
            rows.child(row)
        };
    }
    if count == 0 {
        // 解析失败时说清楚为什么没条件行；否则只是「还没有条件」的空提示。
        let (message, color) = match &builder.error {
            Some(error) => (error.clone(), colors.danger),
            None => (tr("app.mongo.query.builder_empty"), colors.muted),
        };
        rows = rows.child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .text_size(px(10.))
                        .text_color(color)
                        .child(message),
                )
                .child(mongo_query_builder_add_button(
                    tab_id, can_add, colors, cx,
                ))
                .child(mongo_query_builder_run_hint(colors)),
        );
    }

    div()
        .debug_selector(|| "mongo-query-builder".to_string())
        .px(px(12.))
        .py(px(8.))
        .border_t_1()
        .border_color(colors.border_soft)
        .bg(tone.mint)
        .flex()
        .flex_col()
        .child(rows)
}

/// 一行条件：IF/AND | 字段 | 运算符 | 值 | 移除。
#[allow(clippy::too_many_arguments)]
fn mongo_query_builder_row(
    tab_id: TabId,
    index: usize,
    field: &Entity<EditorState>,
    operator: MongoQueryOperator,
    value: &Entity<EditorState>,
    field_paths: &Rc<[SharedString]>,
    colors: UiColors,
    tone: MongoQueryTone,
    window: &Window,
    cx: &mut Context<NavicatMain>,
) -> Div {
    div()
        .debug_selector(move || format!("mongo-query-builder-row:{index}"))
        .flex()
        .items_center()
        .gap_2()
        // 逻辑符：所有条件按 AND 组合，第一行写 IF 表示起点。
        .child(
            div()
                .w(px(21.))
                .h(px(18.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .when(index > 0, |this| {
                    this.border_1().border_color(tone.green_line).bg(tone.green_soft)
                })
                .text_size(px(8.))
                .font_family(crate::PLATFORM_MONOSPACE_FONT)
                .text_color(if index == 0 {
                    colors.muted
                } else {
                    colors.success
                })
                .child(if index == 0 { "IF" } else { "AND" }),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .items_center()
                // 字段与值按交互稿的 1 : 1.35 分配剩余宽度，两边都从 0 起算
                // （`flex_1` 的 basis 是 0）。按内容算基数时，字段格子里的 `w_full`
                // 会以整行宽度作为基准，把值格挤成一条缝。
                .child(
                    div()
                        .flex_1()
                        .min_w(px(MONGO_QUERY_BUILDER_FIELD_MIN_WIDTH))
                        .child(mongo_query_builder_field_cell(
                            tab_id, index, field, field_paths, colors, window, cx,
                        )),
                )
                .child(
                    div()
                        .w(px(MONGO_QUERY_BUILDER_OPERATOR_WIDTH))
                        .flex_none()
                        .child(mongo_query_builder_operator_cell(
                            tab_id, index, operator, colors, cx,
                        )),
                )
                .child(
                    div()
                        .flex_1()
                        .flex_grow(1.35)
                        .min_w(px(MONGO_QUERY_BUILDER_VALUE_MIN_WIDTH))
                        .child(mongo_query_builder_value_cell(
                            index, operator, value, colors, window, cx,
                        )),
                )
                .child(mongo_query_builder_remove_button(
                    tab_id, index, colors, cx,
                )),
        )
}

/// 字段格：可输入任意文档路径，右侧箭头展开字段目录作为候选。
fn mongo_query_builder_field_cell(
    tab_id: TabId,
    index: usize,
    field: &Entity<EditorState>,
    field_paths: &Rc<[SharedString]>,
    colors: UiColors,
    window: &Window,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let focused = field.read(cx).focus_handle(cx).is_focused(window);
    let caret = (!field_paths.is_empty()).then(|| {
        let view = cx.entity().downgrade();
        let paths = field_paths.clone();
        Button::new(SharedString::from(format!(
            "mongo-builder-field-menu:{}:{index}",
            tab_id.0
        )))
        .ghost()
        .small()
        .h(px(24.))
        .w(px(24.))
        .tooltip(tr("app.mongo.query.builder_field_menu"))
        .child(app_icon(AppIcon::ChevronDown, 12., colors.muted))
        // `TopLeft` 才是「贴着触发器下沿往下弹」：本库的 `BottomLeft` 指的是
        // 「弹层底边对齐触发器顶边」，即向上弹。条件行离窗口顶部还有整片结果区，
        // 往上弹会盖住正在编辑的条件。
        .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
            let mut menu = menu.min_w(px(220.));
            for path in paths.iter() {
                let path_text = path.to_string();
                let view = view.clone();
                menu = menu.item(PopupMenuItem::new(path.clone()).on_click(move |_, window, cx| {
                    let Some(view) = view.upgrade() else {
                        return;
                    };
                    let path_text = path_text.clone();
                    view.update(cx, |this, cx| {
                        this.set_mongo_builder_field(tab_id, index, path_text, window, cx);
                    });
                }));
            }
            menu
        })
    });
    mongo_query_box_at(
        MONGO_QUERY_BUILDER_CONTROL_HEIGHT,
        focused,
        colors,
        div()
            .h_full()
            .w_full()
            .flex()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    // 与 Filter 同一套编辑器外壳（顶边贴内容区顶部、超出裁掉）：
                    // 条件是裸路径，打字时挂补全，右侧箭头仍是「浏览整个字段目录」。
                    .child(mongo_query_editor_body(
                        field,
                        Some(format!("mongo-query-builder-field-body:{index}")),
                        11.,
                        None,
                    )),
            )
            .when_some(caret, |this, caret| this.child(caret)),
    )
    .debug_selector(move || format!("mongo-query-builder-field:{index}"))
}

/// 运算符格：固定八项，当前项打勾。
fn mongo_query_builder_operator_cell(
    tab_id: TabId,
    index: usize,
    current: MongoQueryOperator,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> impl IntoElement {
    let view = cx.entity().downgrade();
    (Button::new(SharedString::from(format!(
        "mongo-builder-operator:{}:{index}",
        tab_id.0
    )))
    .outline()
    .h(px(MONGO_QUERY_BUILDER_CONTROL_HEIGHT))
    .w_full()
    .text_size(px(11.))
    .font_family(crate::PLATFORM_MONOSPACE_FONT)
    .text_color(colors.success)
    .label(current.label())
    .dropdown_caret(true)
    // 同字段格：`TopLeft` 向下弹。运算符下拉在条件行里，向上弹会盖住字段格与值格。
    .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
        let mut menu = menu.min_w(px(200.));
        for operator in MongoQueryOperator::ALL {
            let view = view.clone();
            menu = menu.item(
                PopupMenuItem::new(operator.label())
                    .checked(operator == current)
                    .on_click(move |_, window, cx| {
                        let Some(view) = view.upgrade() else {
                            return;
                        };
                        view.update(cx, |this, cx| {
                            this.set_mongo_builder_operator(tab_id, index, operator, window, cx);
                        });
                    }),
            );
        }
        menu
    }))
    .into_any_element()
}

/// 值格：`$exists` 时禁用——它的值恒为布尔，不需要输入。
fn mongo_query_builder_value_cell(
    index: usize,
    operator: MongoQueryOperator,
    value: &Entity<EditorState>,
    colors: UiColors,
    window: &Window,
    cx: &App,
) -> Div {
    let focused = value.read(cx).focus_handle(cx).is_focused(window);
    mongo_query_box_at(
        MONGO_QUERY_BUILDER_CONTROL_HEIGHT,
        focused,
        colors,
        div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .overflow_hidden()
            .child(
                div()
                    .debug_selector(move || format!("mongo-query-builder-value-body:{index}"))
                    .w_full()
                    .child(
                        Editor::new(value)
                            .appearance(false)
                            .bordered(false)
                            .disabled(operator == MongoQueryOperator::Exists)
                            .w_full()
                            .text_size(px(11.))
                            .font_family(crate::PLATFORM_MONOSPACE_FONT),
                    ),
            ),
    )
    .debug_selector(move || format!("mongo-query-builder-value:{index}"))
}

fn mongo_query_builder_remove_button(
    tab_id: TabId,
    index: usize,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    div().w(px(28.)).flex_none().flex().items_center().justify_center().child(
        Button::new(SharedString::from(format!(
            "mongo-builder-remove:{}:{index}",
            tab_id.0
        )))
        .debug_selector(move || format!("mongo-query-builder-remove:{index}"))
        .ghost()
        .small()
        .h(px(28.))
        .w(px(28.))
        .tooltip(tr("app.mongo.query.builder_remove"))
        .child(app_icon(AppIcon::Close, 12., colors.muted))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.remove_mongo_builder_condition(tab_id, index, window, cx);
            cx.stop_propagation();
        })),
    )
}

fn mongo_query_builder_add_button(
    tab_id: TabId,
    enabled: bool,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Button {
    Button::new(("mongo-query-builder-add", tab_id.0))
        .debug_selector(|| "mongo-query-builder-add".to_string())
        .ghost()
        .small()
        .h(px(22.))
        .text_size(px(10.))
        .text_color(colors.success)
        .label(tr("app.mongo.query.builder_add"))
        .child(app_icon(AppIcon::Plus, 12., colors.success))
        .disabled(!enabled)
        .on_click(cx.listener(move |this, _, window, cx| {
            this.add_mongo_builder_condition(tab_id, window, cx);
            cx.stop_propagation();
        }))
}

/// 执行查询的快捷键提示：`kbd` 小方框 + 文案，靠右对齐。
fn mongo_query_builder_run_hint(colors: UiColors) -> Div {
    let kbd = |text: &'static str| {
        div()
            .px(px(4.))
            .py(px(1.))
            .rounded(px(3.))
            .border_1()
            .border_color(colors.border)
            .bg(colors.panel_bg)
            .text_size(px(9.))
            .font_family(crate::PLATFORM_MONOSPACE_FONT)
            .text_color(colors.muted)
            .child(text)
    };
    div()
        .ml_auto()
        .flex()
        .items_center()
        .gap(px(4.))
        .text_size(px(9.))
        .text_color(colors.muted)
        .child(kbd(if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" }))
        .child(kbd("↵"))
        .child(tr("app.mongo.query.builder_run_hint"))
}

#[cfg(test)]
mod mongo_query_builder_tests {
    use super::*;

    /// 条件行 → JSON：等值平铺、其余包 `$op`，`$in` 按逗号拆开，空值跳过。
    #[test]
    fn conditions_build_filter_json() {
        let mut document = serde_json::Map::new();
        for (field, operator, text) in [
            ("status", MongoQueryOperator::Eq, "paid"),
            ("amount", MongoQueryOperator::Gte, "500"),
        ] {
            let value = mongo_builder_condition_value(operator, text).expect("value");
            document.insert(field.into(), mongo_builder_expression(operator, value));
        }
        assert_eq!(
            serde_json::Value::Object(document).to_string(),
            r#"{"status":"paid","amount":{"$gte":500}}"#
        );
        // 空值不产出条件：加了一行还没填不该把结果查空。
        assert!(mongo_builder_condition_value(MongoQueryOperator::Eq, "  ").is_none());
        // `$exists` 没有空值概念，值恒为布尔。
        assert_eq!(
            mongo_builder_condition_value(MongoQueryOperator::Exists, ""),
            Some(serde_json::Value::Bool(true))
        );
        assert_eq!(
            mongo_builder_condition_value(MongoQueryOperator::In, "a, 2"),
            Some(serde_json::Value::Array(vec![
                serde_json::Value::String("a".into()),
                serde_json::Value::from(2)
            ]))
        );
    }

    /// JSON → 条件行 → JSON 必须原样往返，尤其别把字符串 `"500"` 变成数字。
    #[test]
    fn filter_round_trips_through_conditions() {
        let filter = r#"{"code":"500","amount":500,"ok":true,"tags":{"$in":["a","b"]},"note":{"$exists":false}}"#;
        let conditions = parse_mongo_builder_conditions(filter).expect("supported filter");
        let mut document = serde_json::Map::new();
        for (field, operator, text) in conditions {
            let value = mongo_builder_condition_value(operator, &text).expect("value");
            document.insert(field, mongo_builder_expression(operator, value));
        }
        assert_eq!(serde_json::Value::Object(document).to_string(), filter);
    }

    /// 复杂表达式的判断标准：拆不成「字段 / 运算符 / 值」就拒绝，交给手写 JSON。
    #[test]
    fn complex_filters_are_rejected() {
        for filter in [
            r#"{"$or":[{"a":1},{"b":2}]}"#,
            r#"{"a":{"$regex":"^x"}}"#,
            r#"{"a":{"b":1}}"#,
            r#"{"a":{"$gt":1,"$lt":9}}"#,
            r#"{"a":[1,2]}"#,
            r#"{"a":{"$exists":"yes"}}"#,
            r#"not json"#,
            r#"[1,2]"#,
        ] {
            assert!(
                parse_mongo_builder_conditions(filter).is_err(),
                "{filter} must not be rewritten by the builder"
            );
        }
        assert!(parse_mongo_builder_conditions("  ").unwrap().is_empty());
    }

    /// Compass 查询栏常见的宽松写法要能展开：连接器执行时本来就收（先严格 JSON、
    /// 再 JSON5），构建器若只认严格 JSON，就会出现「能查、构建器却报语法错」。
    #[test]
    fn relaxed_filter_syntax_parses() {
        // 无引号键（截图里那条）。
        let conditions = parse_mongo_builder_conditions(r#"{event_type: "page_view"}"#)
            .expect("unquoted key is a Compass-style filter");
        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0].0, "event_type");
        assert_eq!(conditions[0].1, MongoQueryOperator::Eq);
        assert_eq!(conditions[0].2, r#""page_view""#);

        // 单引号、尾逗号、注释也一并收。
        let conditions = parse_mongo_builder_conditions(
            "{\n  // 只看已支付\n  status: 'paid',\n  amount: 500,\n}",
        )
        .expect("single quotes, trailing comma and comments are accepted");
        assert_eq!(conditions.len(), 2);

        // 但 `NaN` 会被读成 null，等于把条件换成「等于 null」——必须拒。
        assert!(parse_mongo_builder_conditions("{amount: NaN}").is_err());
        assert!(parse_mongo_builder_conditions("{amount: Infinity}").is_err());
    }
}

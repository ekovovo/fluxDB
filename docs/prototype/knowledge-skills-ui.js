/* 独立交互稿：所有资源和匹配均为内存中的演示数据，不访问实际连接。 */
const $ = (selector, root = document) => root.querySelector(selector);
const esc = value => String(value).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const paths = {
  home:'<path d="m3 10 9-7 9 7v10a1 1 0 0 1-1 1h-5v-8H9v8H4a1 1 0 0 1-1-1Z"/>',
  database:'<ellipse cx="12" cy="5" rx="8" ry="3"/><path d="M4 5v7c0 4 16 4 16 0V5M4 12v7c0 4 16 4 16 0v-7"/>',
  library:'<path d="M4 4h4v16H4zM11 4h4v16h-4zM17 5l3-1 4 15-3 1z" transform="translate(-1 0)"/>',
  book:'<path d="M4 3h12a3 3 0 0 1 3 3v15H7a3 3 0 0 1-3-3Zm0 15a3 3 0 0 1 3-3h12M8 7h7M8 10h5"/>',
  skill:'<path d="m12 3 2.4 6.6L21 12l-6.6 2.4L12 21l-2.4-6.6L3 12l6.6-2.4Z"/>',
  sparkles:'<path d="m9 5 2 5 5 2-5 2-2 5-2-5-5-2 5-2ZM18 2l1.2 3.8L23 7l-3.8 1.2L18 12l-1.2-3.8L13 7l3.8-1.2Z"/>',
  settings:'<path d="m9 3-.7 3-2.8.9-2.3-.7L2 9l2 2v2l-2 2 1.2 2.8 2.3-.7 2.8.9.7 3h6l.7-3 2.8-.9 2.3.7L22 15l-2-2v-2l2-2-1.2-2.8-2.3.7-2.8-.9L15 3Z"/><circle cx="12" cy="12" r="3"/>',
  sun:'<circle cx="12" cy="12" r="4"/><path d="M12 2v2m0 16v2M2 12h2m16 0h2M5 5l1.5 1.5m11 11L19 19M5 19l1.5-1.5m11-11L19 5"/>',
  moon:'<path d="M20.9 13A9 9 0 0 1 11 3.1 9 9 0 1 0 20.9 13Z"/>',
  grid:'<rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/><rect x="14" y="14" width="7" height="7" rx="1"/>',
  clock:'<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>',
  archive:'<path d="M4 8h16v12H4zM3 3h18v5H3zM10 12h4"/>',
  globe:'<circle cx="12" cy="12" r="9"/><ellipse cx="12" cy="12" rx="4" ry="9"/><path d="M3 12h18"/>',
  table:'<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18M9 9v11"/>',
  terminal:'<rect x="3" y="4" width="18" height="16" rx="2"/><path d="m7 9 3 3-3 3m6 0h4"/>',
  api:'<path d="m8 5-6 7 6 7m8-14 6 7-6 7m-3-16-2 18"/>',
  search:'<circle cx="10.5" cy="10.5" r="6.5"/><path d="m16 16 5 5"/>',
  plus:'<path d="M12 5v14M5 12h14"/>',
  upload:'<path d="M12 16V3m-5 5 5-5 5 5M4 15v6h16v-6"/>',
  link:'<path d="m10 13 4-4m-5 7-2 2a4 4 0 0 1-6-6l4-4a4 4 0 0 1 6 0m2 0 2-2a4 4 0 0 1 6 6l-4 4a4 4 0 0 1-6 0" transform="translate(1 0)"/>',
  check:'<path d="m5 12 4 4L19 6"/>',
  circleCheck:'<circle cx="12" cy="12" r="9"/><path d="m8 12 3 3 5-6"/>',
  info:'<circle cx="12" cy="12" r="9"/><path d="M12 11v6m0-10v.1"/>',
  'arrow-right':'<path d="M4 12h16m-6-6 6 6-6 6"/>',
  pencil:'<path d="m15 4 5 5M4 20l5-1L21 7a2 2 0 0 0-5-5L4 14Z"/>',
  file:'<path d="M14 2H5v20h14V7Zm0 0v5h5M8 12h8m-8 4h6"/>',
  close:'<path d="m6 6 12 12M6 18 18 6"/>',
  rotate:'<path d="M3 10a9 9 0 1 1 2 8M3 4v6h6"/>',
  shield:'<path d="m12 3 8 3v6c0 5-8 9-8 9s-8-4-8-9V6Z"/><path d="m8 12 3 3 5-6"/>',
  target:'<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="5"/><circle cx="12" cy="12" r="1"/>'
};
const icon = name => `<svg viewBox="0 0 24 24" aria-hidden="true">${paths[name] || paths.file}</svg>`;
function hydrate(root = document) {
  root.querySelectorAll('[data-icon]').forEach(el => { el.innerHTML = icon(el.dataset.icon); });
}

const contexts = {
  database:{name:'orders', parent:'商城生产库', path:'商城生产库 / shop_prod / orders', icon:'table', scope:'database', task:'分析上个月的营收变化'},
  ssh:{name:'payment-service', parent:'支付服务主机', path:'SSH / payment-prod / payment-service', icon:'terminal', scope:'ssh', task:'排查支付服务错误日志'},
  api:{name:'GET /v1/orders', parent:'订单开放接口', path:'API / 商城开放平台 / GET /v1/orders', icon:'api', scope:'api', task:'核验订单接口的分页与金额字段'}
};
const scopes = {
  database:{label:'商城生产库 · orders', detail:'数据库 / shop_prod / orders', icon:'table'},
  ssh:{label:'支付服务主机', detail:'SSH / payment-prod / payment-service', icon:'terminal'},
  api:{label:'订单开放接口', detail:'API / 商城开放平台 / GET /v1/orders', icon:'api'},
  global:{label:'通用资源', detail:'不限定具体对象；仍按任务选择', icon:'globe'}
};
let resources = [
  {id:'revenue',type:'knowledge',title:'营收统计口径',summary:'已支付订单计入营收，退款冲减，按北京时间归属自然月。',scopes:['database','api'],tasks:['营收','收入','报表','金额'],status:'confirmed',source:'财务与数据团队 · 月报口径说明',updated:'今天 10:24',body:'### 统计范围\n仅计入已支付订单，以实际支付金额为准，不使用订单原价。\n\n### 退款与取消\n- 已取消、未支付的订单不计入营收。\n- 退款金额冲减退款发生月份的营收。\n- 部分退款仅冲减已退款的金额，不排除整笔订单。\n\n### 时间与金额\n- 业务时区统一为 **Asia/Shanghai（北京时间）**。\n- 按自然月统计，采用左闭右开的时间范围。\n- 数据库 `paid_amount` 与接口 `paid_amount` 均以分为单位，展示时换算为元。\n\n### 使用约束\n缺少支付时间或退款明细时，应说明数据缺口，不得把订单创建时间当作支付时间。'},
  {id:'order-status',type:'knowledge',title:'订单状态与表关系',summary:'订单状态流转，以及 orders、payments 与 refunds 的关联方式。',scopes:['database'],tasks:['订单','状态','关联','退款'],status:'confirmed',source:'订单服务文档 · 数据字典',updated:'昨天 16:08',body:'### 订单状态\n- `pending`：订单已创建，尚未支付。\n- `paid`：支付成功。\n- `cancelled`：订单已取消。\n- `refunded`：订单已全额退款。\n\n### 关联关系\n`payments.order_id` 关联 `orders.id`，一笔订单可能有多次支付尝试。\n\n`refunds.payment_id` 关联 `payments.id`，同一支付可能发生多次部分退款。\n\n### 查询注意事项\n不要直接对多个明细表联结后求和，应先按订单或支付聚合，避免金额被重复放大。'},
  {id:'sql-diagnosis',type:'skill',title:'SQL 性能诊断',summary:'从执行计划出发，定位扫描、索引与联结问题，给出可验证的建议。',scopes:['database'],tasks:['SQL','慢','性能','索引','执行计划'],status:'available',source:'团队维护 · SQL 诊断规范',updated:'昨天 14:32',body:'### 何时使用\n用户希望解释慢查询、检查索引，或比较 SQL 改写效果时使用。\n\n### 诊断步骤\n1. 确认数据库类型、版本、目标 SQL 和相关表结构。\n2. 使用获授权的只读工具取得执行计划，先检查扫描规模与联结顺序。\n3. 区分估算与实际指标；没有实际运行数据时，不声称已测得耗时。\n4. 给出改写或索引建议，并说明收益条件、写入成本和验证方法。\n\n### 输出要求\n按 **观察 → 原因 → 建议 → 验证方法** 组织结果。\n\n### 安全边界\n不自动创建索引、不修改数据库配置；可能实际执行查询的诊断命令须遵守工具授权。'},
  {id:'report',type:'skill',title:'经营分析报告',summary:'先核对指标口径，再组织趋势、差异与证据，明确数据局限。',scopes:['database','api'],tasks:['营收','经营','报告','分析','报表'],status:'available',source:'团队维护 · 分析规范',updated:'10 月 14 日',body:'### 何时使用\n用户需要营收、订单或经营指标的分析报告时使用。\n\n### 工作方式\n1. 先读取当前资源关联的已确认业务口径。\n2. 明确时间范围、比较基准和数据完整性，再请求所需数据。\n3. 先展示总体变化，再解释主要贡献项；相关性不直接表述为因果。\n\n### 交付结构\n- **结论**：先回答用户的问题。\n- **指标与趋势**：标注单位、时间范围与口径来源。\n- **差异解释**：使用实际数据支撑，不编造原因。\n- **限制与建议**：披露缺失数据与后续核验项。'},
  {id:'service-map',type:'knowledge',title:'支付服务运行约定',summary:'服务名称、日志目录与实例关系；不包含主机凭据。',scopes:['ssh'],tasks:['支付','服务','日志','排查'],status:'confirmed',source:'运维团队 · 服务手册',updated:'10 月 13 日',body:'### 服务定位\n支付服务的逻辑名称为 `payment-service`，运行在支付生产主机组。\n\n### 日志约定\n应用日志使用结构化 JSON，包含时间、级别、trace_id 与错误码。实际日志位置应通过已授权工具核实，不假设部署路径。\n\n### 排查边界\n先只读定位错误与关联请求。重启、配置变更与日志清理都不是默认排障步骤，必须走单独授权。'},
  {id:'ssh-triage',type:'skill',title:'服务日志排障',summary:'按时间窗和请求链路定位异常，避免无界拉取或泄露敏感日志。',scopes:['ssh'],tasks:['排查','日志','错误','服务','故障'],status:'available',source:'团队维护 · 运维规范',updated:'10 月 13 日',body:'### 何时使用\n用户需要定位服务异常、错误日志或请求失败原因时使用。\n\n### 排查步骤\n1. 确认目标主机、服务和时间范围。\n2. 获取有界的日志片段，按错误码与 trace_id 关联。\n3. 区分观察事实与原因假设，给出需要补充的证据。\n\n### 数据与权限\n不得读取凭据文件，不自动扩大到其他主机，不把完整日志目录传给模型。输出前脱敏令牌、用户身份和业务敏感字段。'},
  {id:'api-contract',type:'knowledge',title:'订单 API 字段与分页约定',summary:'金额单位为分；基于游标分页，签名凭据不属于业务知识正文。',scopes:['api'],tasks:['接口','分页','字段','金额','订单'],status:'confirmed',source:'开放平台 · API 契约',updated:'10 月 12 日',body:'### 字段语义\n`paid_amount` 以分为单位。`created_at` 是订单创建时间，不是支付时间。\n\n### 分页约定\n使用 `next_cursor` 续取下一页；为空表示本次遍历结束。不要把单页记录数当作符合条件的总数。\n\n### 隐私与凭据\n知识条目只记录协议与字段语义，不保存 Authorization、签名密钥或实际用户响应数据。'},
  {id:'quality-draft',type:'knowledge',title:'测试订单识别规则',summary:'待业务确认：测试订单的排除条件与历史数据例外。',scopes:['database'],tasks:['订单','测试','营收'],status:'proposed',source:'对话提议 · 尚未确认',updated:'今天 09:16',body:'### 待确认的规则\n测试订单是否可以依据特定订单标记排除，需要业务方核实。\n\n### 尚未明确\n历史数据中是否存在缺失标记的测试订单？该规则是否适用于所有时间范围？\n\n用户确认前，不应将这条提议作为统计事实。'}
];
let state = {category:'all',scope:'all',search:'',selected:'revenue',tab:'body',active:'database',preview:false,turn:null,readIds:new Set(),editing:null,newType:'knowledge'};
let toastTimer;
let modalReturnFocus, sheetReturnFocus;
const typeLabel = r => r.type === 'skill' ? 'Skill' : '业务知识';
const stateLabel = r => ({confirmed:'已确认',available:'可用',proposed:'待确认',retired:'已停用'}[r.status]);
const available = r => ['confirmed','available'].includes(r.status);
function scopedResources(context) {
  return resources.filter(r => available(r) && (r.scopes.includes(context) || r.scopes.includes('global')));
}
// 演示匹配只使用示例资源关联和关键词，不代表生产检索或语义模型。
function matchResources(context, task) {
  const candidates = scopedResources(context);
  return candidates.filter(r => !task.trim() || r.tasks.some(word => task.toLowerCase().includes(word.toLowerCase())));
}
function toast(message) {
  clearTimeout(toastTimer);
  $('#toast').textContent = message;
  $('#toast').hidden = false;
  toastTimer = setTimeout(() => { $('#toast').hidden = true; }, 3200);
}
function navRow(id,label,name,count,active,kind) {
  return `<button class="nav-row ${active?'active':''}" data-${kind}="${id}" ${active?'aria-current="true"':''}>${icon(name)}<span>${label}</span><span class="count">${count}</span></button>`;
}
function renderNav() {
  $('#mobile-category').value=state.category;
  $('#mobile-scope').value=state.scope;
  $('#category-nav').innerHTML = [
    ['all','全部资源','grid',resources.length],
    ['knowledge','业务知识','book',resources.filter(r=>r.type==='knowledge').length],
    ['skill','Skills','skill',resources.filter(r=>r.type==='skill').length],
    ['proposed','待确认','clock',resources.filter(r=>r.status==='proposed').length],
    ['retired','已停用','archive',resources.filter(r=>r.status==='retired').length]
  ].map(([id,label,name,count],i) => `${i===3?'<div class="nav-rule"></div>':''}${navRow(id,label,name,count,state.category===id,'category')}`).join('');
  $('#scope-nav').innerHTML = [
    ['all','全部适用资源','globe'],
    ['database','数据库','database'],
    ['ssh','SSH 主机','terminal'],
    ['api','API 接口','api'],
    ['global','通用','globe']
  ].map(([id,label,name])=>navRow(id,label,name,id==='all'?resources.length:resources.filter(r=>r.scopes.includes(id)).length,state.scope===id,'scope')).join('');
}
function filteredResources() {
  return resources.filter(r => {
    const category = state.category==='all' || r.type===state.category || r.status===state.category;
    const scope = state.scope==='all' || r.scopes.includes(state.scope);
    const text = `${r.title} ${r.summary}`.toLowerCase().includes(state.search.toLowerCase());
    return category && scope && text;
  });
}
function renderList() {
  const rows = filteredResources();
  if (!rows.some(r => r.id===state.selected)) state.selected = rows[0]?.id || null;
  $('#list-heading').textContent = ({all:'全部资源',knowledge:'业务知识',skill:'Skills',proposed:'待确认',retired:'已停用'}[state.category]) + (state.scope==='all'?'':` · ${ {database:'数据库',ssh:'SSH',api:'API',global:'通用'}[state.scope] }`);
  $('#list-count').textContent = `${rows.length} 项`;
  $('#resource-list').innerHTML = rows.length ? rows.map(r => `<button class="resource-card ${r.id===state.selected?'selected':''}" data-resource="${r.id}" aria-pressed="${r.id===state.selected}">
    <span class="resource-icon ${r.type}">${icon(r.type==='skill'?'skill':'book')}</span><span class="resource-copy">
    <span class="resource-title"><span>${esc(r.title)}</span><span class="mini-status ${r.status==='proposed'?'pending':''}" title="${stateLabel(r)}">${icon(r.status==='proposed'?'clock':r.status==='retired'?'archive':'circleCheck')}</span></span>
    <span class="resource-desc" style="display:block">${esc(r.summary)}</span>
    <span class="resource-meta">${icon(scopes[r.scopes[0]].icon)}<span>${esc(r.scopes[0]==='database'?'shop_prod · orders':r.scopes[0]==='ssh'?'payment-prod':r.scopes[0]==='api'?'订单 API':'通用')}${r.scopes.length>1?` +${r.scopes.length-1}`:''}</span><span class="mini-dot"></span><span>${typeLabel(r)}</span></span></span>
  </button>`).join('') : `<div class="empty">${icon('search')}<span>没有匹配的资源</span><button class="btn" id="clear-search">清除筛选</button></div>`;
  $('#total-count').textContent = `${resources.length} 项资源`;
}
function markdown(text) {
  let codeBlocks = [];
  let safe = esc(text).replace(/```(?:\w+)?\n([\s\S]*?)```/g, (_,code) => {
    codeBlocks.push(`<pre><code>${code.trim()}</code></pre>`);
    return `BLOCKTOKEN${codeBlocks.length-1}`;
  });
  let list = '';
  let html = safe.split('\n').map(line => {
    const inline = str => str.replace(/\*\*(.+?)\*\*/g,'<strong>$1</strong>').replace(/`([^`]+)`/g,'<code>$1</code>');
    const ordered = /^\d+\.\s/.test(line), unordered = /^-\s/.test(line);
    let out = '';
    if (!ordered && !unordered && list) { out = `</${list}>`; list=''; }
    if (ordered || unordered) {
      const next = ordered?'ol':'ul';
      if (list!==next) { if(list) out+=`</${list}>`; out+=`<${next}>`; list=next; }
      return `${out}<li>${inline(line.replace(/^(?:\d+\.|-)\s/,''))}</li>`;
    }
    if (line.startsWith('### ')) return `${out}<h3>${inline(line.slice(4))}</h3>`;
    if (/^BLOCKTOKEN\d+$/.test(line)) return out + codeBlocks[Number(line.slice(10))];
    return out + (line.trim()?`<p>${inline(line)}</p>`:'');
  }).join('');
  return html + (list?`</${list}>`:'');
}
function renderDetail() {
  const r = resources.find(r=>r.id===state.selected);
  if (!r) { $('#detail').innerHTML = '<div class="empty">选择或新建一项资源，查看内容与适用范围。</div>'; return; }
  const matched = scopedResources(state.active).some(x=>x.id===r.id);
  let content;
  if (state.tab==='scope') {
    content = `<p class="scope-intro">独立保存一份内容，可关联多个资源。切换活动对象时，按本轮目标重新发现。</p>
    ${r.scopes.map(s=>`<div class="association"><span data-icon="${scopes[s].icon}"></span><div><h4>${esc(scopes[s].label)}</h4><p>${esc(scopes[s].detail)}</p></div><span class="tag">${s==='global'?'按任务发现':'关联对象'}</span></div>`).join('')}
    <button class="text-btn" data-action="edit">${icon('plus')}编辑关联资源</button>
    <div class="scope-section-label">任务线索</div><div>${r.tasks.map(t=>`<span class="task-chip">${esc(t)}</span>`).join('')}</div>
    <p class="form-help">用于演示按任务发现，不是必须输入的口令。生产检索可结合资源范围、描述与任务语义。</p>
    <div class="usage-card"><div class="usage-head">${icon('shield')}关联不是授权</div><p>关联 SSH 主机或 API 不会授予执行权限，也不会读取凭据、日志或响应数据。这里只管理知识与技能内容。</p></div>`;
  } else {
    content = `<div class="document-label">${icon('file')}${r.type==='skill'?'任务说明 · 按需加载':'业务规则 · 用户确认后可用'}</div>
    <div class="prose">${markdown(r.body)}</div>
    <div class="source-note">${icon('link')}来源：${esc(r.source)}</div>
    <div class="usage-card"><div class="usage-head">${icon('sparkles')}按需提供给 AI<button class="text-btn" data-action="preview">查看预览${icon('arrow-right')}</button></div>
    <div class="flow"><span class="flow-step"><b>1</b>匹配当前资源</span><span class="flow-line"></span><span class="flow-step"><b>2</b>提供名称与简述</span><span class="flow-line"></span><span class="flow-step"><b>3</b>按需读取正文</span></div>
    <p>${available(r)?'默认支持，无需开启插件。正文不会随资源目录全量发送；关联资源的数据仍需通过授权工具读取。':r.status==='proposed'?'这条知识尚未确认，不进入模型可见目录，也不作为业务事实使用。':'这项资源已停用，不进入后续轮次的可见目录。'}</p></div>`;
  }
  $('#detail').innerHTML = `<div class="detail-head"><div class="detail-topline"><div class="labels">
    <span class="tag ${r.type==='skill'?'green':'blue'}">${icon(r.type==='skill'?'skill':'book')}${typeLabel(r)}</span>
    <span class="tag ${r.status==='proposed'?'amber':available(r)?'green':''}">${icon(r.status==='proposed'?'clock':available(r)?'check':'archive')}${stateLabel(r)}</span>
    ${matched?'<span class="tag">当前资源相关</span>':''}</div>
    <div class="detail-actions"><button class="btn" data-action="edit" aria-label="编辑">${icon('pencil')}<span>编辑</span></button><button class="icon-btn" data-action="retire" title="${r.status==='retired'?'已停用':r.type==='knowledge'?'停用知识':'停用 Skill'}" aria-label="停用资源" ${r.status==='retired'?'disabled':''}>${icon('archive')}</button></div></div>
    <h2>${esc(r.title)}</h2><p class="detail-subtitle">${esc(r.summary)}</p>
    <div class="detail-meta"><span>${icon('link')}${r.scopes.length} 个适用范围</span><span>${icon('clock')}${esc(r.updated)}更新</span><span>${icon('shield')}本地维护</span>
    ${r.status==='proposed'?'<button class="text-btn" data-action="confirm">确认业务知识</button>':''}</div></div>
    <div class="detail-tabs" role="tablist" aria-label="资源详情"><button class="detail-tab ${state.tab==='body'?'active':''}" data-tab="body" role="tab" aria-selected="${state.tab==='body'}">正文</button><button class="detail-tab ${state.tab==='scope'?'active':''}" data-tab="scope" role="tab" aria-selected="${state.tab==='scope'}">适用范围 <span style="font-size:10px;margin-left:4px">${r.scopes.length}</span></button></div>
    <div class="detail-scroll" role="tabpanel">${content}</div>`;
  hydrate($('#detail'));
}
function renderContext() {
  const context = contexts[state.active];
  $('#active-name').textContent = context.name;
  $('#active-path').textContent = context.path;
  $('#active-icon').dataset.icon = context.icon;
  hydrate($('.context-bar'));
  $('#active-count').textContent = `${scopedResources(state.active).length} 项相关资源`;
}
function render() { renderNav(); renderList(); renderDetail(); renderContext(); }

function openPreview() {
  sheetReturnFocus = document.activeElement;
  state.preview=true;
  renderPreview();
  $('#sheet-root .sheet .icon-btn').focus();
}
function closePreview() {
  state.preview=false; $('#sheet-root').innerHTML='';
  if(sheetReturnFocus?.isConnected) sheetReturnFocus.focus();
}
function renderPreview() {
  if (!state.preview) return;
  const context = contexts[state.active];
  const turn = state.turn;
  const html = `<div class="mask" id="preview-mask"><section class="sheet" role="dialog" aria-modal="true" aria-labelledby="preview-title">
    <header class="sheet-head">${icon('sparkles')}<h2 id="preview-title">上下文与按需读取</h2><button class="icon-btn" id="close-preview" aria-label="关闭预览">${icon('close')}</button></header>
    <div class="sheet-body"><p class="sheet-lead">切换一个活动对象，模拟用户发送任务。先发现相关目录，再按需读取内容；不自动读取对象的数据。</p>
    <div class="field-label">模拟当前活动对象<small>SSH / API 为未来场景示意</small></div>
    <div class="context-options">${Object.entries(contexts).map(([id,c])=>`<button class="${state.active===id?'active':''}" data-context="${id}">${icon(c.icon)}${id==='database'?'数据库':id==='ssh'?'SSH':'API'}</button>`).join('')}</div>
    <div class="context-detail"><strong>${esc(context.name)}</strong><p>${esc(context.path)}</p></div>
    <label class="field-label" for="preview-task">用户任务</label><input class="input" id="preview-task" value="${esc(state.previewTask??context.task)}" maxlength="300">
    <button class="btn primary preview-run" id="run-preview">${icon(turn?'rotate':'sparkles')}${turn?'以当前上下文开始新一轮':'模拟发送 · 发现相关资源'}</button>
    <div class="preview-result" id="preview-result">${turn?renderTurn(turn):`<div class="empty">${icon('target')}<span>发送后查看本轮可见目录<br><small>未确认或范围不匹配的内容不会加入</small></span></div>`}</div></div>
    <div class="privacy-note">${icon('shield')}<span>仅演示名称、定位、简述及知识正文。连接密码、SSH 凭据、API Key 不进入上下文；业务数据通过授权工具按需获取。</span></div>
    </section></div>`;
  $('#sheet-root').innerHTML=html;
}
function renderTurn(turn) {
  return `<div class="turn-note">${state.active!==turn.context?'活动对象已切换；当前预览仍绑定上一轮目标。点击上方按钮才按新目标重新发现。':'本轮目标已固定，不随页面切换而改变。'}<br><strong>${esc(contexts[turn.context].path)}</strong></div>
  <div class="section-kicker"><span class="num">1</span>模型先看到的目录<span style="margin-left:auto">${turn.ids.length} 项</span></div>
  ${turn.ids.length?turn.ids.map(id=>resources.find(r=>r.id===id)).filter(Boolean).map(r=>`<div class="hint-card"><div class="hint-title">${icon(r.type==='skill'?'skill':'book')}${esc(r.title)}<span class="tag ${r.type==='skill'?'green':'blue'}" style="margin-left:auto">${typeLabel(r)}</span></div>
  <p>${esc(r.summary)}</p><div class="hint-foot"><span>${state.readIds.has(r.id)?'已模拟读取正文':'仅名称与简述 · 正文未加载'}</span><button class="text-btn" data-read="${r.id}">${state.readIds.has(r.id)?'收起正文':'模拟按需读取'}${icon('arrow-right')}</button></div>
  ${state.readIds.has(r.id)?`<div class="read-content">${esc(r.body)}</div>`:''}</div>`).join(''):'<div class="empty">本轮没有匹配的已确认知识或可用 Skill。<br>不会为了填充上下文而引用其他资源。</div>'}
  <p class="form-help" style="margin-top:12px">此处采用示例范围＋关键词演示匹配，不调用模型；点击“模拟按需读取”才展开对应正文。</p>`;
}

function openEditor(id = null, imported = null) {
  modalReturnFocus = document.activeElement;
  state.editing = id;
  const existing = resources.find(r=>r.id===id);
  state.newType=existing?.type || imported?.type || 'knowledge';
  const draft = existing || {title:imported?.title||'',summary:'',body:imported?.body||'',source:'手动添加',scopes:state.scope==='all'?[state.active]:[state.scope],tasks:[]};
  $('#modal-root').innerHTML = `<div class="modal-mask" id="editor-mask"><section class="modal" role="dialog" aria-modal="true" aria-labelledby="editor-title">
  <header class="modal-head"><h2 id="editor-title">${id?'编辑资源':'新建资源'}</h2><button class="icon-btn" id="close-editor" aria-label="关闭编辑">${icon('close')}</button></header>
  <form id="editor-form" style="display:contents"><div class="modal-body">
  <div class="form-field"><div class="field-label">资源类型</div><div class="type-options">
  ${['knowledge','skill'].map(type=>`<button type="button" class="type-option ${type===state.newType?'active':''}" data-type="${type}" ${id?'disabled':''}>${icon(type==='skill'?'skill':'book')}<div>${type==='skill'?'Skill':'业务知识'}<p>${type==='skill'?'某类任务应该如何完成':'定义、规则与资源语义'}</p></div></button>`).join('')}</div></div>
  <div class="form-field"><label class="field-label" for="edit-title">名称</label><input class="input" id="edit-title" value="${esc(draft.title)}" placeholder="例如：营收统计口径" required maxlength="80"></div>
  <div class="form-field"><label class="field-label" for="edit-summary">简述 <small>先提供给模型，用于判断是否需要正文</small></label><input class="input" id="edit-summary" value="${esc(draft.summary)}" placeholder="一句话说明内容和使用时机" required maxlength="200"></div>
  <div class="form-field"><label class="field-label" for="edit-body">正文 <small>支持 Markdown</small></label><textarea class="input" id="edit-body" required maxlength="20000" placeholder="填写规则、适用条件或任务步骤；不要放入凭据和实际业务数据。">${esc(draft.body)}</textarea></div>
  <div class="form-field"><div class="field-label">关联资源 <small>演示资源，可多选</small></div><div class="check-grid">${Object.entries(scopes).map(([key,s])=>`<label class="check-item"><input type="checkbox" name="scope" value="${key}" ${draft.scopes.includes(key)?'checked':''}>${icon(s.icon)}${esc(s.label)}</label>`).join('')}</div><p class="form-help">关联控制发现范围，不授予访问权限。“通用”不限定具体对象，但仍需匹配任务。</p></div>
  <div class="form-row"><div class="form-field"><label class="field-label" for="edit-source">来源</label><input class="input" id="edit-source" value="${esc(draft.source)}" maxlength="120"></div><div class="form-field"><label class="field-label" for="edit-tasks">任务线索 <small>以逗号分隔</small></label><input class="input" id="edit-tasks" value="${esc(draft.tasks.join('，'))}" placeholder="营收，分析，报表" maxlength="150"></div></div>
  <div id="editor-error" class="form-error" role="alert"></div></div>
  <footer class="modal-foot"><span class="foot-note" id="editor-foot-note">${state.newType==='knowledge'?'新知识保存为待确认，确认后才供 AI 使用。':'Skill 保存后可按上下文发现。'}</span><button class="btn" type="button" id="cancel-editor">取消</button><button class="btn primary" type="submit">保存资源</button></footer></form></section></div>`;
  $('#edit-title').focus();
}
function closeEditor() {
  $('#modal-root').innerHTML=''; state.editing=null;
  if(modalReturnFocus?.isConnected) modalReturnFocus.focus();
}
function saveEditor(event) {
  event.preventDefault();
  const title=$('#edit-title').value.trim(), summary=$('#edit-summary').value.trim(), body=$('#edit-body').value.trim();
  const selectedScopes=Array.from(document.querySelectorAll('input[name="scope"]:checked'),el=>el.value);
  if (!title||!summary||!body||!selectedScopes.length) { $('#editor-error').textContent='请填写名称、简述、正文，并至少选择一个关联资源。'; return; }
  const existing=resources.find(r=>r.id===state.editing);
  const resource={id:existing?.id||`resource-${Date.now()}`,type:state.newType,title,summary,body,scopes:selectedScopes,tasks:$('#edit-tasks').value.split(/[,，]/).map(t=>t.trim()).filter(Boolean),source:$('#edit-source').value.trim()||'手动添加',updated:'刚刚',status:existing?.status==='retired'?'retired':state.newType==='knowledge'?'proposed':'available'};
  if(existing) resources=resources.map(r=>r.id===existing.id?resource:r); else resources.unshift(resource);
  state.selected=resource.id; state.category='all'; state.scope='all'; state.search=''; state.tab='body'; $('#search').value='';
  closeEditor(); render();
  toast(resource.status==='proposed'?'已保存为待确认；请核对后确认业务知识。':'资源已保存，仅当前原型页面有效。');
}
function confirmDialog({title,body,label,onConfirm}) {
  modalReturnFocus=document.activeElement;
  $('#modal-root').innerHTML=`<div class="modal-mask" id="editor-mask"><section class="modal" style="width:430px" role="dialog" aria-modal="true" aria-labelledby="confirm-title"><header class="modal-head"><h2 id="confirm-title">${esc(title)}</h2><button class="icon-btn" id="close-editor" aria-label="关闭">${icon('close')}</button></header><div class="modal-body"><p style="font-size:12px;color:var(--secondary);line-height:1.9">${esc(body)}</p></div><footer class="modal-foot"><button class="btn" id="cancel-editor">取消</button><button class="btn primary" id="confirm-operation">${esc(label)}</button></footer></section></div>`;
  $('#confirm-operation').onclick=()=>{onConfirm();closeEditor();render();if(state.preview)renderPreview();};
  $('#cancel-editor').focus();
}
document.addEventListener('click', event => {
  const el=event.target.closest('button');
  if(event.target.id==='preview-mask'){closePreview();return;}
  if(event.target.id==='editor-mask'){closeEditor();return;}
  if(!el)return;
  if(el.dataset.category){state.category=el.dataset.category;state.tab='body';render();return;}
  if(el.dataset.scope){state.scope=el.dataset.scope;render();return;}
  if(el.dataset.resource){state.selected=el.dataset.resource;state.tab='body';renderList();renderDetail();return;}
  if(el.dataset.tab){state.tab=el.dataset.tab;renderDetail();return;}
  if(el.dataset.context){
    state.previewTask=contexts[el.dataset.context].task;
    state.active=el.dataset.context;renderContext();renderDetail();renderPreview();
    $(`[data-context="${state.active}"]`).focus();return;
  }
  if(el.dataset.read){
    if(state.readIds.has(el.dataset.read))state.readIds.delete(el.dataset.read);else state.readIds.add(el.dataset.read);
    $('#preview-result').innerHTML=renderTurn(state.turn);
    $(`[data-read="${el.dataset.read}"]`).focus();return;
  }
  if(el.dataset.type){
    state.newType=el.dataset.type;
    document.querySelectorAll('.type-option').forEach(b=>b.classList.toggle('active',b.dataset.type===state.newType));
    $('#editor-foot-note').textContent=state.newType==='knowledge'?'新知识保存为待确认，确认后才供 AI 使用。':'Skill 保存后可按上下文发现。';return;
  }
  if(el.dataset.nav){
    if(el.dataset.nav!=='library')toast('本稿聚焦“知识与技能”，其余主导航仅展示层级位置。');
    return;
  }
  const r=resources.find(r=>r.id===state.selected);
  if(el.dataset.action==='edit'){openEditor(state.selected);return;}
  if(el.dataset.action==='preview'){openPreview();return;}
  if(el.dataset.action==='confirm'){
    confirmDialog({title:'确认业务知识',body:`确认“${r.title}”的内容、来源和适用范围准确？确认后，它可在匹配的后续轮次中提供给 AI。`,label:'确认可用',onConfirm:()=>{r.status='confirmed';toast('知识已确认，将参与后续轮次的资源发现。');}});return;
  }
  if(el.dataset.action==='retire'){
    confirmDialog({title:'停用资源',body:`停用“${r.title}”后，后续轮次将不再发现它。原型保留内容，可在“已停用”中查看。`,label:'停用',onConfirm:()=>{r.status='retired';state.turn=null;state.readIds.clear();toast('资源已停用，不再进入可见目录。');}});return;
  }
  switch(el.id){
    case 'theme-toggle':{
      const dark=document.documentElement.dataset.theme!=='dark';
      document.documentElement.dataset.theme=dark?'dark':'light';
      el.dataset.icon=dark?'moon':'sun';el.innerHTML=icon(el.dataset.icon);break;
    }
    case 'new-btn':openEditor();break;
    case 'import-btn':$('#import-file').click();break;
    case 'preview-top':case 'open-preview':openPreview();break;
    case 'close-preview':closePreview();break;
    case 'close-editor':case 'cancel-editor':closeEditor();break;
    case 'reset-filter':case 'clear-search':state.scope='all';state.category='all';state.search='';$('#search').value='';render();break;
    case 'run-preview':
      state.previewTask=$('#preview-task').value;
      state.turn={context:state.active,task:state.previewTask,ids:matchResources(state.active,state.previewTask).map(r=>r.id)};
      state.readIds.clear();renderPreview();$('#run-preview').focus();break;
  }
});
$('#search').addEventListener('input',event=>{state.search=event.target.value;renderList();renderDetail();});
$('#mobile-category').addEventListener('change',event=>{state.category=event.target.value;state.tab='body';render();});
$('#mobile-scope').addEventListener('change',event=>{state.scope=event.target.value;render();});
document.addEventListener('input',event=>{if(event.target.id==='preview-task')state.previewTask=event.target.value;});
document.addEventListener('submit',event=>{if(event.target.id==='editor-form')saveEditor(event);});
$('#import-file').addEventListener('change',async event=>{
  const file=event.target.files[0];if(!file)return;
  event.target.value='';
  if(!/\.(md|txt)$/i.test(file.name)){toast('请选择 Markdown 或文本文件。');return;}
  if(file.size>64*1024){toast('原型导入限制为 64 KiB，请选择较小的说明文件。');return;}
  try{
    const body=await file.text();
    if(body.length>20000){toast('正文超过原型编辑上限 20,000 字符，请精简后导入。');return;}
    openEditor(null,{type:'skill',title:file.name.replace(/\.(md|txt)$/i,''),body});
    toast('文件仅在本地读取。请补充简述与适用范围后保存。');
  }catch{toast('文件读取失败，请重新选择。');}
});
document.addEventListener('keydown',event=>{
  if((event.metaKey||event.ctrlKey)&&event.key.toLowerCase()==='k'){
    event.preventDefault();if(!$('#modal-root').children.length&&!state.preview)$('#search').focus();return;
  }
  if(event.key==='Escape'){
    if($('#modal-root').children.length)closeEditor();else if(state.preview)closePreview();
  }
  // 弹框与侧栏的焦点不越过遮罩进入背景页面。
  if(event.key==='Tab'){
    const root=$('#modal-root .modal')||$('#sheet-root .sheet');
    if(!root)return;
    const focusable=Array.from(root.querySelectorAll('button:not(:disabled),input:not(:disabled),textarea,select,[tabindex="0"]')).filter(el=>el.getClientRects().length);
    const first=focusable[0],last=focusable.at(-1);
    if(event.shiftKey&&document.activeElement===first){event.preventDefault();last.focus();}
    else if(!event.shiftKey&&document.activeElement===last){event.preventDefault();first.focus();}
  }
});
hydrate();
render();

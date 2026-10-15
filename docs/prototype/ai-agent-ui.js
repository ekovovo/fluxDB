/* 独立 UI 原型：无 fetch/数据库/模型请求。localStorage 仅保存演示配置、会话和布局。 */
(() => {
  'use strict';
  const KEY = 'fluxdb-ai-platform-prototype-v2';
  const root = document.getElementById('fluxdb-prototype');
  const $ = id => document.getElementById(id);
  const esc = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  const icon = name => `<span class="icon" data-icon="${name}" aria-hidden="true"></span>`;
  const connections = [
    {id:'mysql',label:'Docker MySQL',kind:'mysql',database:'demo_shop'},
    {id:'postgres',label:'Docker PostgreSQL',kind:'postgres',database:'demo_analytics'},
    {id:'redis',label:'Docker Redis',kind:'redis',database:'DB 0'}
  ];
  const toolDefs = [
    ['connections.list','公共','列出已授权连接资源',false],['db.search_objects','SQL','渐进搜索表、视图与字段',false],
    ['db.describe_object','SQL','读取真实列、索引和关系',false],['db.query','SQL','单条有界只读查询，驱动绑定参数',false],
    ['db.explain','SQL','不执行式查询计划',false],['db.execute','SQL','修改专用入口，范围授权与审批',true],
    ['redis.scan','Redis','渐进扫描，不全量 KEYS',false],['redis.inspect','Redis','按类型有界预览值与 TTL',false],
    ['redis.execute','Redis','受控命令，检查所有 Key 与风险',true],['redis.server_info','Redis','受限版本与诊断摘要',false],
    ['result.read_page','公共','读取已取得的结果快照',false],['operation.status','公共','查询当前 caller 的操作状态',false],
    ['operation.cancel','公共','取消自身操作，不伪造服务端撤销',false]
  ].map(([id,group,description,write]) => ({id,name:id,group,description,write}));
  const plugins = [
    {id:'ui',name:'当前界面与 @ 上下文',icon:'text-select',description:'结构化快照，按需读细节，不全量发送界面。'},
    {id:'database',name:'数据库与 Redis 工具',icon:'database',description:'复用已配置连接，内外工具共用执行服务。'},
    {id:'files',name:'文件与附件',icon:'file-search',description:'仅在授权目录读取，有界处理大文件。'},
    {id:'router',name:'模型路由',icon:'workflow',description:'按能力、价格、隐私选择，用户固定模型优先。'},
    {id:'jev',name:'Jev 决策优化',icon:'activity',description:'配置合格模型后自动可用；故障不阻断主 Agent。'},
    {id:'compaction',name:'上下文压缩',icon:'list-filter',description:'保护用户约束，卸载大结果，保留来源和检查点。'},
    {id:'security',name:'调用前安全审计',icon:'circle-check',description:'最终参数与目标审计；不可关闭，错误时阻止执行。',mandatory:true},
    {id:'budget',name:'Token 用量与执行保护',icon:'calendar-clock',description:'统计会话 Token；缺 usage 明确未知，保留运行保护。',mandatory:true},
    {id:'knowledge',name:'业务知识与 Skills',icon:'folder',description:'按需加载已确认口径，不把模型提议当事实。'},
    {id:'bi',name:'BI 与报表输出',icon:'activity',description:'声明式图表与报告；数字回到查询来源。'},
    {id:'progress',name:'计划与执行进度',icon:'list',description:'公开计划、真实工具状态，不编造私有思考链。'},
    {id:'telemetry',name:'生命周期诊断',icon:'terminal-square',description:'脱敏耗时与错误信息；关闭不移除关键审计。'}
  ];
  const audits = [
    {id:'audit-001',tool:'db.query',origin:'内部 Agent',target:'MySQL / demo_shop',decision:'allow',rule:'read_scope · bounded_result',sql:'SELECT channel, SUM(amount - refund_amount) FROM orders WHERE status = ? GROUP BY channel;'},
    {id:'audit-002',tool:'db.execute',origin:'内部 Agent',target:'MySQL / demo_shop',decision:'approval',rule:'write_requires_user_approval',sql:"UPDATE orders SET status = 'reviewed' WHERE id = 1001;"},
    {id:'audit-003',tool:'redis.execute',origin:'Claude Code / MCP',target:'Redis / DB 0',decision:'deny',rule:'destructive_command · outside_grant',sql:'FLUSHALL'}
  ];
  const promptText = '你是 fluxDB 内部的数据 Agent。\n\n先获取必要的真实结构与业务口径，再选择工具行动。\n不捏造表名、字段、指标或数据。遇到错误，观察结果并调整。\n所有 SQL / Redis / 文件操作必须遵守当前目标与授权。\n报告中的数字必须能够追溯到结果集与查询。\n无 Jev 时独立完成任务；公开思考摘要不能替代执行证据。';
  function seed() {
    return {
      version:1,mode:'drawer',widths:{drawer:460,embedded:420},theme:'light',connectionId:'mysql',defaultModelId:'model-main',decisionModelId:'auto',selectedAgentId:'agent-general',lastSessionId:'session-approval',pendingDraft:'',contexts:[],toolGroup:'all',auditFilter:'all',agentTab:0,agentDraft:null,agentDirty:false,agentPageId:null,filters:{models:'',tools:''},
      models:[{id:'model-main',name:'主模型 · 高能力',kind:'generation',provider:'兼容 API',modelId:'main-model',tier:'strong',endpoint:'https://model.example.invalid/v1',contextWindow:400000,maxOutput:32000,priceInput:2.5,priceOutput:10,priceCached:0.25,keyConfigured:true,enabled:true},{id:'model-economy',name:'经济模型 · 摘要',kind:'generation',provider:'兼容 API',modelId:'economy-model',tier:'economy',endpoint:'https://model.example.invalid/v1',contextWindow:400000,maxOutput:16000,priceInput:0.15,priceOutput:0.6,priceCached:0.075,keyConfigured:true,enabled:true},{id:'model-jev',name:'Jev · 决策模型',kind:'decision',provider:'TypeSafe System One',modelId:'jev-demo',tier:'decision',endpoint:'https://decision.example.invalid/v1',contextWindow:32768,maxOutput:4096,priceInput:0,priceOutput:0,priceCached:0,keyConfigured:true,enabled:true}],
      agents:[{id:'agent-general',builtin:true,compressionPercent:80,name:'通用数据库 Agent',description:'跨 SQL、Redis 与文件自主探索、验证，并交付可追溯结果。',icon:'bot',modelId:'model-main',systemPrompt:promptText,policy:'approval',report:true,enabled:true},{id:'agent-bi',name:'BI 分析与报表',description:'结合已确认指标口径，生成图表与报告，明确数据限制。',icon:'activity',modelId:'model-main',systemPrompt:promptText,policy:'read',report:true,enabled:true},{id:'agent-redis',name:'Redis 排障助手',description:'渐进扫描、类型检查、TTL 分析，不默认开放危险管理命令。',icon:'database',modelId:'model-economy',systemPrompt:promptText,policy:'approval',report:false,enabled:true}],
      pluginEnabled:Object.fromEntries(plugins.map(p=>[p.id,true])),toolEnabled:Object.fromEntries(toolDefs.map(t=>[t.id,true])),
      mcp:{enabled:true,globalPolicy:'read_only',datasources:['mysql','postgres','redis'],datasourcePolicy:{mysql:'default',postgres:'read_write',redis:'read_only'},registered:{codex:true,claude:false},testing:false,lastTest:false},budget:{iterations:40,concurrency:2},
      sessions:[{id:'session-approval',title:'清理7天前未支付订单',agentId:'agent-general',targetId:'mysql',draft:'',status:'approval',kind:'write',tokens:1200,messages:['把超过7天仍是 pending 的订单标记为过期。'],stage:3,folds:{},contexts:['开发环境 / 连接','orders 表']},{id:'session-sales',title:'月度销售趋势分析',agentId:'agent-general',targetId:'mysql',draft:'',status:'done',kind:'analysis',tokens:1800,messages:['分析3–8月净销售额趋势，按支付月份汇总并生成可追溯报告。'],stage:3,folds:{},contexts:['开发环境 / 连接','orders 表','月度销售分析 / 查询']},{id:'session-redis',title:'缓存 TTL 排查',agentId:'agent-redis',targetId:'redis',draft:'',status:'done',kind:'redis',tokens:600,messages:['检查 demo:order:* 的类型和 TTL，找出没有过期时间的缓存。'],stage:3,folds:{},contexts:[]}]
    };
  }
  const reportSeries=[312000,385000,331000,425400,463100,521500];
  let stored = null;
  try { stored = JSON.parse(localStorage.getItem(KEY)); } catch (_) { /* 损坏原型状态不向用户泄漏错误载荷。 */ }
  const initial = seed();
  const state = stored?.version===1 ? {...initial,...stored} : initial;
  state.mode = ['drawer','embedded'].includes(state.mode) ? state.mode : 'drawer';
  state.widths = {drawer:460,embedded:420,...state.widths};
  for (const mode of ['drawer','embedded']) if (!Number.isFinite(state.widths[mode]) || state.widths[mode]<=0) state.widths[mode]=mode==='drawer'?460:420;
  if (!Array.isArray(state.sessions) || !Array.isArray(state.models)) Object.assign(state,seed());
  delete state.promptVersions;delete state.selectedPrompt;
  if(!state.agents.some(a=>a.id==='agent-general'))state.agents.unshift(initial.agents[0]);
  state.agents.sort((a,b)=>Number(b.id==='agent-general')-Number(a.id==='agent-general'));
  state.agents.forEach(a=>{a.builtin=a.id==='agent-general';a.compressionPercent??=80;a.systemPrompt??=promptText;delete a.promptVersion;a.temperature??=.2;a.maxOutputTokens??=4096;a.toolIds??=toolDefs.map(t=>t.id);a.pluginIds??=plugins.filter(p=>p.id!=='jev').map(p=>p.id);a.allowExport??=true;a.privacy??='redacted';a.fileRoots??='';a.limits??={iterations:40,wallSeconds:600,concurrency:2,outputBytes:131072,rows:200,fileBytes:262144};a.revision??=1;});
  state.sessions.forEach(s => { if(s.status==='running') s.status='interrupted'; });
  state.models.forEach(m=>{delete m.testing;if(m.priceInput==null)m.priceInput=0;if(m.priceOutput==null)m.priceOutput=0;if(m.priceCached==null)m.priceCached=0;});state.mcp.testing=false;
  // 旧版本原型状态没有分类/模式字段：补齐后再渲染，避免评审时读到半旧配置。
  state.mcp={enabled:true,globalPolicy:'read_only',datasources:[],datasourcePolicy:{},registered:{},...state.mcp};
  state.mcp.datasourcePolicy={...(state.mcp.datasourcePolicy||{})};
  state.mcp.registered={codex:true,claude:false,...(state.mcp.registered||{})};
  // 评审截图用：?theme=dark|light 覆盖本地保存的主题，不改动其它演示状态。
  const themeOverride = new URLSearchParams(location.search).get('theme');
  if(themeOverride==='dark'||themeOverride==='light')state.theme=themeOverride;
  let page = location.hash.replace(/^#/,'') || 'workbench';
  const initialModal=['report','budget'].includes(page)?page:null;if(initialModal)page='workbench';
  if (!(page in window.FluxPrototypePages)) page='workbench';
  if(page==='prompts')page='agents';
  let aiOpen = !stored && page==='workbench'; // 首次展示示例抽屉用于评审；重载相当于应用重启，面板不自动弹出。
  let activeMenu = null, modal = null, drag = null, timer = null, chartObserver = null;
  const taskTimers = new Map();
  const modelById = id => state.models.find(m=>m.id===id);
  const agentById = id => state.agents.find(a=>a.id===id);
  const connectionById = id => connections.find(c=>c.id===id);
  const session = () => state.sessions.find(s=>s.id===state.lastSessionId) || null;
  const currentDraft = () => session()?.draft ?? state.pendingDraft;
  function newAgentDraft(){return {id:'agent-new-'+Date.now(),name:'',description:'',builtin:false,compressionPercent:80,icon:'bot',modelId:state.defaultModelId,systemPrompt:promptText,policy:'approval',report:true,enabled:true,temperature:.2,maxOutputTokens:4096,toolIds:toolDefs.map(t=>t.id),pluginIds:plugins.filter(p=>p.id!=='jev').map(p=>p.id),allowExport:true,privacy:'redacted',fileRoots:'',limits:{iterations:40,wallSeconds:600,concurrency:2,outputBytes:131072,rows:200,fileBytes:262144},revision:0,isNew:true};}
  function agentDraft(){return state.agentDraft;}
  function draftContexts() {const c=session();return c ? (c.draftContexts ?? c.contexts ?? []) : state.contexts;}
  function updateDraftContexts(values) {const list=[...new Set(values)].slice(0,12);const c=session();if(c)c.draftContexts=list;else state.contexts=list;save();}
  function decisionCandidates() { return state.models.filter(m=>m.kind==='decision'&&m.enabled&&m.keyConfigured); }
  function jevAvailable() { const candidates=decisionCandidates();return !!state.pluginEnabled.jev && (state.decisionModelId==='auto'?candidates.length===1:candidates.some(m=>m.id===state.decisionModelId)); }
  function jevStatus() { if(!state.pluginEnabled.jev)return '已关闭';const c=decisionCandidates();if(!c.length)return '未配置 / 不可用';if(state.decisionModelId==='auto'&&c.length>1)return '待选择模型';return jevAvailable()?'自动可用':'模型不可用'; }
  function save() {
    // API Key 输入从来不是 state 字段；这里只保存演示状态。
    try {localStorage.setItem(KEY,JSON.stringify(state));} catch (_) {toast('本次已应用，但浏览器未能保存演示状态。');}
  }
  function icons(scope=root) {scope.querySelectorAll('[data-icon]').forEach(el=>{el.style.setProperty('--icon',`url("../../apps/fluxdb-desktop/assets/icons/${el.dataset.icon}.svg")`);});}
  function toast(text) {clearTimeout(timer);$('toast').textContent=text;$('toast').hidden=false;timer=setTimeout(()=>$('toast').hidden=true,2600);}
  function button(label,action,attrs='',cls='') {return `<button class="button ${cls}" data-action="${action}" ${attrs}>${label}</button>`;}
  function tag(text,color='') {return `<span class="tag ${color}">${esc(text)}</span>`;}
  function goto(next) {if(next==='report'){openReport();return;}
    if(next==='agents'&&!state.agentDraft&&state.agents.length){state.agentDraft=structuredClone(state.agents.find(a=>a.id===state.selectedAgentId)||state.agents[0]);state.agentDraft.isNew=false;state.agentPageId=state.agentDraft.id;}if(next==='budget'){openBudget();return;}if(!(next in window.FluxPrototypePages))return;page=next;activeMenu=null;location.hash=next;render();}
  function currentTitle() {return session()?.title || '新建会话';}
  function boundsWidth(mode=state.mode) {const maximum=Math.floor(root.clientWidth*(mode==='drawer'?.6:.4));return {min:Math.min(320,maximum),max:maximum};}
  function actualWidth(mode=state.mode) {const b=boundsWidth(mode);return Math.min(b.max,Math.max(b.min,state.widths[mode]));}
  function applyWidth() {$('ai-panel').style.setProperty('--ai-width',actualWidth()+'px');$('ai-panel').dataset.mode=state.mode;$('ai-mask').hidden=!aiOpen||state.mode==='embedded';}
  function sideNav() {
    const settings=['models','tools','mcp','plugins','audit'].includes(page);
    const managing=['agents','history'].includes(page);
    let html = '';
    if(settings) {
      html='<div class="sidebar-heading">设置</div>'+[['models','bot','大模型'],['tools','plug','工具与 MCP']].map(([p,i,l])=>`<button class="nav-item ${page===p||(p==='tools'&&['tools','mcp','plugins','audit'].includes(page))?'active':''}" data-action="goto" data-page="${p}">${icon(i)}${l}</button>`).join('')+'<div class="sidebar-caption">应用设置</div>'+['编辑器','快捷键','外观','系统','数据','连接安全','数据库支持','关于'].map(t=>`<button class="nav-item" disabled>${icon('settings')}${t}</button>`).join('');
    } else if(managing) {
      html='<div class="sidebar-heading">AI Agent</div>'+[['agents','bot','Agent 配置'],['history','inbox','会话历史']].map(([p,i,l])=>`<button class="nav-item ${page===p?'active':''}" data-action="goto" data-page="${p}">${icon(i)}${l}</button>`).join('')+'<div class="sidebar-caption">配置与能力</div>'+[['models','bot','大模型'],['tools','plug','工具与 MCP']].map(([p,i,l])=>`<button class="nav-item" data-action="goto" data-page="${p}">${icon(i)}${l}</button>`).join('');
    } else {
      html='<div class="sidebar-heading">连接'+button(icon('plus'),'demo-note','data-text="连接在实际项目中已配置；这里展示示例连接。" aria-label="新增连接"','ghost icon-only')+'</div><div class="search">'+icon('search')+'<input aria-label="搜索连接" placeholder="搜索对象…" disabled></div><div style="height:12px"></div>'+connections.map(c=>`<button class="nav-item ${c.id===state.connectionId?'active':''}" data-action="connection" data-id="${c.id}"><img class="tree-db-icon" src="../../apps/fluxdb-desktop/assets/db/${c.kind}.svg" alt="">${c.label}${c.id===state.connectionId?'<span class="status-dot green"></span>':''}</button>${c.id===state.connectionId?`<button class="nav-item sub" data-action="demo-note" data-text="示例资源：${esc(c.database)}">${icon('database')}${c.database}</button>${c.kind==='redis'?'<div class="nav-item sub muted">demo:order:*</div>':`<button class="nav-item sub active" data-action="workbench">${icon('table-2')} orders</button><div class="nav-item sub muted">${icon('table-2')} order_items</div><div class="nav-item sub muted">${icon('table-2')} customers</div>`}`:''}`).join('');
    }
    html+='<div class="sidebar-bottom">'+button(icon('home')+'数据库工作台','goto','data-page="workbench"','ghost')+`<button class="nav-item" data-action="goto" data-page="agents">${icon('bot')}AI Agent 管理</button>`+'</div>';
    $('sidebar').innerHTML=html;
  }
  function captureView() {const el=document.activeElement;return {id:el?.id,start:el?.selectionStart,end:el?.selectionEnd,scroll:$('ai-panel').querySelector('.ai-messages')?.scrollTop};}
  function restoreView(v) {if(v?.id){const el=$(v.id);if(el){el.focus({preventScroll:true});if(typeof v.start==='number'&&el.setSelectionRange)try{el.setSelectionRange(v.start,v.end);}catch(_){}}}const list=$('ai-panel').querySelector('.ai-messages');if(list&&typeof v.scroll==='number')list.scrollTop=v.scroll;}
  function render() {
    const focus=captureView();
    document.documentElement.dataset.theme=state.theme;
    sideNav();$('view').innerHTML=window.FluxPrototypePages[page](state);
    $('header-path').textContent={workbench:'数据库工作台',models:'设置 / 大模型',tools:'设置 / 工具与 MCP',mcp:'设置 / 本地 MCP',plugins:'设置 / 内置插件',audit:'设置 / 安全审计',agents:'AI Agent / 管理',budget:'AI Agent / 执行保护',history:'AI Agent / 会话',report:'AI Agent / 报表'}[page];
    $('status-target').textContent=connectionById(state.connectionId).label+' · '+connectionById(state.connectionId).database;
    renderPanel();renderSceneMenu();icons();drawChart();restoreView(focus);
  }
  const SCENES=[['workbench','数据库工作台'],['models','大模型配置'],['agents','Agent 配置'],['plugins','内置插件 / Jev'],['tools','工具目录与定义'],['mcp','本地 MCP Server'],['audit','调用前安全审计'],['history','会话历史'],['report','报告弹框与来源'],['budget','执行保护']];
  function renderSceneMenu() {$('scene-menu').hidden=activeMenu!=='scenes';$('scene-menu').innerHTML='<div class="menu-caption">完整 AI Agent 交互范围</div>'+SCENES.map(([p,l])=>`<button class="menu-item ${p===page?'active':''}" data-action="goto" data-page="${p}">${icon('chevron-right')}${l}</button>`).join('');}
  function toolCard(c,id,name,description,detail,status='done') {
    const color=status==='denied'?'red':status==='approval'?'amber':status==='running'?'blue':'green';
    const label=status==='denied'?'已拒绝':status==='approval'?'待批准':status==='running'?'执行中':'完成';
    return `<details class="tool-card" data-fold="${id}" data-fold-owner="${c.turnKey||c.id}" ${c.folds[id]?'open':''}><summary>${icon('chevron-right')}<span class="tool-name mono">${name}</span><span class="muted">${description}</span>${tag(label,color)}</summary><div class="tool-detail"><pre>${esc(detail)}</pre><div class="audit-line ${status==='denied'?'denied':status==='approval'?'warning':''}">${icon('circle-check')}安全审计 · ${status==='denied'?'禁止危险命令':status==='approval'?'写入需要确认':'授权范围内，参数与目标已核对'} · 模拟</div></div></details>`;
  }
  function messages(c) {
    if(!c) return `<div class="ai-empty"><div class="empty-symbol">${icon('wand-sparkles')}</div><h2>从一个问题开始</h2><p>基于当前连接与界面上下文，自主探索、执行、验证。写操作会先审计。</p>${[['解释当前 SQL','解释当前 SQL，并检查性能问题。'],['分析数据并生成报表','按渠道分析净销售额变化，并生成报表。'],['检查缓存与 TTL','扫描 demo:order:*，检查类型与 TTL。']].map(([l,t])=>`<button class="suggestion" data-action="suggest" data-text="${t}">${icon('chevron-right')}${l}</button>`).join('')}</div>`;
    let html=(c.previousTurns||[]).map(old=>messages({...old,previousTurns:[],isHistory:true})).join('');
    html+=c.messages.map((m,i)=>`<div class="user-label">${icon('users')}你${i===c.messages.length-1?'<span class="muted">· 当前任务</span>':''}</div><div class="user-bubble">${esc(m)}${i===0?`<div class="message-context">${(c.contexts||[]).map(t=>tag('@ '+t)).join('')}</div>`:''}</div>`).join('');
    html+=`<div class="assistant-label">${icon('bot')}fluxDB Agent<span class="time">${c.status==='running'?'执行中':'示例 · 3.2s'}</span></div>`;
    if(c.status==='paused')return html+'<div class="budget-notice">任务达到运行保护上限，未发起下一次调用。'+button('查看执行保护','goto','data-page="budget"','ghost')+'</div>';
    if(c.status==='interrupted')return html+'<div class="notice">'+icon('info')+'上次任务已中断。打开面板不会重放工具或未知写操作。</div>'+(c.isHistory?'':button('继续模拟任务','resume-demo',`data-id="${c.id}"`));
    html+=`<details class="reasoning" data-fold="reasoning" data-fold-owner="${c.turnKey||c.id}" ${c.folds.reasoning?'open':''}><summary>${icon('chevron-right')}公开思考摘要 <span>· ${c.status==='running'?'进行中':'已完成'}</span></summary><p>先确认真实结构和净销售额口径，再执行受限汇总。结果解释差异，并不直接证明因果；需要进一步检验订单量与客单价。此处是公开摘要示例，不是所有模型的私有完整思考链。</p></details>`;
    if(c.kind==='denied')return html+toolCard(c,'blocked',c.blockedTool||'redis.execute','未执行',c.blockedReason||'FLUSHALL\n规则：destructive_command / outside_grant','denied')+'<div class="answer">请求未执行。请收窄目标或启用允许的读取工具；可选决策模型不能覆盖内核拒绝。</div>';
    if(c.kind==='write') {
      html+=toolCard(c,'write','db.execute','orders / 1001',"UPDATE orders SET status = 'reviewed' WHERE id = 1001;",c.status==='approval'?'approval':c.status==='denied'?'denied':c.status==='running'?'running':'done');
      if(c.status==='approval')html+=approvalCard(c);
      else html+=`<div class="answer">${c.status==='denied'?'本次修改已拒绝，未执行。':c.status==='running'?'正在核实执行状态…':'修改完成，影响 1 行（模拟）。真实执行回执不能被普通 Hook 改成成功。'}</div>`;
      return html;
    }
    if(c.kind==='redis')return html+toolCard(c,'scan','redis.scan','demo:order:*','SCAN · prefix demo:order:*\n返回 3 个示例 Key · cursor 0')+toolCard(c,'inspect','redis.inspect','类型 / TTL','demo:order:1002 → string\nTTL -1 → 没有过期时间\n值预览 412 bytes')+'<div class="answer">示例中 <code>demo:order:1002</code> 没有过期时间。建议先确认它是否应长期保留，再申请更新 TTL；本次没有修改数据。</div>';
    const stage=c.stage??3;
    html+=executionTrace(c,stage);
    if(c.status==='running')return html+'<p class="answer muted">正在观察真实工具回执与修正结果（模拟）…</p>';
    html+='<div class="answer">8月净销售额为 <strong>¥52.15万</strong>，较7月增加 <strong>12.6%</strong>；3–8月累计 <strong>¥243.80万</strong>。图中6个点均为示例完整聚合，不是截断明细。</div>';
    if(state.pluginEnabled.bi)html+=reportCard(true);
    return html;
  }
  function approvalCard(c) {
    const conn=connectionById(c.targetId)||connections[0];
    return `<div class="approval-card" role="group" aria-label="等待批准的写入操作"><div class="approval-head"><span class="inline">${icon('triangle-alert')}<strong>需要你批准这次写入</strong></span>${tag('风险：高','amber')}</div><div class="approval-context"><div><span>连接环境</span><strong>${esc(conn.label)}（${conn.kind}）</strong>${tag('非生产','green')}</div><div><span>数据库</span><strong>demo_shop / public</strong></div><div><span>目标连接配置</span><strong class="mono">conn_test rev 4</strong><small>连接配置已变更将使批准失效</small></div></div><div class="code-box approval-sql"><pre>UPDATE public.orders
SET expires_flag = 1
WHERE status = 'pending'
  AND created_at &lt; (NOW() - INTERVAL 7 DAY)
  AND expires_flag = 0</pre></div><div class="approval-impact"><div><span>预计影响行数</span><strong>142 行</strong><small>先 SELECT COUNT 核实</small></div><div><span>依据</span><strong>内核实测 + 事务内再校验</strong></div></div><div class="approval-checks"><div>${icon('check')}<span>仅本条 UPDATE，不含 DDL、不含其他表</span></div><div>${icon('check')}<span>必须带 WHERE，事务内复核命中行数 ≤ 500</span></div><label>${icon('square')}<span>本次授权在 10 分钟内对同一 plan hash 有效</span><input type="checkbox" data-change="approval-scope" aria-label="10分钟内同一plan免重复审批"></label><div>${icon('check')}<span>执行后回显受影响行数与提交结果</span></div></div><div class="approval-hint">按 Esc 或点击遮罩 = 保留等待（等同拒绝本次），不会视为批准。写操作不支持自动重放。</div><div class="approval-actions">${button(icon('circle-x')+'拒绝','resolve-approval',`data-id="${c.id}" data-decision="deny"`,'danger')}${button(icon('check')+'批准一次','resolve-approval',`data-id="${c.id}" data-decision="approve"`,'primary')}</div></div>`;
  }
  function executionTrace(c,stage) {
    const fixed=!!c.fixedModelId,sqlState=c.targetId==='postgres'?'42703':'42S22';
    const leaf=(status,iconName,title,badges,note,detail)=>`<div class="trace-node ${status}"><span class="trace-mark"></span><div class="trace-content"><div class="trace-title">${icon(iconName)}<span>${title}</span>${badges}</div>${note?`<p class="trace-note">${note}</p>`:''}${detail?`<details class="trace-detail"><summary>${icon('file-code')}参数与结果摘要</summary><pre>${esc(detail)}</pre></details>`:''}</div></div>`;
    let nodes=leaf(stage===0&&c.status==='running'?'running':'done','workflow','模型路由',tag(fixed?'用户固定 → 本次模型':'用户未固定 → 自动','blue'), '上下文示例按选中模型重算：128k − 4k输出 − 12k工具/安全余量 = 112k可用，本次装配12.5k。');
    nodes+=leaf(stage>=1?'done':'running','search','db.search_objects',tag('只读'),'','目标：授权开发环境 / orders\n仅返回对象摘要，不加载整库。');
    if(stage>=1){nodes+=leaf('error','triangle-alert','db.query 失败 · 保留原回执',tag('SQLSTATE '+sqlState,'red'),'首次用了不存在的 net_amount；错误作为下一轮观察输入，不篡改失败记录。','原始列：net_amount\n回执：undefined / unknown column');nodes+=leaf(stage>=2?'done':'running','table-2','db.describe_object',tag('已核对字段'), '获取实际 amount / refund_amount / paid_at，再形成修正查询。');}
    if(stage>=2)nodes+=leaf(stage>=3?'done':'running','table-2','db.query · 已修正',tag('ResultRef')+tag('6行聚合')+tag('未截断'),'','绑定参数：[paid, 2026-03-01, 2026-09-01]\n返回：month / net_revenue\nResultRef：result_sales_9f2@1');
    return `<details class="execution-group" data-fold="execution" data-fold-owner="${c.turnKey||c.id}" ${(c.folds.execution??c.status==='running')?'open':''}><summary><span class="inline">${icon('chevron-right')}执行过程 · ${stage>=3?'5步':'进行中'}</span><span class="trace-total">${stage>=3?'6.8s · 4次工具 · 1次纠正':'模拟运行'} · ${c.tokens||0} tokens</span></summary><div class="trace-nodes">${nodes}</div></details>`;
  }
  function reportCard(inChat) {
    const head=inChat?`<div class="artifact-header"><div class="inline">${icon('activity')}<strong>销售趋势报告 · 3–8月</strong></div>${tag('fluxdb.bi.report')}</div>`:'';
    return `<article class="chat-report ${inChat?'in-chat':'in-dialog'}">${head}<div class="artifact-body"><div class="artifact-metrics"><div><span>8月净销售额</span><strong>¥52.15万</strong><small>环比 +12.6%</small></div><div><span>6个月累计</span><strong>¥243.80万</strong><small>对比口径已确认</small></div></div><div class="artifact-chart"><div class="between"><h3>月度净销售额（CNY）</h3><span>6个聚合点 · 完整</span></div><div class="monthly-trend" role="img" aria-label="示例月度净销售额：3月31.2万，4月38.5万，5月33.1万，6月42.54万，7月46.31万，8月52.15万"></div><p class="chart-definition">口径：支付月份，扣除退款；不含运费与税费。图表只引用已查询字段。</p></div><div class="artifact-provenance"><p>来源结果集 <code>result_sales_9f2@1</code> · 6行 · 本轮快照，不随数据库变化</p><p>工具引用 <code>call_db_query_7c21</code> · 完整聚合，未截断</p><p>指标口径 <code>metric_net_revenue@rev4</code> · Asia/Shanghai · status='paid'</p></div>${inChat?`<div class="artifact-actions">${button(icon('file-code')+'SQL','source-sql')}${button(icon('folder-up')+'导出','export-report')}${button(icon('file-search')+'打开报告','open-report')}</div>`:''}</div></article>`;
  }
  function renderPanel() {
    const pane=$('ai-panel');pane.hidden=!aiOpen;applyWidth();
    const active=state.sessions.some(c=>['running','approval'].includes(c.status));$('entry-status').hidden=!active;
    if(!aiOpen)return;
    const c=session();const target=connectionById(c?.targetId||state.connectionId);const a=agentById(c?.agentId||state.selectedAgentId)||state.agents[0];const model=modelById(c?.fixedModelId||a?.modelId)||modelById(state.defaultModelId);
    pane.setAttribute('role',state.mode==='drawer'?'dialog':'region');pane.setAttribute('aria-modal',state.mode==='drawer'?'true':'false');
    const mismatch=c&&c.targetId!==state.connectionId;
    pane.innerHTML=`<div class="resize-handle" id="panel-resize" role="separator" aria-label="调整AI面板宽度" aria-orientation="vertical"></div><div class="ai-header"><div class="ai-title">询问 AI<div class="ai-subtitle">${esc(currentTitle())}</div></div><span class="token-usage">${c?.tokens||0} tokens</span>${button(icon('plus')+'<span class="button-text">新建</span>','new-chat','id="ai-new" aria-label="新建会话"','ghost')}${button(icon('inbox'),'show-history','aria-label="会话历史"','ghost icon-only')}${button(icon('panel-right'),'mode-menu','id="mode-toggle" aria-label="切换展现形式"','ghost icon-only')}${button(icon('x'),'close-ai','id="ai-close" aria-label="关闭询问AI"','ghost icon-only')}</div><div class="ai-scope">${icon('database')}${tag(target?.label+' / '+target?.database)}${tag(a?.policy==='read'?'只读范围':'修改需审批','green')}${mismatch?'<span class="scope-label">会话目标与当前页面不同，未自动切库</span>':''}</div><div class="ai-messages">${messages(c)}</div><div class="ai-composer">${draftContexts().length?`<div class="composer-references">${draftContexts().map((text,i)=>`<span class="reference-chip">${icon(i===0?'database':i===1?'table-2':'file-code')}<span>${esc(text)}</span><button data-action="remove-context" data-index="${i}" aria-label="移除${esc(text)}">${icon('x')}</button></span>`).join('')}</div>`:''}<div class="composer-box"><textarea id="ai-input" aria-label="询问AI输入" maxlength="2000" placeholder="描述目标；用 @ 引用连接、表、结果集或文件">${esc(currentDraft())}</textarea><div class="composer-tools">${button('@','context-menu','aria-label="引用上下文"','ghost')}${button(icon('folder-input'),'attach-file','aria-label="添加附件"','ghost icon-only')}<span class="scope-label">${draftContexts().length?draftContexts().length+'项引用':'当前页面'}</span>${button(c?.status==='running'?icon('square')+'停止':icon('arrow-up')+'发送',c?.status==='running'?'stop-task':'send','id="ai-send"','submit primary')}</div></div><div class="composer-bottom"><span class="routing-indicator">${icon('workflow')}自动路由</span><button data-action="agent-menu">${esc(a?.name)} ${icon('chevron-down')}</button><span>·</span><button data-action="model-menu">${esc(model?.name||'未选模型')}</button><span>Jev：${esc(jevStatus())}</span></div></div>${activeMenu==='mode'?`<div class="popover mode-popover" role="menu"><div class="menu-caption">展现形式 · 下次打开沿用</div>${[['drawer','抽屉 · 最大 60%'],['embedded','内嵌 · 最大 40%']].map(([id,text])=>`<button class="menu-item ${state.mode===id?'active':''}" data-action="set-mode" data-mode="${id}" role="menuitemradio" aria-checked="${state.mode===id}">${icon('panel-right')}${text}${state.mode===id?'<span class="trailing">'+icon('check')+'</span>':''}</button>`).join('')}</div>`:''}${['agent','model'].includes(activeMenu)?`<div class="popover model-popover"><div class="menu-caption">${activeMenu==='agent'?'选择 Agent':'固定本会话模型'}</div>${(activeMenu==='agent'?state.agents:state.models.filter(m=>m.kind==='generation')).map(v=>`<button class="menu-item" data-action="choose-${activeMenu}" data-id="${v.id}">${icon('bot')}${esc(v.name)}</button>`).join('')}${button('管理配置','goto',`data-page="${activeMenu==='agent'?'agents':'models'}"`,'ghost')}</div>`:''}<input id="file-input" type="file" hidden accept=".csv,.sql,.txt,.json">`;
    icons(pane);
  }
  function openAi() {aiOpen=true;activeMenu=null;renderPanel();icons();setTimeout(()=>$('ai-input')?.focus({preventScroll:true}),20);}
  function closeAi() {aiOpen=false;activeMenu=null;save();renderPanel();$('ask-ai').focus({preventScroll:true});}
  function doNewChat() {const c=session();if(!c&&state.pendingDraft.trim()){showDialog('放弃未发送的新草稿？','<p class="muted">当前内容尚未发送。关闭面板会保留它；新建将清空这份草稿。</p>',button('取消','close-modal')+button('放弃并新建','discard-draft','','primary'),'small');return;}state.lastSessionId=null;state.pendingDraft='';state.contexts=[];save();aiOpen=true;activeMenu=null;renderPanel();icons();$('ai-input')?.focus();}
  function startMock(c) {
    c.status='running';c.stage=0;save();renderPanel();icons();
    const old=taskTimers.get(c.id);if(old)clearInterval(old);
    const t=setInterval(()=>{c.stage++;if(c.stage>=3){c.status='done';c.tokens=(c.tokens||0)+600;clearInterval(t);taskTimers.delete(c.id);}save();if(session()?.id===c.id&&aiOpen){const focus=captureView();renderPanel();restoreView(focus);}else renderPanel();icons();},650);
    taskTimers.set(c.id,t);
  }
  function send() {
    if(session()&&['running','approval'].includes(session().status))return toast('当前任务尚在运行或等待批准。请停止、处理批准，或新建会话。');
    if(state.sessions.filter(c=>['running','approval'].includes(c.status)).length>=Number(state.budget.concurrency))return toast('活动任务已达到上限，请等待或停止已有任务。');
    const text=currentDraft().trim();if(!text)return toast('先描述一个目标，或选择示例问题。');
    const a=agentById(session()?.agentId||state.selectedAgentId)||state.agents[0];const m=modelById(session()?.fixedModelId||a?.modelId||state.defaultModelId);
    if(!a?.enabled||!m?.enabled||!m?.keyConfigured)return toast('当前 Agent 的主模型不可用，请先在大模型配置中启用并配置。');
    let c=session();
    if(!c){c={id:'session-'+Date.now(),title:text.slice(0,16),agentId:a.id,targetId:state.connectionId,draft:'',status:'idle',kind:'analysis',tokens:0,messages:[],stage:0,folds:{},contexts:[...draftContexts()]};state.sessions.unshift(c);state.lastSessionId=c.id;state.pendingDraft='';}
    if(c.messages.length){const previous={...c,messages:[...c.messages],folds:{...c.folds},previousTurns:[]};c.previousTurns=[...(c.previousTurns||[]),previous].slice(-5);}
    c.messages=[text];c.draft='';c.folds={};c.turnKey='turn-'+Date.now();c.runSnapshot={agentId:a.id,agentRevision:a.revision,systemPrompt:a.systemPrompt,modelId:m.id,permissions:a.policy,limits:{...a.limits}};c.blockedTool=null;c.blockedReason=null;
    if(/FLUSH|清空所有|删除全部缓存/i.test(text)){c.kind='denied';c.status='done';c.stage=3;c.blockedTool='redis.execute';c.blockedReason='FLUSHALL\n规则：destructive_command / outside_grant';}
    else if(/UPDATE|修改|更新订单|设为|设成|删除订单/i.test(text)){c.kind=a.policy==='read'?'denied':'write';c.status=a.policy==='read'?'done':'approval';c.stage=3;c.blockedTool='db.execute';c.blockedReason='只读Agent范围不允许修改，不能通过批准按钮扩大权限。';}
    else if(c.targetId==='redis'||/TTL|缓存|Redis/i.test(text)){c.kind='redis';c.status='done';c.stage=3;c.tokens=(c.tokens||0)+300;}
    else if(state.toolEnabled['db.query']===false||!a.toolIds.includes('db.query')){c.kind='denied';c.status='done';c.blockedTool='db.query';c.blockedReason='工具未启用，不执行查询。';toast('db.query 当前已关闭，模拟任务不会继续查询。');}
    else {c.kind='analysis';startMock(c);return;}
    save();renderPanel();icons();
  }
  function showDialog(title,body,footer,extra='') {
    modal=title;$('modal-root').innerHTML=`<div class="scrim" data-action="close-modal"><section class="dialog ${extra}" role="dialog" aria-modal="true" aria-label="${esc(title)}"><div class="dialog-head"><h2>${esc(title)}</h2>${button(icon('x'),'close-modal','aria-label="关闭弹框"','ghost icon-only')}</div><div class="dialog-body">${body}<div id="dialog-error" class="dialog-error" role="alert"></div></div><div class="dialog-footer">${footer}</div></section></div>`;icons($('modal-root'));setTimeout(()=>$('modal-root').querySelector('input:not([type=radio]),textarea,select,button')?.focus(),20);
  }
  function closeDialog() {modal=null;$('modal-root').innerHTML='';if(aiOpen){icons();drawChart();}}
  function editModel(id) {
    const m=modelById(id)||{id:'new',name:'',provider:'openai',modelId:'',endpoint:'',contextWindow:null,maxOutput:null,priceInput:0,priceOutput:0,priceCached:0};
    const capacity=(field,label,value)=>`<div class="field"><label for="${field}">${label}（tokens）</label><div class="token-capacity-input"><input id="${field}" type="text" inputmode="decimal" value="${value==null?'':value/1000}" placeholder="未提供，可手动填写"><select data-capacity-unit="${field}" aria-label="${label}单位"><option>K</option><option>M</option></select></div></div>`;
    showDialog(id==='new'?'新建模型':'编辑模型',`<div class="form-grid">
      <div class="field"><label for="edit-model-name">显示名称</label><input id="edit-model-name" value="${esc(m.name)}"></div>
      <div class="field"><label for="edit-model-provider">Provider</label><select id="edit-model-provider">${['openai','openai-responses','anthropic','decisions'].map(p=>`<option ${m.provider===p?'selected':''}>${p}</option>`).join('')}</select></div>
      <div class="field wide"><label for="edit-model-endpoint">Endpoint</label><input id="edit-model-endpoint" value="${esc(m.endpoint)}" placeholder="https://…/v1"></div>
      <div class="field wide"><label for="edit-api-key">API Key</label><div class="token-capacity-input"><input id="edit-api-key" type="password" value="${m.keyConfigured?'prototype-key-not-real':''}"><button class="button ghost" data-action="reveal-prototype-key" aria-label="显示或隐藏密钥">${icon('eye')}</button></div></div>
      <div class="field wide"><label for="edit-model-id">模型 ID</label><div class="token-capacity-input"><input id="edit-model-id" value="${esc(m.modelId)}"><button class="button ghost" data-action="fetch-prototype-capacity">获取模型列表</button></div></div>
      ${capacity('edit-ctx','上下文窗口',m.contextWindow)}${capacity('edit-maxout','最大输出',m.maxOutput)}
      <div class="model-price-row wide">${[['edit-price-in','输入',m.priceInput],['edit-price-out','输出',m.priceOutput],['edit-price-cache','缓存读取',m.priceCached]].map(([field,label,value])=>`<div class="field"><label for="${field}">计价 · ${label} $/1M</label><input id="${field}" type="number" min="0" step="0.01" value="${value??0}"></div>`).join('')}</div></div>`,button('取消','close-modal')+button('保存模型','save-model',`data-id="${id}"`,'primary'));
    document.querySelectorAll('[data-capacity-unit]').forEach(select=>{select.dataset.previous='K';select.addEventListener('change',()=>{const input=$(select.dataset.capacityUnit),value=Number(input.value)*(select.dataset.previous==='M'?1000000:1000);if(input.value)input.value=String(value/(select.value==='M'?1000000:1000));select.dataset.previous=select.value;});});
  }
  function saveModel(id) {
    const name=$('edit-model-name').value.trim(),modelId=$('edit-model-id').value.trim(),endpoint=$('edit-model-endpoint').value.trim();
    if(!name||!modelId)return $('dialog-error').textContent='请填写显示名称和模型 ID。';
    let url;try{url=new URL(endpoint);if(!['http:','https:'].includes(url.protocol)||url.username||url.password||/api_?key|token|password/i.test(url.search))throw Error();}catch(_){return $('dialog-error').textContent='Endpoint 必须是无嵌入凭据的 HTTP(S) URL。';}
    const keyInput=$('edit-api-key').value.trim();const typed=keyInput&&!keyInput.includes('•');
    const ctx=$('edit-ctx').value.trim()?Number($('edit-ctx').value)*(document.querySelector('[data-capacity-unit="edit-ctx"]').value==='M'?1000000:1000):null,maxout=$('edit-maxout').value.trim()?Number($('edit-maxout').value)*(document.querySelector('[data-capacity-unit="edit-maxout"]').value==='M'?1000000:1000):null,pIn=Number($('edit-price-in').value),pOut=Number($('edit-price-out').value),pCache=Number($('edit-price-cache').value);
    if(![pIn,pOut,pCache].every(v=>Number.isFinite(v)&&v>=0)||[ctx,maxout].some(v=>v!=null&&(!Number.isInteger(v)||v<1||v>1e9))||(ctx!=null&&maxout!=null&&maxout>ctx))return $('dialog-error').textContent='容量需为正整数tokens，输出不能超过窗口。';
    const previous=modelById(id);const kind=$('edit-model-provider').value==='decisions'?'decision':'generation';
    const m={id:previous?.id||'model-'+Date.now(),name,modelId,endpoint,kind,provider:$('edit-model-provider').value,tier:kind==='decision'?'decision':'strong',contextWindow:ctx,maxOutput:maxout,priceInput:pIn,priceOutput:pOut,priceCached:pCache,enabled:previous?.enabled??true,keyConfigured:typed?true:!!previous?.keyConfigured};
    // 不读取/存入密钥内容，仅由模式决定模拟凭据状态。
if(previous)state.models.splice(state.models.indexOf(previous),1,m);else state.models.push(m);
    save();closeDialog();render();toast('模型配置已模拟保存；未保存实际 Key。');
  }
  function collectAgentDraft(){
    const d=state.agentDraft;if(!d)return null;
    d.name=$('agent-name').value;d.description=$('agent-description').value;
    d.modelId=$('agent-model').value;d.systemPrompt=$('agent-system-prompt').value;
    d.compressionPercent=Number($('agent-compression').value);
    d.toolIds=[...$('view').querySelectorAll('[data-agent-tool]:checked')].map(e=>e.dataset.agentTool);
    d.pluginIds=[...$('view').querySelectorAll('[data-agent-plugin]:checked')].map(e=>e.dataset.agentPlugin);
    d.report=$('agent-report').checked;
    d.enabled=$('agent-enabled').checked;
    return d;
  }
  function validateAgentDraft(d){
    if(!d.name.trim())return '请填写名称。';
    if(new TextEncoder().encode(d.systemPrompt).length>65536)return '提示词不能超过64KiB UTF-8。';
    if(!Number.isInteger(d.compressionPercent)||d.compressionPercent<10||d.compressionPercent>95)return '压缩触发比例必须在10–95%之间。';
    if(d.builtin&&!d.enabled)return '内置助手不能停用。';
    return '';
  }
  function saveAgent(id) {
    const d=collectAgentDraft();if(!d)return;
    const error=validateAgentDraft(d);if(error)return toast(error);
    const previous=agentById(d.id);
    const saved={...d,isNew:false,revision:(previous?.revision||0)+1};
    if(previous)state.agents.splice(state.agents.indexOf(previous),1,saved);else state.agents.push(saved);
    state.agentDraft=null;state.agentDirty=false;state.agentPageId=saved.id;save();goto('agents');toast('Agent与直接提示词已保存，修改仅影响下一任务。');
  }
  function selectAgent(id){
    collectAgentDraft();
    if(state.agentDirty&&!confirm('当前 Agent 有未保存修改，切换将丢失。继续？'))return;
    const a=agentById(id);if(!a)return;
    state.agentDraft=structuredClone(a);state.agentDraft.isNew=false;state.agentPageId=id;state.agentDirty=false;state.agentTab=state.agentTab||0;save();render();
  }
  function newAgent(){
    collectAgentDraft();
    if(state.agentDirty&&!confirm('当前 Agent 有未保存修改，切换将丢失。继续？'))return;
    state.agentDraft=newAgentDraft();state.agentPageId=state.agentDraft.id;state.agentDirty=false;state.agentTab=0;save();goto('agents');
  }
  function openReport() {
    activeMenu=null;
    showDialog('销售趋势报告 · 3–8月',window.FluxPrototypePages.report(state),button(icon('file-code')+'SQL','source-sql')+button(icon('folder-up')+'导出报告','export-report')+button('关闭','close-modal'),'report-dialog');
    drawChart();
  }
  function openBudget() {
    activeMenu=null;
    showDialog('执行保护 · 高级',window.FluxPrototypePages.budget(state),button('取消','close-modal')+button('保存设置','save-budget','','primary'),'budget-dialog');
  }
  function download(name,text,type='text/plain;charset=utf-8') {const a=document.createElement('a');a.href=URL.createObjectURL(new Blob([text],{type}));a.download=name;a.click();setTimeout(()=>URL.revokeObjectURL(a.href),200);}
  function drawChart() {
    chartObserver?.disconnect();const hosts=[...document.querySelectorAll('.monthly-trend')];if(!hosts.length)return;
    const draw=host=>{const w=Math.max(160,host.clientWidth),h=Math.max(165,host.clientHeight),l=w<330?34:45,r=13,t=18,b=26,pw=w-l-r,ph=h-t-b,max=600000;let svg=`<svg viewBox="0 0 ${w} ${h}" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">`;
      for(const v of [200000,400000,600000]){const y=t+ph*(1-v/max);svg+=`<line x1="${l}" y1="${y}" x2="${w-r}" y2="${y}" stroke="var(--line)" stroke-dasharray="4 5"/><text x="${l-7}" y="${y+4}" text-anchor="end" fill="var(--muted)" font-size="11">${v/10000}万</text>`;}
      const points=reportSeries.map((v,i)=>[l+pw*i/5,t+ph*(1-v/max)]);svg+=`<path d="${points.map(([x,y],i)=>(i?'L':'M')+x+','+y).join(' ')}" fill="none" stroke="var(--ok)" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/>`;
      points.forEach(([x,y],i)=>{if(i===5)svg+=`<circle cx="${x}" cy="${y}" r="3.5" fill="var(--ok)"/>`;svg+=`<text x="${x}" y="${h-7}" text-anchor="${i===5?'end':'middle'}" fill="var(--muted)" font-size="11">${i+3}月</text>`;});host.innerHTML=svg+'</svg>';};
    hosts.forEach(draw);chartObserver=new ResizeObserver(entries=>entries.forEach(e=>draw(e.target)));hosts.forEach(h=>chartObserver.observe(h));
  }
  function changeMode(mode) {const focus=captureView();state.mode=mode;activeMenu=null;save();renderPanel();icons();restoreView(focus);}
  function eventAction(e) {
    const el=e.target.closest('[data-action]');if(!el)return;
    if(el.classList.contains('scrim')&&e.target!==el)return;
    const action=el.dataset.action,id=el.dataset.id;
    if(action==='goto'){if(el.dataset.page!=='workbench')aiOpen=false;goto(el.dataset.page);return;}
    if(action==='connection'){state.connectionId=id;save();goto('workbench');return;}
    if(action==='workbench'){goto('workbench');return;}
    if(action==='open-ai'){openAi();return;}
    if(action==='close-ai'){closeAi();return;}
    if(action==='new-chat'){doNewChat();return;}
    if(action==='discard-draft'){closeDialog();state.pendingDraft='';doNewChat();return;}
    if(action==='send'){send();return;}if(action==='confirm-task-target'){const target=$('turn-target').value;closeDialog();send(target);return;}
    if(action==='suggest'){aiOpen=true;const c=session();if(c)c.draft=el.dataset.text;else state.pendingDraft=el.dataset.text;save();renderPanel();icons();$('ai-input')?.focus();return;}
    if(action==='stop-task'){const c=session();if(c){clearInterval(taskTimers.get(c.id));taskTimers.delete(c.id);c.status='interrupted';save();renderPanel();icons();toast('已停止后续模拟步骤。');}return;}
    if(action==='resume-demo'){const c=state.sessions.find(c=>c.id===id);if(c)startMock(c);return;}
    if(action==='mode-menu'||action==='agent-menu'||action==='model-menu'||action==='scene-menu'){const name={'mode-menu':'mode','agent-menu':'agent','model-menu':'model','scene-menu':'scenes'}[action];activeMenu=activeMenu===name?null:name;renderPanel();renderSceneMenu();icons();return;}
    if(action==='set-mode'){changeMode(el.dataset.mode);return;}
    if(action==='choose-agent'){state.selectedAgentId=id;activeMenu=null;doNewChat();save();return;}
    if(action==='choose-model'){const c=session();if(c)c.fixedModelId=id;else state.defaultModelId=id;activeMenu=null;save();renderPanel();icons();return;}
    if(action==='show-history'){aiOpen=false;goto('history');return;}
    if(action==='select-session'){state.lastSessionId=id;save();goto('workbench');openAi();return;}
    if(action==='open-report'){openReport();return;}if(action==='open-budget'){openBudget();return;}
    if(action==='theme'){state.theme=state.theme==='light'?'dark':'light';save();render();return;}
    if(action==='close-modal'){closeDialog();return;}
    if(action==='edit-model'){editModel(id);return;}
    if(action==='toggle-key'){const inp=$('edit-api-key');if(!inp)return;const show=inp.type==='password';inp.type=show?'text':'password';const btn=$('modal-root').querySelector('.secret-eye');btn.innerHTML=icon(show?'eye-off':'eye');btn.setAttribute('aria-label',show?'隐藏 API Key':'显示 API Key');return;}
    if(action==='reveal-prototype-key'){const input=$('edit-api-key');input.type=input.type==='password'?'text':'password';return;}
    if(action==='fetch-prototype-capacity'){if(!$('edit-api-key').value.trim())return toast('请先填写API Key');$('edit-model-id').value='prototype-model';if(!$('edit-ctx').value)$('edit-ctx').value=document.querySelector('[data-capacity-unit="edit-ctx"]').value==='M'?'0.128':'128';if(!$('edit-maxout').value)$('edit-maxout').value=document.querySelector('[data-capacity-unit="edit-maxout"]').value==='M'?'0.008':'8';toast('模拟返回模型容量；此原型不发真实请求');return;}
    if(action==='save-model'){saveModel(id);return;}
    if(action==='delete-agent'){const a=agentById(id);if(!a||a.builtin)return;if(!confirm('删除助手配置？历史会话和审计保留。'))return;state.agents=state.agents.filter(a=>a.id!==id);state.agentDraft=null;state.agentPageId=null;state.selectedAgentId='agent-general';save();goto('agents');toast('助手配置已删除（模拟）');return;}
    if(action==='save-agent'){saveAgent(id);return;}
    if(action==='select-agent'){selectAgent(id);return;}
    if(action==='new-agent'){newAgent();return;}
    if(action==='copy-agent'){collectAgentDraft();const src=agentById(id);if(!src)return;state.agentDraft={...structuredClone(src),id:'agent-new-'+Date.now(),name:src.name+' 副本',builtin:false,revision:0,isNew:true};state.agentPageId=state.agentDraft.id;state.agentDirty=true;goto('agents');toast('已复制为新草稿，保存后生效。');return;}
    if(action==='agent-editor-tab'){collectAgentDraft();const tab=el.dataset.tab;state.agentTab=tab;$('view').querySelectorAll('[data-agent-panel]').forEach(p=>p.hidden=p.dataset.agentPanel!==tab);$('view').querySelectorAll('.agent-tabs [role=tab]').forEach(b=>{const yes=b.dataset.tab===tab;b.classList.toggle('active',yes);b.setAttribute('aria-selected',String(yes));});return;}
    if(action==='test-model'){const m=modelById(id);m.testing=true;render();setTimeout(()=>{m.testing=false;render();toast(m.enabled&&m.keyConfigured?'示例连接测试通过（没有发起网络请求）。':'模型停用或凭据缺失，示例测试未通过。');},700);return;}
    if(action==='tool-detail'){const t=toolDefs.find(t=>t.id===id);showDialog(t.name,`<div class="inline">${tag(t.group)}${tag(t.write?'修改需授权':'读取 / 有界',t.write?'amber':'green')}</div><p class="page-subtitle">${esc(t.description)}</p><div class="code-box" style="margin-top:16px"><pre>${esc(JSON.stringify({tool_id:t.id,version:1,target:'授权资源句柄',input:'严格schema + 业务校验',limits:{timeout_ms:5000,output_bytes:65536},before_execute:'强制安全审计',outcome:'succeeded / denied / unknown'},null,2))}</pre></div>`,button('关闭','close-modal'),'small');return;}
    if(action==='audit-detail'){const a=audits.find(a=>a.id===id);showDialog('安全审计详情',`<div class="inline">${tag(a.tool)}${tag(a.decision==='deny'?'拒绝':a.decision==='allow'?'允许':'需审批',a.decision==='deny'?'red':a.decision==='allow'?'green':'amber')}</div><div class="field" style="margin-top:17px"><label>最终计划 · ${a.target}</label><div class="code-box"><pre>${esc(a.sql)}</pre></div><small>规则 ${esc(a.rule)} · plan hash 示例 · 审计不会包含秘密</small></div>`,button('关闭','close-modal'),'small');return;}
    if(action==='resolve-approval'){const c=state.sessions.find(c=>c.id===id);if(c){c.status=el.dataset.decision==='approve'?'done':'denied';save();renderPanel();icons();toast(c.status==='done'?'本次模拟修改已批准，影响1行。':'本次模拟修改已拒绝，数据不变。');}return;}
    if(action==='save-budget'){const b={iterations:Number($('iteration-budget').value),concurrency:Number($('concurrency-budget').value)};if(![b.iterations,b.concurrency].every(v=>Number.isInteger(v)&&v>0))return toast('运行保护上限必须是正整数。');state.budget=b;save();closeDialog();toast('执行保护已模拟保存；会话只统计 Token。');return;}
    if(action==='export-mcp'){download('fluxdb-local.example.json',JSON.stringify({mcpServers:{fluxdb_local:{command:'/path/to/fluxdb-mcp',args:['--transport','stdio','--server-profile','fluxdb.local','--config-root','/path/to/storage']}}},null,2),'application/json');toast('已导出不含秘密的示例配置，路径需要按实际安装位置生成。');return;}
    if(action==='toggle-mcp-client'){state.mcp.registered[id]=!state.mcp.registered[id];save();render();toast(state.mcp.registered[id]?'已把 fluxdb 写入示例客户端配置（模拟，未写磁盘）。':'已移除本应用写入的 server 条目，其它 server 不受影响。');return;}
    if(action==='mcp-datasource-add'){const next=U().connections.find(c=>!state.mcp.datasources.includes(c.id));if(!next)return toast('没有可加入的连接。');state.mcp.datasources=[...state.mcp.datasources,next.id];state.mcp.datasourcePolicy[next.id]='default';save();render();toast(`已加入数据源 ${next.label}（默认跟随全局）。`);return;}
    if(action==='mcp-datasource-remove'){state.mcp.datasources=state.mcp.datasources.filter(v=>v!==id);delete state.mcp.datasourcePolicy[id];save();render();toast('已从对外服务移除该数据源。');return;}
    if(action==='test-mcp'){if(!state.mcp.enabled||!state.mcp.datasources.length)return toast('请先启用示例Server并加入至少一个数据源。');state.mcp.testing=true;render();setTimeout(()=>{state.mcp.testing=false;state.mcp.lastTest=state.mcp.enabled;save();render();toast(state.mcp.enabled?'模拟握手与查询通过；未运行Claude或实际Server。':'Server已关闭，模拟测试停止。');},800);return;}
    if(action==='source-sql'){if(modal==='销售趋势报告 · 3–8月'){const area=$('report-sql');area.hidden=!area.hidden;return;}showDialog('报告来源SQL',`<div class="code-box"><pre>SELECT DATE_FORMAT(paid_at, '%Y-%m') AS month,\n       SUM(amount-refund_amount) AS net_revenue\nFROM orders WHERE status=? AND paid_at&gt;=? AND paid_at&lt;?\nGROUP BY month ORDER BY month;\n\nResultRef：result_sales_9f2@1 · 6行完整聚合</pre></div>`,button('关闭','close-modal'));return;}
    if(action==='export-report'){const a=agentById(session()?.agentId||state.selectedAgentId);if(a?.allowExport===false)return toast('此Agent不允许导出产物。');download('monthly-sales.example.md','# 销售趋势报告（模拟）\n\n8月净销售额 ¥52.15万，环比+12.6%；3–8月累计 ¥243.80万。\n\n口径：支付月份，实付扣除退款，不含运费与税费。\n来源：result_sales_9f2@1 / call_db_query_7c21 / metric_net_revenue@rev4。\n限制：仅界面示例，不来自实际数据库。\n');toast('已导出示例报告，包含口径、快照和来源。');return;}
    if(action==='rename-session'){const c=state.sessions.find(c=>c.id===id);showDialog('重命名会话',`<div class="field"><label for="rename-title">会话名称</label><input id="rename-title" value="${esc(c.title)}"></div>`,button('取消','close-modal')+button('保存','save-session-title',`data-id="${id}"`,'primary'),'small');return;}
    if(action==='save-session-title'){const c=state.sessions.find(c=>c.id===id);c.title=$('rename-title').value.trim()||c.title;save();closeDialog();render();return;}
    if(action==='context-menu'){showDialog('引用当前上下文',`<p class="page-subtitle">只加入引用与摘要，Agent需要时再读取内容。</p>${['当前 SQL','orders 表','当前结果集','已确认净销售额口径'].map(c=>button('@ '+c,'add-context',`data-text="${c}"`,'ghost')).join('')}`,button('关闭','close-modal'),'small');return;}
    if(action==='add-context'){updateDraftContexts([...draftContexts(),el.dataset.text]);closeDialog();renderPanel();icons();return;}if(action==='remove-context'){updateDraftContexts(draftContexts().filter((_,i)=>i!==Number(el.dataset.index)));renderPanel();icons();return;}
    if(action==='attach-file'){$('file-input')?.click();return;}
    if(action==='run-sql'||action==='work-stop'||action==='demo-note'){toast(el.dataset.text||'交互稿只演示状态，不执行实际数据库操作。');return;}
  }
  root.addEventListener('click',e=>{const summary=e.target.closest('summary');const d=summary?.parentElement;if(d?.dataset.fold&&session()){const current=session();const owner=current.turnKey===d.dataset.foldOwner?current:(current.previousTurns||[]).find(t=>t.turnKey===d.dataset.foldOwner);if(owner){owner.folds[d.dataset.fold]=!d.open;save();}}eventAction(e);});
  root.addEventListener('input',e=>{
    const el=e.target;
    if(el.id==='ai-input'){const c=session();if(c)c.draft=el.value;else state.pendingDraft=el.value;save();}
    if(el.dataset.filter){state.filters[el.dataset.filter]=el.value;render();}
    if(el.closest('.agent-detail')){collectAgentDraft();state.agentDirty=true;const note=$('agent-dirty');if(note)note.textContent='有未保存修改';}
  });
  root.addEventListener('change',e=>{
    const el=e.target,kind=el.dataset.change,id=el.dataset.id;

    if(el.id==='file-input'&&el.files[0]){updateDraftContexts([...draftContexts(),'文件 '+el.files[0].name]);renderPanel();icons();toast('已加入文件名引用；原型不读取实际文件内容。');return;}
    if(!kind)return;
    if(kind==='connection')state.connectionId=el.value;
    if(kind==='model-enabled')modelById(id).enabled=el.checked;
    if(kind==='plugin-enabled'){if(plugins.find(p=>p.id===id)?.mandatory)return;state.pluginEnabled[id]=el.checked;}
    if(kind==='tool-enabled')state.toolEnabled[id]=el.checked;
    if(kind==='default-model')state.defaultModelId=el.value;
    if(kind==='default-decision')state.decisionModelId=el.value;
    if(kind==='tool-group')state.toolGroup=el.value;
    if(kind==='audit-filter')state.auditFilter=el.value;
    if(kind==='mcp-enabled')state.mcp.enabled=el.checked;
    if(kind==='mcp-global-policy')state.mcp.globalPolicy=el.value;
    if(kind==='mcp-datasource-policy')state.mcp.datasourcePolicy[id]=el.value;
    save();render();
  });

  root.addEventListener('keydown',e=>{if(e.target.id==='ai-input'&&e.key==='Enter'&&(e.metaKey||e.ctrlKey)){e.preventDefault();send();}});
  document.addEventListener('keydown',e=>{if(e.key==='Tab'){const surface=modal?$('modal-root').querySelector('.dialog'):(aiOpen&&state.mode==='drawer'?$('ai-panel'):null);if(surface){const items=[...surface.querySelectorAll('button,input,select,textarea,summary,a[href]')].filter(x=>!x.disabled&&x.offsetParent!==null);const first=items[0],last=items[items.length-1];if(first&&(e.shiftKey&&(document.activeElement===first||!surface.contains(document.activeElement)))){e.preventDefault();last.focus();}else if(last&&!e.shiftKey&&(document.activeElement===last||!surface.contains(document.activeElement))){e.preventDefault();first.focus();}}return;}if(e.key!=='Escape')return;if(modal){closeDialog();return;}if(activeMenu){activeMenu=null;renderPanel();renderSceneMenu();icons();return;}if(aiOpen)closeAi();});
  document.addEventListener('click',e=>{if(activeMenu&&!e.target.closest('.popover')&&!e.target.closest('[data-action$="-menu"]')){activeMenu=null;renderPanel();renderSceneMenu();icons();}});
  root.addEventListener('pointerdown',e=>{if(e.target.id!=='panel-resize')return;drag={x:e.clientX,width:actualWidth(),mode:state.mode};e.target.classList.add('dragging');e.preventDefault();});
  document.addEventListener('pointermove',e=>{if(!drag)return;const b=boundsWidth(drag.mode);const desired=drag.width+drag.x-e.clientX;state.widths[drag.mode]=Math.min(b.max,Math.max(b.min,desired));applyWidth();});
  document.addEventListener('pointerup',()=>{if(!drag)return;drag=null;$('panel-resize')?.classList.remove('dragging');save();});
  window.addEventListener('resize',()=>{applyWidth();});
  window.addEventListener('hashchange',()=>{const next=location.hash.slice(1);if(next==='report'){openReport();return;}if(next==='budget'){openBudget();return;}if(next in window.FluxPrototypePages&&next!==page){page=next;render();}});
  window.FluxPrototype={esc,icon,button,state,connections,tools:toolDefs,plugins,audits,modelById,agentById,connectionById,agentDraft,connections,jevAvailable,jevStatus,actualWidth,boundsWidth,save,render,goto,openAi,closeAi,reportCard,storageKey:KEY};
  render();if(initialModal==='report')openReport();if(initialModal==='budget')openBudget();
})();

/* Standalone prototype. All records are fictional and stay in this page's memory. */
const $ = selector => document.querySelector(selector);
const esc = value => String(value).replace(/[&<>"']/g, char => ({
  '&':'&amp;', '<':'&lt;', '>':'&gt;', '"':'&quot;', "'":'&#39;'
}[char]));
const translations = {
  zh: {
    prototype:'MongoDB · 交互设计稿',designNote:'原型说明',theme:'切换明暗主题',toggleSidebar:'切换连接侧栏',
    connections:'连接',newGroup:'新建分组（原型未接入）',newConnection:'添加连接（原型未接入）',
    refresh:'刷新示例',searchCollections:'搜索集合',connectionTree:'连接树',connectionTabs:'连接页签',objectTabs:'对象页签',
    collection:'集合',demoOnly:'只读示例',localOnly:'沿用 fluxDB 工作区 · 无真实连接',
    collectionViews:'集合视图',documents:'文档',aggregation:'聚合',indexes:'索引',collectionInfo:'集合信息',
    options:'选项',find:'查找',reset:'重置查询',displayMode:'显示方式',documentView:'文档视图',tableView:'表格视图',
    table:'表格',insert:'插入文档',insertUnavailable:'仅布局示意，不提供数据库写入',detail:'详情',
    documentDetail:'文档详情',closeDetail:'关闭详情',preserveBson:'保留 BSON 类型',notAiContext:'仅本地预览，文档值不发送给 AI。',
    perPage:'每页',previous:'上一页',next:'下一页',sampleData:'虚构示例数据',noConnection:'未连接数据库',
    close:'关闭',contextBoundary:'AI 上下文边界',queryBoundary:'交互范围',gotIt:'知道了',
    designBody:'外壳沿用 fluxDB 的连接树、双层标签和紧凑工具栏。集合内部参考 Compass 的文档、查询与索引组织方式，不复制其品牌皮肤，也不使用 Redis 的 TTL、键类型或扫描统计。',
    contextBody:'本原型没有 AI 或数据库接线，不读取真实连接、密码或业务数据，也不上传内容。后续生产实现需单独确认页面 AI 上下文范围；文档值只能通过有界只读工具按需读取。',
    queryBody:'支持本地示例的字段等值、$eq、$gte、$in，以及顶层字段 Projection、Sort、Limit。其他操作符明确报错。聚合是固定示例管道；索引为示例，不创建索引或执行写入。',
    records:'条文档',sampleSuffix:'当前示例',range:'显示',of:'共',noResults:'没有匹配的示例文档',resetAndShow:'重置并显示全部',
    openDetail:'在侧栏查看文档',collapseDoc:'折叠文档',expandDoc:'展开文档',fieldCount:'个字段',items:'项',
    queryInvalid:'查询参数无效。请使用 JSON 对象；Filter 仅支持标量等值或 $eq / $gte / $in。',
    projectInvalid:'Projection 仅支持顶层字段的 0 / 1，不能混用包含与排除（_id 除外）。',
    sortInvalid:'Sort 仅支持顶层标量字段，方向必须为 1 或 -1。',limitInvalid:'Limit 必须是 1 到 100 的整数。',
    refreshed:'已重新载入本地示例，未请求数据库。',copied:'已复制所选文档的 Extended JSON。',
    copyFailed:'浏览器未允许复制，请在 JSON 视图中选择文本。',copyDoc:'复制 Extended JSON',
    filtersApplied:'已应用示例查询',indexesNote:'仅展示索引布局与键定义；不包含实际大小、使用次数或服务端状态。',
    indexName:'索引名称',indexKeys:'键定义',properties:'属性',unique:'唯一',regular:'普通',createIndex:'创建索引',
    aggregationTitle:'聚合管道',aggregationSubtitle:'以当前集合示例数据演示分组统计，不执行 MongoDB 命令。',
    stage:'阶段',stageMatch:'筛选记录',stageGroup:'分组计数',preview:'示例结果',pipelineNote:'固定示例管道 · 不受文档页 Filter 影响',
    general:'基本信息',dataShape:'文档与类型',database:'数据库',name:'集合名称',type:'类型',readOnly:'只读示例',
    count:'示例文档数',fields:'文档字段',bson:'类型保留',bsonValue:'ObjectId / Date / Decimal128',
    schema:'模式',schemaValue:'非固定结构；字段按文档展示',infoNote:'这里只显示可证实的本地示例信息，不伪造服务端版本、存储大小或精确总量。',
    expandConnection:'展开或收起连接',expandDatabase:'展开或收起数据库',allExpanded:'全部文档已展开',
    missing:'缺失',pipelineCount:'count',pipelineGroup:'status',otherConnection:'未接入本原型',
  },
  en: {
    prototype:'MongoDB · Interactive prototype',designNote:'Design notes',theme:'Toggle light / dark',toggleSidebar:'Toggle connections',
    connections:'Connections',newGroup:'New group (not connected)',newConnection:'Add connection (not connected)',
    refresh:'Reload sample',searchCollections:'Search collections',connectionTree:'Connection tree',connectionTabs:'Connection tabs',objectTabs:'Object tabs',
    collection:'Collection',demoOnly:'Read-only sample',localOnly:'fluxDB workspace · No live connection',
    collectionViews:'Collection views',documents:'Documents',aggregation:'Aggregation',indexes:'Indexes',collectionInfo:'Collection info',
    options:'Options',find:'Find',reset:'Reset query',displayMode:'Display mode',documentView:'Document view',tableView:'Table view',
    table:'Table',insert:'Insert document',insertUnavailable:'Layout only; database writes are not connected',detail:'Details',
    documentDetail:'Document details',closeDetail:'Close details',preserveBson:'BSON types preserved',notAiContext:'Local preview only. No document values are sent to AI.',
    perPage:'Per page',previous:'Previous page',next:'Next page',sampleData:'Fictional sample data',noConnection:'No database connection',
    close:'Close',contextBoundary:'AI context boundary',queryBoundary:'Prototype interactions',gotIt:'Got it',
    designBody:'The connection tree, nested tabs and compact toolbars follow fluxDB. Collection views take inspiration from Compass without adopting its brand styling or Redis key types, TTL or scan statistics.',
    contextBody:'This prototype has no AI or database wiring. It reads no real connections, credentials or business data, and uploads nothing. Production AI context scope requires separate confirmation; document values must be read on demand through bounded read-only tools.',
    queryBody:'Local samples support scalar equality, $eq, $gte and $in, plus top-level Projection, Sort and Limit. Unsupported operators are rejected. Aggregation uses a fixed example pipeline. No index creation or writes are performed.',
    records:'documents',sampleSuffix:'local sample',range:'Showing',of:'of',noResults:'No matching sample documents',resetAndShow:'Reset and show all',
    openDetail:'View document in side panel',collapseDoc:'Collapse document',expandDoc:'Expand document',fieldCount:'fields',items:'items',
    queryInvalid:'Invalid query. Use a JSON object; Filter supports scalar equality, $eq, $gte and $in only.',
    projectInvalid:'Projection supports top-level 0 / 1 only. Do not mix include and exclude, except _id.',
    sortInvalid:'Sort supports top-level scalar fields only. Directions must be 1 or -1.',limitInvalid:'Limit must be an integer from 1 to 100.',
    refreshed:'Local sample reloaded. No database request was made.',copied:'Selected document copied as Extended JSON.',
    copyFailed:'Clipboard access was denied. Select the text in JSON view instead.',copyDoc:'Copy Extended JSON',
    filtersApplied:'Sample query applied',indexesNote:'Example index layout and key definitions only. No actual size, usage count or server state is reported.',
    indexName:'Index name',indexKeys:'Keys',properties:'Properties',unique:'Unique',regular:'Regular',createIndex:'Create index',
    aggregationTitle:'Aggregation pipeline',aggregationSubtitle:'Group the current sample collection. No MongoDB command is executed.',
    stage:'Stage',stageMatch:'Match records',stageGroup:'Group and count',preview:'Sample results',pipelineNote:'Fixed example pipeline · Independent of the Documents filter',
    general:'General',dataShape:'Documents and types',database:'Database',name:'Collection name',type:'Type',readOnly:'Read-only sample',
    count:'Sample document count',fields:'Document fields',bson:'Type preservation',bsonValue:'ObjectId / Date / Decimal128',
    schema:'Schema',schemaValue:'Flexible; fields shown per document',infoNote:'Only verifiable local sample metadata is displayed, without fabricated server versions, storage sizes or totals.',
    expandConnection:'Expand or collapse connection',expandDatabase:'Expand or collapse database',allExpanded:'All documents expanded',
    missing:'Missing',pipelineCount:'count',pipelineGroup:'status',otherConnection:'Not connected in this prototype',
  }
};
let lang = 'zh';
const t = key => translations[lang][key];
const iconAliases = {'shield-check':'circle-check','lock-keyhole':'eye-off','copy':'copy-plus'};
function icon(name) {
  if (name === 'sun' || name === 'moon') {
    const path = name === 'sun'
      ? '<circle cx="12" cy="12" r="4"/><path d="M12 2v2m0 16v2M2 12h2m16 0h2M5 5l1.5 1.5m11 11L19 19M5 19l1.5-1.5m11-11L19 5"/>'
      : '<path d="M20.9 13A9 9 0 0 1 11 3.1 9 9 0 1 0 20.9 13Z"/>';
    return `<span data-icon="${name}"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${path}</svg></span>`;
  }
  return `<span data-icon="${name}"><img src="../../apps/fluxdb-desktop/assets/icons/${iconAliases[name] || name}.svg" alt="" aria-hidden="true"></span>`;
}
function hydrate(root = document) {
  root.querySelectorAll('[data-icon]:empty').forEach(el => {
    el.outerHTML = icon(el.dataset.icon);
  });
  root.querySelectorAll('[data-i18n]').forEach(el => { el.textContent = t(el.dataset.i18n); });
  root.querySelectorAll('[data-label]').forEach(el => {
    el.setAttribute('aria-label', t(el.dataset.label));
    el.title = t(el.dataset.label);
  });
  root.querySelectorAll('[data-placeholder]').forEach(el => { el.placeholder = t(el.dataset.placeholder); });
}

const oid = value => ({$oid:`67170000000000000000${String(value).padStart(4,'0')}`});
const date = value => ({$date:`2026-10-${value}T08:30:00.000Z`});
const orders = [
  {order_no:'SO-20261022-0089',status:'paid',channel:'online',customer:{name:'演示客户 036',level:'enterprise',city:'杭州'},amount:{$numberDecimal:'1286.00'},items:[{sku:'SKU-GEA-001',quantity:2},{sku:'SKU-TEA-002',quantity:1}],created_at:date('22')},
  {order_no:'SO-20261022-0090',status:'pending',channel:'offline',customer:{name:'演示客户 037',level:'basic',city:'上海'},amount:{$numberDecimal:'458.00'},items:[{sku:'SKU-TR-002',quantity:1}],created_at:date('22')},
  {order_no:'SO-20261021-0087',status:'paid',channel:'online',customer:{name:'演示客户 038',level:'enterprise',city:'深圳'},amount:{$numberDecimal:'965.00'},items:[{sku:'SKU-GEA-003',quantity:3}],created_at:date('21')},
  {order_no:'SO-20261021-0088',status:'refunded',channel:'offline',customer:{name:'演示客户 039',level:'basic',city:'北京'},amount:{$numberDecimal:'730.00'},items:[],created_at:date('21'),remark:'演示退款订单'},
  {order_no:'SO-20261020-0085',status:'paid',channel:'online',customer:{name:'演示客户 040',level:'basic',city:'广州'},amount:{$numberDecimal:'214.00'},items:[{sku:'SKU-EDU-001',quantity:1}],created_at:date('20')},
  {order_no:'SO-20261020-0086',status:'pending',channel:'online',customer:{name:'演示客户 041',level:'basic',city:'成都'},amount:{$numberDecimal:'320.00'},items:[{sku:'SKU-GEA-001',quantity:1}],created_at:date('20')},
  {order_no:'SO-20261019-0083',status:'paid',channel:'offline',customer:{name:'演示客户 042',level:'enterprise',city:'苏州'},amount:{$numberDecimal:'2400.00'},items:[{sku:'SKU-GEA-002',quantity:4}],created_at:date('19')},
  {order_no:'SO-20261019-0084',status:'paid',channel:'online',customer:{name:'演示客户 043',level:'basic',city:'南京'},amount:{$numberDecimal:'89.00'},items:[{sku:'SKU-COF-001',quantity:1}],created_at:date('19'),remark:null}
].map((doc,index) => ({_id:oid(index + 1),...doc}));
const customers = orders.slice(0,5).map((order,index) => ({
  _id:oid(index+101),name:order.customer.name,level:order.customer.level,
  city:order.customer.city,active:index!==3,tags:index%2===0?['demo','enterprise']:['demo'],
  created_at:date('18')
}));
const products = [
  {_id:oid(201),sku:'SKU-GEA-001',name:'演示手冲壶',stock:24,price:{$numberDecimal:'199.00'},status:'active'},
  {_id:oid(202),sku:'SKU-TEA-002',name:'演示茶礼盒',stock:86,price:{$numberDecimal:'168.00'},status:'active'},
  {_id:oid(203),sku:'SKU-COF-001',name:'演示咖啡豆',stock:0,price:{$numberDecimal:'89.00'},status:'inactive'}
];
const collections = {
  orders:{docs:orders,unique:'order_no',secondary:'status'},
  customers:{docs:customers,unique:null,secondary:'city'},
  products:{docs:products,unique:'sku',secondary:'stock'}
};
let state = {
  collection:'orders',tab:'documents',view:'documents',page:0,pageSize:3,selected:orders[0],
  results:orders,project:{},inspector:false,connectionOpen:true,databaseOpen:true,
  collapsed:new Set(),busy:false
};
const current = () => collections[state.collection];
const docId = doc => doc?._id?.$oid || '';
const isScalar = value => value===null || ['string','number','boolean'].includes(typeof value);
const scalarComparable = value => isScalar(value) || (value && ('$oid' in value || '$date' in value));
const comparable = value => value?.$oid ?? value?.$date ?? value;
function bsonType(value) {
  if (value===undefined) return t('missing');
  if (value===null) return 'Null';
  if (Array.isArray(value)) return 'Array';
  if (typeof value==='object') return '$oid' in value?'ObjectId':'$date' in value?'Date':'$numberDecimal' in value?'Decimal128':'Object';
  return typeof value==='string'?'String':typeof value==='boolean'?'Boolean':Number.isInteger(value)?'Int32':'Double';
}
function valueLabel(value) {
  const type = bsonType(value);
  if(type==='ObjectId') return `ObjectId("${value.$oid}")`;
  if(type==='Date') return `ISODate("${value.$date}")`;
  if(type==='Decimal128') return `Decimal128("${value.$numberDecimal}")`;
  return JSON.stringify(value);
}
function renderField(key,value,depth=0) {
  const type=bsonType(value);
  const nested=type==='Object'||type==='Array';
  const label=`<span class="field-key">${esc(key)}</span><span class="field-colon">:</span>`;
  if(nested) {
    const count=Object.keys(value).length;
    return `<details class="nested" ${key==='customer'&&depth===0?'open':''}><summary>${label}<span class="nested-size">${type==='Array'?'[':'{'} ${count} ${t(type==='Array'?'items':'fieldCount')} ${type==='Array'?']':'}'}</span><span class="field-type">${type}</span></summary><div class="nested-children">${Object.entries(value).map(([k,v])=>renderField(k,v,depth+1)).join('')}</div></details>`;
  }
  const kind={String:'string',Int32:'number',Double:'number',Boolean:'boolean',Decimal128:'number',ObjectId:'id',Date:'date',Null:'null'}[type]||'';
  return `<div class="field-row">${label}<span class="field-value ${kind}">${esc(valueLabel(value))}</span><span class="field-type">${type}</span></div>`;
}
function jsonMarkup(value) {
  // Tokenize before inserting HTML so syntax spans never match one another.
  const raw=JSON.stringify(value,null,2);
  const token=/"(?:\\.|[^"\\])*"(?=\s*:)|"(?:\\.|[^"\\])*"|-?\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?\b|\b(?:true|false|null)\b/g;
  let output='',last=0;
  for (const match of raw.matchAll(token)) {
    output+=esc(raw.slice(last,match.index));
    const kind=match[0][0]==='"'?(raw.slice(match.index+match[0].length).trimStart().startsWith(':')?'key':'str'):/^(true|false|null)$/.test(match[0])?'literal':'num';
    output+=`<span class="${kind}">${esc(match[0])}</span>`;
    last=match.index+match[0].length;
  }
  output+=esc(raw.slice(last));
  return `<div class="json-code">${output.split('\n').map((line,index)=>`<div class="json-line"><span class="line-number">${index+1}</span><span class="line-content">${line||' '}</span></div>`).join('')}</div>`;
}
function projectDoc(doc) {
  const entries=Object.entries(state.project);
  if(!entries.length) return doc;
  const include=entries.some(([key,value])=>key!=='_id'&&value===1)||(entries.length===1&&state.project._id===1);
  return Object.fromEntries(Object.entries(doc).filter(([key])=>
    include?(key==='_id'?state.project._id!==0:state.project[key]===1):state.project[key]!==0));
}
function renderTree() {
  const query=$('#tree-search').value.toLowerCase();
  const visible=Object.keys(collections).filter(name=>name.includes(query));
  $('#tree').innerHTML=`
    <button class="tree-row connection other" disabled title="${t('otherConnection')}">${icon('chevron-right')}<img class="db-logo" src="../../apps/fluxdb-desktop/assets/db/mysql.svg" alt="">dev-mysql</button>
    <button class="tree-row connection live" data-action="connection" aria-expanded="${state.connectionOpen}" aria-label="${t('expandConnection')}">${icon(state.connectionOpen?'chevron-down':'chevron-right')}<img class="db-logo" src="../../apps/fluxdb-desktop/assets/db/mongodb.svg" alt="">local-mongo<span class="dot"></span></button>
    ${state.connectionOpen?`<button class="tree-row db" data-action="database" aria-expanded="${state.databaseOpen}" aria-label="${t('expandDatabase')}">${icon(state.databaseOpen?'chevron-down':'chevron-right')}<span class="database-icon">${icon('database')}</span>shop_demo</button>
    ${state.databaseOpen?visible.map(name=>`<button class="tree-row collection ${state.collection===name?'selected':''}" data-collection="${name}" ${state.collection===name?'aria-current="page"':''}>${icon('table-2')}${name}<span class="tail">${collections[name].docs.length}</span></button>`).join(''):''}`:''}
    <button class="tree-row connection other" disabled title="${t('otherConnection')}">${icon('chevron-right')}<img class="db-logo" src="../../apps/fluxdb-desktop/assets/db/redis.svg" alt="">docker-redis</button>
    <button class="tree-row connection other" disabled title="${t('otherConnection')}">${icon('chevron-right')}<img class="db-logo" src="../../apps/fluxdb-desktop/assets/db/postgres.svg" alt="">dev-postgres</button>`;
}
function renderCard(doc,index) {
  const id=docId(doc),collapsed=state.collapsed.has(id),value=projectDoc(doc);
  return `<article class="document-card ${state.inspector&&docId(state.selected)===id?'is-selected':''}">
    <header class="document-head"><button class="icon-button" data-collapse="${id}" aria-expanded="${!collapsed}" aria-label="${t(collapsed?'expandDoc':'collapseDoc')}">${icon(collapsed?'chevron-right':'chevron-down')}</button>
    <span class="record-number">${index+1+state.page*state.pageSize}</span><span class="id-prefix">_id: ObjectId</span><code>${id}</code>
    <button class="icon-button doc-open" data-open="${id}" aria-label="${t('openDetail')}" title="${t('openDetail')}">${icon('panel-right')}</button>
    <button class="icon-button" data-copy="${id}" aria-label="${t('copyDoc')}" title="${t('copyDoc')}">${icon('copy')}</button></header>
    ${collapsed?'':`<div class="field-list">${state.view==='json'?`<div class="json-document">${jsonMarkup(value)}</div>`:Object.entries(value).map(([key,val])=>renderField(key,val)).join('')}</div>`}
  </article>`;
}
function renderTable(docs) {
  const rows=docs.map(projectDoc),keys=[...new Set(rows.flatMap(doc=>Object.keys(doc)))];
  return `<div class="table-wrap"><table class="data-table"><thead><tr><th>#</th>${keys.map(key=>`<th>${esc(key)}<small>${bsonType(rows.find(doc=>doc[key]!==undefined)?.[key])}</small></th>`).join('')}</tr></thead>
    <tbody>${rows.map((doc,index)=>`<tr data-open="${docId(docs[index])}" tabindex="0" aria-label="${t('openDetail')}" class="${state.inspector&&docId(state.selected)===docId(docs[index])?'selected':''}"><td>${index+1+state.page*state.pageSize}</td>${keys.map(key=>{
      const v=doc[key],type=bsonType(v);
      const label=type==='Object'?`{ ${Object.keys(v).length} ${t('fieldCount')} }`:type==='Array'?`[ ${v.length} ${t('items')} ]`:type==='ObjectId'?v.$oid:type==='Date'?v.$date:type==='Decimal128'?v.$numberDecimal:v===undefined?t('missing'):valueLabel(v);
      return `<td class="${v==null?'null-value':''}">${esc(label)}</td>`;
    }).join('')}</tr>`).join('')}</tbody></table></div>`;
}
function renderResults() {
  const pages=Math.max(1,Math.ceil(state.results.length/state.pageSize));
  state.page=Math.min(state.page,pages-1);
  const start=state.page*state.pageSize,docs=state.results.slice(start,start+state.pageSize);
  $('#result-summary').textContent=`${state.results.length} ${t('records')} · ${t('sampleSuffix')}`;
  $('#page-range').textContent=`${t('range')} ${docs.length?start+1:0}–${start+docs.length} / ${state.results.length}`;
  $('#page-number').textContent=`${state.page+1} / ${pages}`;
  $('#previous').disabled=state.page===0;$('#next').disabled=state.page>=pages-1;
  $('#results').innerHTML=docs.length?(state.view==='table'?renderTable(docs):docs.map(renderCard).join('')):
    `<div class="empty-state">${icon('search')}<span>${t('noResults')}</span><button class="button" data-action="reset">${t('resetAndShow')}</button></div>`;
  $('#inspector').hidden=!state.inspector;
  $('#detail-toggle').setAttribute('aria-pressed',String(state.inspector));
  $('#inspector-code').innerHTML=state.selected?jsonMarkup(state.selected):'';
}
function indexDefinitions() {
  const config=current();
  return [{name:'_id_',keys:[['_id',1]],unique:true},
    ...(config.unique?[{name:`${config.unique}_1`,keys:[[config.unique,1]],unique:true}]:[]),
    {name:`${config.secondary}_1`,keys:[[config.secondary,1]],unique:false}];
}
function renderSecondary() {
  const tab=state.tab,data=current().docs;
  if(tab==='indexes') {
    $('#secondary-page').innerHTML=`<header class="section-heading"><div><h2>${t('indexes')}</h2><p>shop_demo.${state.collection}</p></div><button class="button" disabled>${icon('plus')}${t('createIndex')}</button></header>
    <div class="table-wrap"><table><thead><tr><th>${t('indexName')}</th><th>${t('indexKeys')}</th><th>${t('properties')}</th></tr></thead><tbody>${indexDefinitions().map(i=>`<tr><td class="index-name">${i.name}</td><td>${i.keys.map(([key,dir])=>`<span class="index-chip">${key}: ${dir}</span>`).join('')}</td><td>${t(i.unique?'unique':'regular')}</td></tr>`).join('')}</tbody></table></div>
    <p class="section-note">${icon('info')}${t('indexesNote')}</p>`;
  } else if(tab==='aggregation') {
    const key=state.collection==='orders'?'status':state.collection==='customers'?'level':'status';
    const groups=data.reduce((result,doc)=>{result[doc[key]]=(result[doc[key]]||0)+1;return result;},{});
    $('#secondary-page').innerHTML=`<header class="section-heading"><div><h2>${t('aggregationTitle')}</h2><p>${t('aggregationSubtitle')}</p></div></header>
      <div class="pipeline-layout"><div>
        <section class="stage"><div class="stage-head"><span>01</span><code>$match</code><span>${t('stageMatch')}</span></div><pre>{}</pre></section>
        <section class="stage"><div class="stage-head"><span>02</span><code>$group</code><span>${t('stageGroup')}</span></div><pre>{\n  "_id": "$${key}",\n  "count": { "$sum": 1 }\n}</pre></section>
        <p class="section-note">${icon('info')}${t('pipelineNote')}</p>
      </div><section class="pipeline-result"><h3>${t('preview')}</h3>${jsonMarkup(Object.entries(groups).map(([id,count])=>({_id:id,count})))}</section></div>`;
  } else {
    const row=(key,value)=>`<div class="info-row"><span>${t(key)}</span><code>${esc(value)}</code></div>`;
    $('#secondary-page').innerHTML=`<header class="section-heading"><div><h2>${t('collectionInfo')}</h2><p>shop_demo.${state.collection}</p></div></header>
      <div class="info-grid"><section class="info-section"><h3>${t('general')}</h3>${row('database','shop_demo')}${row('name',state.collection)}${row('type','Collection')}${row('count',data.length)}</section>
      <section class="info-section"><h3>${t('dataShape')}</h3>${row('bson',t('bsonValue'))}${row('schema',t('schemaValue'))}${row('type',t('readOnly'))}</section></div><p class="section-note">${icon('info')}${t('infoNote')}</p>`;
  }
}
function render() {
  document.documentElement.lang=lang==='zh'?'zh-CN':'en';
  hydrate();
  $('#language .language-label').textContent=lang==='zh'?'EN':'中';
  $('#object-name').textContent=state.collection;$('#namespace-name').textContent=state.collection;
  $('#status-path').textContent=`local-mongo / shop_demo / ${state.collection}`;
  $('#index-count').textContent=indexDefinitions().length;
  renderTree();renderResults();
  $('#document-page').hidden=state.tab!=='documents';$('#secondary-page').hidden=state.tab==='documents';
  if(state.tab!=='documents') renderSecondary();
  document.querySelectorAll('[data-tab]').forEach(el=>{
    const active=el.dataset.tab===state.tab;
    el.classList.toggle('selected',active);el.setAttribute('aria-selected',String(active));
    el.id=`tab-${el.dataset.tab}`;el.tabIndex=active?0:-1;
    el.setAttribute('aria-controls',el.dataset.tab==='documents'?'document-page':'secondary-page');
  });
  $('#document-page').setAttribute('aria-labelledby','tab-documents');
  $('#secondary-page').setAttribute('aria-labelledby',`tab-${state.tab}`);
  document.querySelectorAll('[data-view]').forEach(el=>{
    const active=el.dataset.view===state.view;el.classList.toggle('selected',active);el.setAttribute('aria-pressed',String(active));
  });
}
let toastTimer;
function toast(message) {
  clearTimeout(toastTimer);$('#toast').textContent=message;$('#toast').hidden=false;
  toastTimer=setTimeout(()=>{$('#toast').hidden=true;},2600);
}
function queryObject(value,key) {
  let parsed;
  try {parsed=JSON.parse(value);} catch {throw new Error(t(key));}
  if(!parsed || typeof parsed!=='object'||Array.isArray(parsed)) throw new Error(t(key));
  return parsed;
}
function resetQuery() {
  $('#filter').value='{}';$('#project').value='{}';$('#sort').value='{}';$('#limit').value='8';
  $('#query-error').hidden=true;state.results=current().docs;state.project={};state.page=0;
  state.selected=state.results[0];renderResults();
}
function runQuery(event) {
  event?.preventDefault();
  try {
    const filter=queryObject($('#filter').value,'queryInvalid');
    const projection=queryObject($('#project').value,'projectInvalid');
    const sort=queryObject($('#sort').value,'sortInvalid');
    const limit=Number($('#limit').value);
    if(!Number.isInteger(limit)||limit<1||limit>100) throw new Error(t('limitInvalid'));
    // The prototype deliberately rejects unimplemented MQL rather than simulating success.
    for(const [key,val] of Object.entries(filter)) {
      if(key.includes('.')||key.startsWith('$')||(!isScalar(val)&&(Array.isArray(val)||!val||Object.keys(val).length!==1||
        !Object.entries(val).every(([op,v])=>op==='$eq'?isScalar(v):op==='$gte'?['string','number'].includes(typeof v):op==='$in'&&Array.isArray(v)&&v.every(isScalar)))))
        throw new Error(t('queryInvalid'));
    }
    const projectionEntries=Object.entries(projection);
    const includes=projectionEntries.some(([key,v])=>key!=='_id'&&v===1),excludes=projectionEntries.some(([key,v])=>key!=='_id'&&v===0);
    if((includes&&excludes)||projectionEntries.some(([key,v])=>key.includes('.')||key.startsWith('$')||![0,1].includes(v))) throw new Error(t('projectInvalid'));
    if(Object.entries(sort).some(([key,v])=>key.includes('.')||key.startsWith('$')||![1,-1].includes(v)||current().docs.some(doc=>doc[key]!==undefined&&!scalarComparable(doc[key])))) throw new Error(t('sortInvalid'));
    let docs=current().docs.filter(doc=>Object.entries(filter).every(([key,val])=>{
      const actual=doc[key];
      if(isScalar(val)) return actual===val || (val===null&&actual===undefined);
      const [op,want]=Object.entries(val)[0];
      return op==='$eq'?actual===want||(want===null&&actual===undefined):op==='$gte'?typeof actual===typeof want&&actual>=want:want.some(v=>v===actual||(v===null&&actual===undefined));
    }));
    if(Object.keys(sort).length) docs=[...docs].sort((a,b)=>{
      for(const [key,dir] of Object.entries(sort)) {
        const left=comparable(a[key]),right=comparable(b[key]);
        if(left==null&&right!=null)return -dir;if(left!=null&&right==null)return dir;
        if(left<right)return -dir;if(left>right)return dir;
      }
      return 0;
    });
    state.results=docs.slice(0,limit);state.project=projection;state.page=0;state.selected=state.results[0]||null;
    $('#query-error').hidden=true;renderResults();
  } catch(error) {
    $('#query-error').textContent=error.message;$('#query-error').hidden=false;
  }
}
let inspectorReturnFocus;
function openInspector(id,trigger) {
  state.selected=state.results.find(doc=>docId(doc)===id)||state.selected;
  if(!state.selected) return;
  inspectorReturnFocus=trigger||document.activeElement;state.inspector=true;renderResults();$('#close-inspector').focus();
}
function closeInspector() {
  state.inspector=false;renderResults();
  if(inspectorReturnFocus?.isConnected) inspectorReturnFocus.focus();else $('#detail-toggle').focus();
}
document.addEventListener('click',async event=>{
  const target=event.target.closest('button,[data-open]');
  if(!target||target.disabled)return;
  if(target.dataset.collection) {
    state.collection=target.dataset.collection;state.page=0;state.inspector=false;state.collapsed.clear();
    resetQuery();render();$('.sidebar').classList.remove('mobile-open');
  } else if(target.dataset.tab) {state.tab=target.dataset.tab;render();}
  else if(target.dataset.view) {state.view=target.dataset.view;render();$(`[data-view="${state.view}"]`).focus();}
  else if(target.dataset.open) openInspector(target.dataset.open,target);
  else if(target.dataset.collapse) {
    const id=target.dataset.collapse;
    if(state.collapsed.has(id))state.collapsed.delete(id);else state.collapsed.add(id);
    renderResults();document.querySelector(`[data-collapse="${id}"]`)?.focus();
  } else if(target.dataset.copy) {
    const doc=current().docs.find(item=>docId(item)===target.dataset.copy);
    try {await navigator.clipboard.writeText(JSON.stringify(doc,null,2));toast(t('copied'));}
    catch {toast(t('copyFailed'));}
  } else if(target.dataset.action==='connection'){state.connectionOpen=!state.connectionOpen;renderTree();}
  else if(target.dataset.action==='database'){state.databaseOpen=!state.databaseOpen;renderTree();}
  else if(target.dataset.action==='reset') resetQuery();
});
$('#query-form').addEventListener('submit',runQuery);
$('#reset').onclick=resetQuery;
$('#query-options').onclick=()=>{
  const open=$('#query-extras').hidden;$('#query-extras').hidden=!open;
  $('#query-options').setAttribute('aria-expanded',String(open));
};
$('#detail-toggle').onclick=()=>state.inspector?closeInspector():openInspector(docId(state.selected),$('#detail-toggle'));
$('#close-inspector').onclick=closeInspector;
$('#page-size').onchange=event=>{state.pageSize=Number(event.target.value);state.page=0;renderResults();};
$('#previous').onclick=()=>{state.page--;renderResults();$('#results').scrollTop=0;};
$('#next').onclick=()=>{state.page++;renderResults();$('#results').scrollTop=0;};
$('#tree-search').oninput=renderTree;
$('#language').onclick=()=>{lang=lang==='zh'?'en':'zh';$('#query-error').hidden=true;render();};
$('#theme').onclick=()=>{
  const dark=document.documentElement.dataset.theme==='light';
  document.documentElement.dataset.theme=dark?'dark':'light';$('#theme').innerHTML=icon(dark?'moon':'sun');
};
$('#sidebar-toggle').onclick=()=>{
  if(matchMedia('(max-width:800px)').matches) {
    $('.sidebar').hidden=false;$('.sidebar').classList.toggle('mobile-open');
  }
  else $('.sidebar').hidden=!$('.sidebar').hidden;
};
function refreshSample() {
  if(state.busy)return;
  state.busy=true;$('#refresh').disabled=true;$('#tree-refresh').disabled=true;$('#results').setAttribute('aria-busy','true');
  setTimeout(()=>{
    state.busy=false;$('#refresh').disabled=false;$('#tree-refresh').disabled=false;
    $('#results').setAttribute('aria-busy','false');runQuery();renderTree();toast(t('refreshed'));
  },350);
}
$('#refresh').onclick=refreshSample;$('#tree-refresh').onclick=refreshSample;
$('#about').onclick=()=>$('#design-dialog').showModal();
$('#close-dialog').onclick=()=>$('#design-dialog').close();$('#got-it').onclick=()=>$('#design-dialog').close();
$('#design-dialog').addEventListener('close',()=>$('#about').focus());
$('#design-dialog').addEventListener('click',event=>{
  if(event.target!==event.currentTarget)return;
  const rect=event.currentTarget.getBoundingClientRect();
  if(event.clientX<rect.left||event.clientX>rect.right||event.clientY<rect.top||event.clientY>rect.bottom) event.currentTarget.close();
});
document.addEventListener('keydown',event=>{
  if(event.key==='Escape'&&!$('#design-dialog').open) {
    if(state.inspector)closeInspector();$('.sidebar').classList.remove('mobile-open');
  }
  if((event.metaKey||event.ctrlKey)&&event.key==='Enter'&&state.tab==='documents') runQuery(event);
  const row=event.target.closest('tr[data-open]');
  if(row&&(event.key==='Enter'||event.key===' ')){event.preventDefault();openInspector(row.dataset.open,row);}
});
$('.section-tabs').addEventListener('keydown',event=>{
  const tabs=[...document.querySelectorAll('[data-tab]')],index=tabs.indexOf(event.target);
  if(index<0||!['ArrowRight','ArrowLeft','Home','End'].includes(event.key))return;
  event.preventDefault();
  const next=event.key==='Home'?0:event.key==='End'?tabs.length-1:(index+(event.key==='ArrowRight'?1:-1)+tabs.length)%tabs.length;
  tabs[next].click();tabs[next].focus();
});
render();

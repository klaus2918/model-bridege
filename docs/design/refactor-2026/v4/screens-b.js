/* Screens 06–11 */
function sAccessDetail(){
  const items=[
    {name:'DeepSeek 官方',id:'deepseek',kind:'官方',kindCls:'ok',url:'api.deepseek.com',models:'8',cred:'ok',health:'正常',healthCls:'ok',p95:'820ms',today:'842',rate:'99.6%',default:true},
    {name:'Anthropic 官方',id:'anthropic',kind:'官方',kindCls:'ok',url:'api.anthropic.com',models:'5',cred:'ok',health:'正常',healthCls:'ok',p95:'1.1s',today:'126',rate:'100%'},
    {name:'ZCode 账号池',id:'zcode',kind:'账号池',kindCls:'violet',url:'zcode.ai/api/v1',models:'3',cred:'pool',pool:'2 / 4',health:'冷却 1',healthCls:'warn',p95:'1.4s',today:'216',rate:'97.2%'},
    {name:'My Gateway',id:'my-gateway',kind:'自定义',kindCls:'info',url:'gateway.example.com',models:'6',cred:'missing',health:'缺 Key',healthCls:'err',p95:'—',today:'0',rate:'—'},
    {name:'OpenRouter Free',id:'openrouter',kind:'免费',kindCls:'warn',url:'openrouter.ai/api/v1',models:'14',cred:'ok',health:'限流',healthCls:'warn',p95:'2.3s',today:'100',rate:'92.8%'}
  ];
  return board('b06','',shell('access',`
    <div class="page-head"><div><h2 class="t">接入管理</h2><div class="s">官方、自定义、免费来源、账号池和插件，统一列表、统一抽屉、统一健康口径。</div></div><div class="actions"><button class="btn">连通性自检</button><button class="btn primary">＋ 添加接入</button></div></div>
    <div class="toolbar"><div class="field" style="min-width:250px"><span class="mono" style="font-size:10px;color:var(--faint)">/</span><input placeholder="搜索名称、地址或模型…"></div><select class="select"><option>全部来源</option></select><select class="select"><option>全部健康状态</option></select><div class="right"><span class="hint">7 个接入 · 5 正常</span><div class="seg"><button class="active">列表</button><button>卡片</button></div></div></div>
    <div class="table-wrap"><table><thead><tr><th style="width:30px">${cb()}</th><th>接入</th><th>类型</th><th>协议 / 地址</th><th class="right">模型</th><th>凭据</th><th>健康</th><th class="right">今日</th></tr></thead><tbody>${items.map(a=>accessRow(a,a.id==='deepseek')).join('')}</tbody></table></div>
    <div class="scrim"></div>
    <aside class="drawer">
      <div class="drawer-head"><div class="row1"><span class="avatar" style="width:30px;height:30px;color:var(--signal);border-color:var(--signal-line);background:var(--signal-dim)">DS</span><h2>DeepSeek 官方</h2>${badge('官方','ok')}${badge('默认','ok')}<span class="actions"><button class="icon-btn" style="width:26px;height:26px">×</button></span></div><div class="sub">deepseek · OpenAI 兼容 · api.deepseek.com · 8 模型</div></div>
      <div class="tabs"><button class="active">概览</button><button>连接</button><button>凭据</button><button>模型</button><button>高级</button><button>诊断</button></div>
      <div class="drawer-body">
        <div class="kpis" style="grid-template-columns:repeat(3,1fr);margin-bottom:12px">${kpi('健康','正常','P95 820ms','up',null,'')}${kpi('今日请求','842','成功率 99.6%','',null,'')}${kpi('今日费用','¥8.42','占比 65.6%','',null,'')}</div>
        <div class="section-title">连接</div>
        <div class="kv"><span class="k">Base URL</span><span class="v mono">https://api.deepseek.com</span><span class="k">协议</span><span class="v">OpenAI 兼容</span><span class="k">模型目录</span><span class="v">目录拉取 · 最近 10:31 成功</span><span class="k">超时</span><span class="v mono">连接 10s / 请求 120s</span></div>
        <div class="section-title">凭据</div>
        <div class="banner" style="display:flex;align-items:center;gap:8px"><span class="dot ok"></span><div><b>已配置 · 环境变量</b><div class="hint">DEEPSEEK_API_KEY · 最后更新 2 天前 · 只写不回显</div></div><button class="btn sm" style="margin-left:auto">替换</button></div>
        <div class="section-title">模型与定价</div>
        <div class="box" style="padding:0"><div class="row" style="padding:8px 12px"><div class="grow"><div class="nm" style="font-size:11.5px">deepseek-v4.1-flash</div><div class="sub">别名 ds-flash · 默认模型</div></div><div class="mono" style="font-size:10px;color:var(--muted)">¥0.50 / ¥2.00</div>${badge('已定价','ok')}</div><div class="row" style="padding:8px 12px"><div class="grow"><div class="nm" style="font-size:11.5px">deepseek-v4.1-pro</div><div class="sub">别名 ds-pro</div></div><div class="mono" style="font-size:10px;color:var(--muted)">¥2.00 / ¥8.00</div>${badge('已定价','ok')}</div><div class="row" style="padding:8px 12px"><div class="grow"><div class="nm" style="font-size:11.5px">deepseek-r2</div><div class="sub">推理模型 · 目录新增</div></div><div class="mono" style="font-size:10px;color:var(--muted)">—</div>${badge('未定价','warn')}</div></div>
        <div class="section-title">最近诊断</div>
        <div class="box" style="padding:10px 12px;font-size:11px;color:var(--muted)"><div style="display:flex;gap:8px;align-items:center"><span class="dot ok"></span><b style="color:var(--text)">全部检查通过</b><span class="mono" style="margin-left:auto">10:31:22</span></div><div style="margin-top:5px">DNS 12ms · TLS 48ms · HTTP 210ms · 模型 320ms</div></div>
      </div>
      <div class="drawer-foot"><button class="btn sm">测试连接</button><button class="btn sm danger">删除</button><span class="spacer"></span><button class="btn sm">取消</button><button class="btn sm primary">保存</button></div>
    </aside>
  `),'06 · 接入详情');
}

function sAddAccess(){
  const templates=[['DeepSeek','deepseek','官方','8 模型','ok'],['Anthropic','anthropic','官方','5 模型','ok'],['OpenAI','openai','官方','12 模型','ok'],['通义千问','dashscope','官方','9 模型','ok'],['OpenRouter','openrouter','聚合','14 模型','warn'],['CommandCode','commandcode','插件','3 模型','blue']];
  return board('b07','',shell('access',`
    <div class="page-head"><div><h2 class="t">接入管理</h2><div class="s">官方、自定义、免费来源、账号池和插件，统一列表、统一抽屉、统一健康口径。</div></div><div class="actions"><button class="btn">连通性自检</button><button class="btn primary">＋ 添加接入</button></div></div>
    <div class="toolbar"><div class="field" style="min-width:250px"><span class="mono" style="font-size:10px;color:var(--faint)">/</span><input placeholder="搜索名称、地址或模型…"></div><select class="select"><option>全部来源</option></select><div class="right"><span class="hint">7 个接入 · 5 正常</span></div></div>
    <div class="table-wrap"><table><thead><tr><th style="width:30px">${cb()}</th><th>接入</th><th>类型</th><th>协议 / 地址</th><th class="right">模型</th><th>健康</th><th class="right">今日</th></tr></thead><tbody>
      ${[{name:'DeepSeek 官方',id:'deepseek',kind:'官方',kindCls:'ok',url:'api.deepseek.com',models:'8',cred:'ok',health:'正常',healthCls:'ok',p95:'820ms',today:'842',rate:'99.6%'},{name:'Anthropic 官方',id:'anthropic',kind:'官方',kindCls:'ok',url:'api.anthropic.com',models:'5',cred:'ok',health:'正常',healthCls:'ok',p95:'1.1s',today:'126',rate:'100%'},{name:'ZCode 账号池',id:'zcode',kind:'账号池',kindCls:'violet',url:'zcode.ai/api/v1',models:'3',cred:'pool',pool:'2 / 4',health:'冷却 1',healthCls:'warn',p95:'1.4s',today:'216',rate:'97.2%'},{name:'My Gateway',id:'my-gateway',kind:'自定义',kindCls:'info',url:'gateway.example.com',models:'6',cred:'missing',health:'缺 Key',healthCls:'err',p95:'—',today:'0',rate:'—'}].map(a=>accessRow(a)).join('')}
    </tbody></table></div>
    <div class="scrim" style="background:rgba(2,5,4,.68)"></div>
    <div class="modal">
      <div class="modal-head"><h2>添加接入</h2>${badge('官方模板','ok')}<span class="hint" style="margin-left:6px">第 2 步，共 3 步</span><span style="margin-left:auto;display:flex;gap:5px;align-items:center"><span class="dot ok"></span><span class="hint">已选 DeepSeek</span><button class="icon-btn" style="width:26px;height:26px">×</button></span></div>
      <div class="modal-body">
        <div class="modal-side">
          <div class="hint" style="font-size:9.5px;letter-spacing:.1em;margin-bottom:8px">选择来源</div>
          <div class="stepper" style="margin-bottom:14px"><div class="step done"><span class="n">✓</span><div><b>选择来源</b><div class="hint">从模板开始</div></div></div></div>
          <div style="display:flex;flex-direction:column;gap:5px">
            ${templates.map((t,i)=>`<div style="display:flex;align-items:center;gap:8px;padding:7px 8px;border-radius:7px;border:1px solid ${i===0?'var(--signal-line)':'transparent'};background:${i===0?'var(--signal-dim)':'transparent'}"><span class="avatar" style="width:22px;height:22px;font-size:8px">${t[1].slice(0,2).toUpperCase()}</span><div style="min-width:0"><div style="font-size:11.5px;font-weight:550">${t[0]}</div><div class="hint" style="font-size:9px">${t[2]} · ${t[3]}</div></div>${i===0?'<span class="dot ok" style="margin-left:auto"></span>':''}</div>`).join('')}
          </div>
          <div class="divider"></div>
          <button class="btn sm ghost" style="width:100%;justify-content:flex-start">＋ 自定义 / 中转</button>
          <button class="btn sm ghost" style="width:100%;justify-content:flex-start;margin-top:4px">＋ 从免费来源添加</button>
          <button class="btn sm ghost" style="width:100%;justify-content:flex-start;margin-top:4px">＋ 从插件接入</button>
        </div>
        <div class="modal-main">
          <div style="display:flex;align-items:center;gap:10px;margin-bottom:12px"><span class="avatar" style="width:34px;height:34px;color:var(--signal);border-color:var(--signal-line);background:var(--signal-dim)">DS</span><div><div style="font-weight:650;font-size:15px">DeepSeek 官方</div><div class="hint">OpenAI 兼容 · api.deepseek.com · 8 模型</div></div>${badge('官方模板','ok')}<span class="hint" style="margin-left:auto">模板 v2 · 更新于 3 天前</span></div>
          <div class="form-grid">
            <div class="form-row"><label>接入标识</label><div class="input"><span class="mono" style="color:var(--text)">deepseek</span><span class="hint" style="margin-left:auto">用于 /deepseek/v1</span></div><div class="help">小写字母、数字、连字符；创建后可改，但会改变调用地址。</div></div>
            <div class="form-row"><label>显示名称</label><div class="input"><span>DeepSeek 官方</span></div><div class="help">出现在列表、用量和错误提示中。</div></div>
            <div class="form-row full"><label>API Key <span style="color:var(--danger)">*</span></label><div class="input focus"><span class="mask">••••••••••••••••••••••••</span>${badge('只写','gray')}<span class="hint" style="margin-left:auto">已从剪贴板识别</span></div><div class="help">密钥写入后不再回显；可在环境变量 DEEPSEEK_API_KEY 中维护。</div></div>
            <div class="form-row full"><label>Base URL</label><div class="input"><span class="mono" style="color:var(--text)">https://api.deepseek.com</span>${badge('模板默认','gray')}</div></div>
            <div class="form-row"><label>协议</label><div class="input"><span>OpenAI 兼容</span><span style="margin-left:auto;color:var(--faint)">▾</span></div></div>
            <div class="form-row"><label>默认模型</label><div class="input"><span class="mono">deepseek-v4.1-flash</span><span style="margin-left:auto;color:var(--faint)">▾</span></div></div>
          </div>
          <div class="banner info" style="margin-top:11px"><span>ⓘ</span><div><b>官方模板已预填 6 项。</b> 你只需要粘贴 Key 并确认标识；模型、别名、隐私能力会随后探测补齐。</div></div>
          <div style="display:flex;gap:8px;margin-top:10px"><button class="btn sm ghost">高级设置（模型 / 别名 / 逐模型 Key / 隐私 / 超时）▾</button></div>
        </div>
      </div>
      <div class="modal-foot"><button class="btn sm ghost">上一步</button><span class="hint">按 Esc 关闭 · 有未保存改动会确认</span><span class="spacer" style="flex:1"></span><button class="btn sm">取消</button><button class="btn sm primary">验证并添加 →</button></div>
    </div>
  `),'07 · 添加接入');
}

function sLogs(){
  const lines=[
    ['10:42:18','INFO','proxy','req_01J8K3F2 → deepseek/deepseek-v4.1-flash 200 · 820ms · 8,214 tok'],
    ['10:42:17','DEBUG','router','qualified model resolved: deepseek/deepseek-v4.1-flash'],
    ['10:41:52','INFO','proxy','req_01J8K2A1 → anthropic/claude-sonnet-4.5 200 · 1.4s'],
    ['10:40:31','WARN','pool','zcode member #3 cooling down after 429 · retry in 42s'],
    ['10:40:31','INFO','router','failover zcode#3 → zcode#1 · attempt 2'],
    ['10:39:07','INFO','proxy','req_01J8JZY0 → deepseek/deepseek-v4.1-flash 200 · 640ms'],
    ['10:38:44','ERROR','proxy','req_01J8JY11 → dashscope/qwen3-max 429 · rate_limit_exceeded'],
    ['10:38:44','WARN','pricing','no price rule for dashscope/qwen3-max · cost recorded as null'],
    ['10:37:20','INFO','proxy','req_01J8JX02 → openrouter/gpt-5.2 200 · 3.8s'],
    ['10:36:02','INFO','proxy','req_01J8JW91 → deepseek/deepseek-v4.1-flash 200 · 510ms'],
    ['10:35:48','DEBUG','config','config revision cfg_01J8 loaded · 7 upstreams · 32 models'],
    ['10:31:22','INFO','diagnostics','connectivity check: 7 passed, 1 warning, 0 failed']
  ];
  return board('b08','',shell('logs',`
    <div class="page-head"><div><h2 class="t">日志与诊断</h2><div class="s">出问题时先看结论，再定位到一行日志；日志只读，诊断可复制。</div></div><div class="actions"><button class="btn">导出诊断</button><button class="btn primary">重新检查</button></div></div>
    <div class="tabs" style="padding:0;margin-bottom:12px;border-bottom:1px solid var(--line)"><button class="active" style="padding-left:2px">运行日志 <span class="badge gray" style="margin-left:5px">1,284 行</span></button><button>诊断 <span class="badge warn" style="margin-left:5px">1 项</span></button></div>
    <div style="display:grid;grid-template-columns:minmax(0,1fr) 318px;gap:12px;height:calc(100% - 120px)">
      <div style="display:flex;flex-direction:column;min-height:0">
        <div class="toolbar" style="margin-bottom:8px"><select class="select"><option>全部级别</option><option>ERROR</option><option>WARN</option><option>INFO</option><option>DEBUG</option></select><div class="field" style="min-width:220px"><span class="mono" style="font-size:10px;color:var(--faint)">/</span><input placeholder="搜索消息、请求 ID…"></div><select class="select"><option>最近 500 行</option></select><div class="right"><label style="display:flex;align-items:center;gap:5px;font-size:10.5px;color:var(--muted)"><span class="cb on"></span>自动刷新</label><button class="btn sm ghost">跳到末尾</button><button class="btn sm ghost">清空</button></div></div>
        <div class="log" style="flex:1;overflow:hidden">
          ${lines.map((l,i)=>`<div><span class="ln">${(1284-lines.length+i+1).toString().padStart(4,' ')}</span><span class="lv ${l[1]==='ERROR'?'e':l[1]==='WARN'?'w':l[1]==='DEBUG'?'d':'i'}">${l[1].padEnd(5,' ')}</span><span style="color:var(--faint)">${l[2].padEnd(11,' ')}</span><span>${l[3]}</span></div>`).join('')}
          <div style="margin-top:8px;color:var(--signal)">▌ 已连接 · 等待新日志…</div>
        </div>
        <div style="display:flex;align-items:center;gap:10px;padding-top:8px" class="hint"><span>滚动到底部自动跟随</span><span style="margin-left:auto">UTF-8 · 等宽</span><label style="display:flex;align-items:center;gap:4px">自动换行 <span class="cb on"></span></label></div>
      </div>
      <div class="stack">
        <div class="box"><div class="box-head"><h2>诊断摘要</h2><span class="badge warn" style="margin-left:auto">1 项需处理</span></div><div style="padding:10px 12px">
          <div class="banner warn" style="margin-bottom:8px"><span>!</span><div><b>DashScope 未配置价格</b><div class="hint" style="margin-top:2px">9 个模型可调用，但费用记录为 null。</div><button class="btn sm" style="margin-top:6px">去定价 →</button></div></div>
          <div style="display:flex;gap:8px;align-items:center;padding:7px 2px;font-size:11px;color:var(--muted)"><span class="dot ok"></span>配置结构校验 <span class="mono" style="margin-left:auto">通过</span></div>
          <div style="display:flex;gap:8px;align-items:center;padding:7px 2px;font-size:11px;color:var(--muted)"><span class="dot ok"></span>7 个接入连通性 <span class="mono" style="margin-left:auto">6 / 7</span></div>
          <div style="display:flex;gap:8px;align-items:center;padding:7px 2px;font-size:11px;color:var(--muted)"><span class="dot warn"></span>凭据池冷却 <span class="mono" style="margin-left:auto">1</span></div>
          <div style="display:flex;gap:8px;align-items:center;padding:7px 2px;font-size:11px;color:var(--muted)"><span class="dot ok"></span>插件契约 <span class="mono" style="margin-left:auto">5 / 5</span></div>
        </div></div>
        <div class="box"><div class="box-head"><h2>最近检查</h2><span class="hint">10:31:22</span></div><div style="padding:10px 12px"><div class="kv"><span class="k">总数</span><span class="v">12 项检查</span><span class="k">通过</span><span class="v" style="color:var(--signal)">10</span><span class="k">警告</span><span class="v" style="color:var(--amber)">1</span><span class="k">失败</span><span class="v">0</span></div><button class="btn sm" style="width:100%;margin-top:9px">复制诊断 JSON</button></div></div>
      </div>
    </div>
  `),'08 · 日志与诊断');
}

function sConfig(){
  return board('b09','',shell('config',`
    <div class="page-head"><div><h2 class="t">配置中心</h2><div class="s">低频配置集中在这里。日常操作请从总览或接入管理进入；每个待处理项都能直接落到具体对象。</div></div><div class="actions"><button class="btn">查看配置审计</button><button class="btn">导出配置</button></div></div>
    <div class="toolbar" style="margin-bottom:14px"><div class="field" style="min-width:340px"><span class="mono" style="font-size:10px;color:var(--faint)">/</span><input placeholder="搜索配置任务、模型、凭据池、插件…"><kbd style="font-family:var(--mono);font-size:9px;border:1px solid var(--line2);padding:1px 4px;border-radius:3px;color:var(--faint)">⌘K</kbd></div><span class="hint">常用：定价规则 · 统一入口 · 池冷却 · 提示词保留</span><div class="right"><span class="hint">revision cfg_01J8 · 12 分钟前</span></div></div>
    <div class="grid-2">
      ${configDomain('01','模型与路由','模型目录、单价、统一入口、路由组',[['模型','32'],['未定价','2'],['统一入口','已开启'],['路由组','3']],{text:'2 项待处理',cls:'warn',hint:'DashScope · deepseek-r2'},'管理模型与路由','var(--info)')}
      ${configDomain('02','凭据池','密钥池、ZCode 账号、冷却与配额',[['池','3'],['密钥','11'],['冷却中','1'],['配额耗尽','0']],{text:'1 项待处理',cls:'warn',hint:'zcode #3 · 42s 后恢复'},'查看凭据池','var(--violet)')}
      ${configDomain('03','资源库','插件、免费来源、官方模板',[['插件','5'],['未接入','1'],['免费模型','14'],['模板更新','1']],{text:'2 项待处理',cls:'info',hint:'commandcode · deepseek v2'},'打开资源库','var(--amber)')}
      ${configDomain('04','设置','服务、隐私、AI 接口、高级维护',[['保存','10:30'],['待重启','0'],['提示词记录','已开启'],['备份','12']],{text:'全部正常',cls:'ok',hint:'下次自动备份 04:00'},'进入设置','var(--signal)')}
    </div>
    <div class="grid-2" style="grid-template-columns:1.15fr 1fr;margin-top:12px">
      <div class="box"><div class="box-head"><h2>常用配置任务</h2><span class="hint">按最近使用排序</span></div><div>
        ${[['创建凭据池','把多个 Key 组织成可轮换的池','~2 分钟','pool'],['配置统一入口','选择可见上游、命名分组','~3 分钟','entry'],['添加定价规则','为未定价模型补充单价','~1 分钟','pricing'],['开启提示词记录','设置脱敏与保留策略','~1 分钟','privacy'],['导出诊断','打包配置、日志与检查结果','~30 秒','diag']].map(t=>`<div class="row" style="padding:9px 14px"><span class="avatar" style="width:24px;height:24px;font-size:9px">${t[3].slice(0,2).toUpperCase()}</span><div class="grow"><div class="nm" style="font-size:11.5px">${t[0]}</div><div class="sub">${t[1]}</div></div><span class="hint mono">${t[2]}</span><span style="color:var(--faint)">›</span></div>`).join('')}
      </div></div>
      <div class="box"><div class="box-head"><h2>最近变更</h2><span class="hint">可回滚</span></div><div>
        ${[['10:30','deepseek 模板 v2','配置审计 · 自动应用','cfg_01J8'],['10:12','新增池成员 zcode#4','凭据池 · 手动','cfg_01J7'],['09:48','qwen3-max 定价','单价表 · 手动','cfg_01J6'],['09:20','统一入口可见上游 +1','路由 · 手动','cfg_01J5']].map(c=>`<div class="row" style="padding:9px 14px"><span class="mono" style="font-size:10px;color:var(--faint);width:38px">${c[0]}</span><div class="grow"><div class="nm" style="font-size:11.5px">${c[1]}</div><div class="sub">${c[2]}</div></div><span class="mono" style="font-size:9px;color:var(--faint)">${c[3]}</span><button class="btn sm ghost">回滚</button></div>`).join('')}
      </div></div>
    </div>
    <div class="banner" style="margin-top:12px"><span>⌘</span><div><b>配置中心不是表单堆。</b> 每张域卡片先回答“是否正常、有什么待处理、下一步去哪”；只有进入子页后才展示完整表单与高级项。</div></div>
  `),'09 · 配置中心');
}

function sModels(){
  const rows=[
    ['deepseek-v4.1-flash','deepseek','DeepSeek 官方','目录拉取','ds-flash','¥0.50 / ¥2.00','842','ok'],
    ['deepseek-v4.1-pro','deepseek','DeepSeek 官方','目录拉取','ds-pro','¥2.00 / ¥8.00','126','ok'],
    ['deepseek-r2','deepseek','DeepSeek 官方','目录新增','—','未定价','0','warn'],
    ['claude-sonnet-4.5','anthropic','Anthropic 官方','目录拉取','sonnet','$3.00 / $15.00','126','ok'],
    ['glm-5.3-flash','zcode','ZCode 账号池','固定清单','—','套餐内','216','ok'],
    ['qwen3-max','dashscope','DashScope','目录拉取','—','未定价','0','warn'],
    ['gpt-5.2','openrouter','OpenRouter Free','目录拉取','gpt5','$1.25 / $10.00','100','ok']
  ];
  return board('b10','',shell('models',`
    <div class="page-head"><div><h2 class="t">模型与路由 · 模型目录</h2><div class="s">所有接入声明的模型、别名、来源与定价状态；这里是配置中心下的二级页。</div></div><div class="actions"><button class="btn">刷新目录</button><button class="btn">导出</button></div></div>
    <div class="kpis">${kpi('模型总数','32','7 个接入','',null,'MODELS')}${kpi('已定价','30','覆盖 93.8%','up',null,'PRICED')}${kpi('未定价','2','DashScope · deepseek-r2','down',null,'TODO')}${kpi('别名','11','最近新增 2','',null,'ALIAS')}</div>
    <div class="toolbar"><div class="field" style="min-width:250px"><span class="mono" style="font-size:10px;color:var(--faint)">/</span><input placeholder="搜索模型、别名、上游…"></div><select class="select"><option>全部上游</option></select><select class="select"><option>全部来源</option></select><button class="chip on">未定价 2</button><button class="chip">有别名 9</button><div class="right"><span class="hint">32 个模型</span><div class="seg"><button class="active">表格</button><button>分组</button></div></div></div>
    <div class="table-wrap"><table><thead><tr><th>模型</th><th>上游</th><th>来源</th><th>别名</th><th class="right">输入 / 输出 · 百万</th><th class="right">今日调用</th><th>状态</th><th class="right">操作</th></tr></thead><tbody>
      ${rows.map(r=>`<tr><td><div class="name" style="font-size:11.5px">${r[0]}</div><div class="sub">${r[4]!=='—'?'别名 '+r[4]:'无别名'}</div></td><td><div style="font-size:11.5px">${r[2]}</div><div class="sub mono">${r[1]}</div></td><td>${r[3]==='目录新增'?badge('目录新增','info'):badge(r[3], r[3]==='固定清单'?'violet':'gray')}</td><td class="mono" style="font-size:10px;color:var(--muted)">${r[4]}</td><td class="right mono" style="font-size:10.5px">${r[5]==='未定价'?'<span style="color:var(--amber)">未定价</span>':r[5]}</td><td class="right mono">${r[6]}</td><td>${r[7]==='warn'?st('未定价','warn'):st('正常','ok')}</td><td class="right"><button class="btn sm ghost">${r[7]==='warn'?'去定价':'查看'}</button><button class="icon-btn" style="width:24px;height:24px;display:inline-grid">⋯</button></td></tr>`).join('')}
    </tbody></table></div>
    <div style="display:flex;align-items:center;gap:10px;padding-top:10px" class="hint"><span>显示 1–7 / 32</span><span style="margin-left:auto">目录最近拉取 10:31 · 2 个接入未返回目录</span><button class="btn sm ghost">上一页</button><button class="btn sm ghost">下一页</button></div>
  `),'10 · 模型目录');
}

function sPricing(){
  const rows=[
    ['deepseek-v4.1-flash','DeepSeek 官方','deepseek','输入 ¥0.50','输出 ¥2.00','缓存 ¥0.10','有效','ok'],
    ['deepseek-v4.1-pro','DeepSeek 官方','deepseek','输入 ¥2.00','输出 ¥8.00','缓存 ¥0.40','有效','ok'],
    ['claude-sonnet-4.5','Anthropic 官方','anthropic','输入 $3.00','输出 $15.00','缓存 $0.30','有效','ok'],
    ['qwen3-max','DashScope','dashscope','输入 —','输出 —','缓存 —','未配置','warn'],
    ['glm-5.3-flash','ZCode 账号池','zcode','套餐内','套餐内','—','套餐','violet'],
    ['gpt-5.2','OpenRouter Free','openrouter','输入 $1.25','输出 $10.00','缓存 —','有效','ok']
  ];
  return board('b11','',shell('pricing',`
    <div class="page-head"><div><h2 class="t">模型与路由 · 单价表</h2><div class="s">单价匹配优先级：上游固定 &gt; 任意上游；精确 &gt; 通配。保存后可选择重算历史费用。</div></div><div class="actions"><button class="btn">JSON 编辑</button><button class="btn primary">＋ 新增单价</button></div></div>
    <div style="display:flex;align-items:center;gap:14px;padding:9px 12px;border:1px solid var(--line);border-radius:9px;background:var(--surface);margin-bottom:11px"><span class="hint">汇率</span><span class="mono" style="font-size:12px">USD / CNY 7.18</span>${badge('2 小时前更新','gray')}<span class="hint">计费币种</span><div class="seg"><button class="active">CNY</button><button>USD</button></div><span class="hint" style="margin-left:auto">保存后重算历史费用</span><span class="cb on"></span><button class="btn sm">立即重算</button></div>
    <div style="display:grid;grid-template-columns:minmax(0,1fr) 380px;gap:12px;height:calc(100% - 116px)">
      <div class="table-wrap"><table><thead><tr><th>模型 / 上游</th><th>输入</th><th>输出</th><th>缓存</th><th>计费</th><th>状态</th><th></th></tr></thead><tbody>
        ${rows.map((r,i)=>`<tr class="${i===0?'selected':''}"><td><div class="name" style="font-size:11.5px">${r[0]}</div><div class="sub">${r[1]} · ${r[2]}</div></td><td><div class="input" style="padding:4px 7px;min-height:26px;font-size:10.5px;font-family:var(--mono)">${r[3]}</div></td><td><div class="input" style="padding:4px 7px;min-height:26px;font-size:10.5px;font-family:var(--mono)">${r[4]}</div></td><td><div class="input" style="padding:4px 7px;min-height:26px;font-size:10.5px;font-family:var(--mono)">${r[5]}</div></td><td>${r[7]==='warn'?'—':r[7]==='violet'?badge('套餐','violet'):'按量'}</td><td>${r[7]==='warn'?badge('未配置','warn'):badge('有效',r[7]==='violet'?'violet':'ok')}</td><td><button class="icon-btn" style="width:24px;height:24px">⋯</button></td></tr>`).join('')}
      </tbody></table></div>
      <div class="box" style="display:flex;flex-direction:column">
        <div class="box-head"><h2>编辑单价</h2>${badge('deepseek-v4.1-flash','ok')}<span class="hint" style="margin-left:auto">上游固定</span></div>
        <div style="padding:12px 14px;flex:1;overflow:hidden">
          <div class="form-grid" style="grid-template-columns:1fr 1fr;gap:9px">
            <div class="form-row full"><label>模型匹配</label><div class="input"><span class="mono" style="font-size:11px">deepseek-v4.1-flash</span>${badge('精确','gray')}</div></div>
            <div class="form-row full"><label>生效上游</label><div class="input"><span>deepseek</span><span class="hint" style="margin-left:auto">仅此上游 · 可改为任意</span></div></div>
            <div class="form-row"><label>输入 / 百万</label><div class="input focus"><span class="mono">0.50</span><span class="hint" style="margin-left:auto">CNY</span></div></div>
            <div class="form-row"><label>输出 / 百万</label><div class="input"><span class="mono">2.00</span><span class="hint" style="margin-left:auto">CNY</span></div></div>
            <div class="form-row"><label>缓存 / 百万</label><div class="input"><span class="mono">0.10</span><span class="hint" style="margin-left:auto">CNY</span></div></div>
            <div class="form-row"><label>计费类型</label><div class="input"><span>按量</span><span style="margin-left:auto;color:var(--faint)">▾</span></div></div>
            <div class="form-row full"><label>标签</label><div class="chips"><span class="chip on">官方</span><span class="chip">低价</span><span class="chip">默认</span><span class="chip">＋</span></div></div>
          </div>
          <div class="divider"></div>
          <div class="hint" style="margin-bottom:6px">官方参考价</div>
          <div class="box" style="padding:8px 10px;background:var(--surface2)"><div style="display:flex;font-size:10.5px;color:var(--muted)"><span>DeepSeek 官方</span><span class="mono" style="margin-left:auto">¥0.50 / ¥2.00</span></div><div class="hint" style="margin-top:3px">与当前输入一致</div></div>
          <div class="banner warn" style="margin-top:10px"><span>!</span><div>修改单价不会改写已产生的费用；勾选“重算”才会按当前规则回溯。</div></div>
        </div>
        <div class="drawer-foot" style="padding:10px 14px"><button class="btn sm ghost">删除</button><span class="spacer"></span><button class="btn sm">取消</button><button class="btn sm primary">保存</button></div>
      </div>
    </div>
  `),'11 · 单价表');
}

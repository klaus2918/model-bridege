/* Screens 00–05 */
function sCover(){
  const cells=[
    ['01','工作台','总览先给结论：运行状态、今日 KPI、需要处理、用量与提示词快照。','P0'],
    ['02','分析','用量分析与提示词升为一级入口；先结论，后明细，任何行都能打开抽屉。','P1'],
    ['03','运行','接入管理与日志诊断常驻；新增、排障、自检都在一次点击内。','P1 / P2'],
    ['04','配置','模型与路由、凭据池、资源库、设置统一收进低频配置中心。','P3']
  ];
  return board('b00','cover',`<div class="cover-wrap">
    <div class="cover-top"><span style="color:var(--signal)">●</span> SIGNAL DESK / MODEL-GATE <span style="margin-left:auto">DESIGN SYSTEM 2026 · V4</span></div>
    <h1 class="cover-title">把网关，<br>变成一张可读的<em>信号台</em>。</h1>
    <div class="cover-sub">不是把功能堆进侧栏，而是按「关注度 × 使用频率」重新组织。高频页面永远一次点击可达；低频配置收进任务板；每个异常都能直接落到可执行的动作。</div>
    <div class="cover-grid">${cells.map(c=>`<div class="cover-cell"><div class="n">${c[0]} / ${c[3]}</div><div class="t">${c[1]}</div><div class="d">${c[2]}</div></div>`).join('')}</div>
    <div class="cover-foot"><span>18 BOARDS · 1440×1024</span><span>DARK / LIGHT</span><span>IBM PLEX SANS + MONO</span><span class="big">◧</span></div>
  </div>`,'00 · 封面');
}

function sDesignSystem(){
  const dark=[['bg','#0A0E0D'],['surface','#111715'],['surface-2','#161E1B'],['surface-3','#1C2521'],['line','#24302B'],['text','#E8EDE9'],['muted','#8FA098'],['signal','#7BE495'],['amber','#E5B567'],['danger','#F07167'],['info','#7CC7D9'],['violet','#B8A1E8']];
  const light=[['bg','#F4F1E8'],['surface','#FBF9F3'],['surface-2','#EFEBDF'],['surface-3','#E6E1D3'],['line','#D8D2C2'],['text','#1A211E'],['muted','#5F6B64'],['signal','#1F7A4D'],['amber','#9A6414'],['danger','#B3372F'],['info','#23697A'],['violet','#6B4FA0']];
  const types=[['Display','28 / 36','600','接入控制台 v4'],['H1','21 / 30','650','今天，网关运行良好。'],['H2','13 / 20','600','接入健康'],['Body','13.5 / 22','400','官方、自定义、免费来源与账号池统一管理。'],['Caption','11 / 17','400','最近一次配置变更在 12 分钟前'],['Mono','12 / 20','500','req_01J8K3 · 820ms']];
  return board('b01','tall',shell('config',`
    <div class="page-head"><div><h2 class="t">设计系统 · Signal Desk</h2><div class="s">深墨绿底、信号绿强调、细分隔线与等宽数据。领域色只用于图标底、图表和状态。</div></div><div class="actions">${badge('v4.0','ok')}${badge('Dark / Light','gray')}</div></div>

    <div class="section-title">01 / COLOR TOKENS</div>
    <div class="grid-2">
      <div class="box"><div class="box-head"><h2>Dark</h2><span class="hint">默认</span></div><div style="padding:12px"><div class="swatches">${dark.map(s=>`<div class="sw"><div class="c" style="background:${s[1]}"></div><div class="l"><span>${s[0]}</span><span>${s[1]}</span></div></div>`).join('')}</div></div></div>
      <div class="box"><div class="box-head"><h2>Light</h2><span class="hint">阅读模式</span></div><div style="padding:12px"><div class="swatches">${light.map(s=>`<div class="sw"><div class="c" style="background:${s[1]}"></div><div class="l"><span>${s[0]}</span><span>${s[1]}</span></div></div>`).join('')}</div></div></div>
    </div>

    <div class="section-title">02 / TYPE SCALE</div>
    <div class="box" style="padding:4px 16px 10px">${types.map(t=>`<div class="type-row"><span class="tag">${t[0]} · ${t[1]} / ${t[2]}</span><span style="font-size:${t[0]==='Display'?28:t[0]==='H1'?21:t[0]==='H2'?13:13.5}px;font-weight:${t[2]};font-family:${t[0]==='Mono'?'var(--mono)':'var(--font)'};letter-spacing:${t[0]==='Display'||t[0]==='H1'?'-.03em':'0'}">${t[3]}</span></div>`).join('')}</div>

    <div class="section-title">03 / SPACING · RADIUS · ELEVATION</div>
    <div class="grid-3">
      <div class="card"><div class="hint">Spacing</div><div style="display:flex;align-items:flex-end;gap:8px;margin-top:14px">${[4,8,12,16,20,24,32].map(n=>`<div style="text-align:center"><div style="width:${n}px;height:${n}px;background:var(--signal-dim);border:1px solid var(--signal-line);border-radius:2px"></div><div class="hint mono" style="font-size:8px;margin-top:4px">${n}</div></div>`).join('')}</div></div>
      <div class="card"><div class="hint">Radius</div><div style="display:flex;gap:10px;margin-top:14px;align-items:flex-end">${[4,8,12,16].map(n=>`<div style="text-align:center"><div style="width:46px;height:36px;background:var(--surface3);border:1px solid var(--line2);border-radius:${n}px"></div><div class="hint mono" style="font-size:8px;margin-top:4px">${n}</div></div>`).join('')}</div></div>
      <div class="card"><div class="hint">Elevation</div><div style="margin-top:12px;height:52px;border-radius:10px;background:var(--surface2);border:1px solid var(--line);box-shadow:0 18px 40px rgba(0,0,0,.45)"></div><div class="help" style="margin-top:8px">抽屉 0 28 90 / 卡片不使用重阴影</div></div>
    </div>

    <div class="section-title">04 / CONTROLS</div>
    <div class="box"><div class="box-head"><h2>Buttons</h2><span class="hint">Primary / Secondary / Ghost / Danger / Small</span></div><div class="comp-row"><button class="btn primary">＋ 添加接入</button><button class="btn">连通性自检</button><button class="btn ghost">取消</button><button class="btn danger">删除接入</button><button class="btn sm">测试</button><button class="btn sm primary">保存</button></div></div>
    <div class="grid-2" style="margin-top:12px">
      <div class="box"><div class="box-head"><h2>Badges & Status</h2></div><div class="comp-row">${badge('官方','ok')}${badge('自定义','info')}${badge('免费','warn')}${badge('账号池','violet')}${badge('插件','blue')}${st('正常','ok')}${st('需关注','warn')}${st('冷却中','err')}</div></div>
      <div class="box"><div class="box-head"><h2>Inputs</h2></div><div style="padding:12px;display:flex;flex-direction:column;gap:8px"><div class="input"><span class="ph">搜索接入、模型、配置任务…</span><span class="mono" style="margin-left:auto;font-size:9px;color:var(--faint)">⌘K</span></div><div class="input focus"><span class="ph">Base URL</span><span class="mono" style="margin-left:auto;color:var(--text)">https://api.deepseek.com</span></div><div class="input"><span class="ph">API Key</span><span class="mask" style="margin-left:auto">••••••••••••</span>${badge('只写','gray')}</div></div></div>
    </div>

    <div class="section-title">05 / DATA DISPLAY</div>
    <div class="table-wrap"><table><thead><tr><th style="width:28px">${cb()}</th><th>接入</th><th>类型</th><th>协议 / 地址</th><th class="right">模型</th><th>凭据</th><th>健康</th><th class="right">今日</th></tr></thead><tbody>
      ${accessRow({name:'DeepSeek 官方',id:'deepseek',kind:'官方',kindCls:'ok',url:'https://api.deepseek.com',protocol:'OpenAI 兼容',models:'8',cred:'ok',health:'正常',healthCls:'ok',p95:'820ms',today:'842',rate:'99.6%',default:true})}
      ${accessRow({name:'ZCode 账号池',id:'zcode',kind:'账号池',kindCls:'violet',url:'https://zcode.ai/api/v1',protocol:'Anthropic 兼容',models:'3',cred:'pool',pool:'2/4',health:'冷却 1',healthCls:'warn',p95:'1.4s',today:'216',rate:'97.2%'})}
    </tbody></table></div>

    <div class="section-title">06 / DRAWER · TABS · STEPPER</div>
    <div class="grid-2">
      <div class="box" style="padding:0"><div class="drawer-head" style="border-bottom:0"><div class="row1"><h2>DeepSeek 官方</h2>${badge('官方','ok')}<span class="actions"><button class="icon-btn" style="width:26px;height:26px">×</button></span></div><div class="sub">deepseek · OpenAI 兼容 · api.deepseek.com</div></div><div class="tabs"><button class="active">概览</button><button>连接</button><button>凭据</button><button>模型</button><button>高级</button><button>诊断</button></div><div style="padding:14px"><div class="kv"><span class="k">健康</span><span class="v">${st('正常 · P95 820ms','ok')}</span><span class="k">今日</span><span class="v mono">842 请求 / 99.6%</span><span class="k">凭据</span><span class="v">已配置 · 环境变量</span></div></div></div>
      <div class="box" style="padding:14px"><div class="stepper"><div class="step done"><span class="n">✓</span><div><b>选择来源</b><div class="hint">官方厂商 / 自定义 / 免费 / 账号 / 插件</div></div></div><div class="step on"><span class="n">2</span><div><b>填写配置</b><div class="hint">只填 Key + 标识，其余使用模板默认值</div></div></div><div class="step"><span class="n">3</span><div><b>验证完成</b><div class="hint">DNS / TLS / HTTP / 模型探测</div></div></div></div></div>
    </div>

    <div class="section-title">07 / MOTION & ACCESSIBILITY</div>
    <div class="banner info"><span>◐</span><div><b>120ms 页面淡入 · 180ms 抽屉 · 仅“运行中”呼吸。</b> 尊重 <span class="mono">prefers-reduced-motion</span>；正文对比度 ≥ 4.5:1；所有状态不只依赖颜色，同时给出文字与图标。</div></div>
  `,'01 · 设计系统'),'design-system');
}

function sOverview(){
  const health=[
    {name:'DeepSeek 官方',kind:'官方',kindCls:'ok',url:'api.deepseek.com',models:8,cred:'ok',health:'正常',healthCls:'ok',p95:'820ms',today:842,rate:'99.6%',default:true},
    {name:'Anthropic 官方',kind:'官方',kindCls:'ok',url:'api.anthropic.com',models:5,cred:'ok',health:'正常',healthCls:'ok',p95:'1.1s',today:126,rate:'100%'},
    {name:'ZCode 账号池',kind:'账号池',kindCls:'violet',url:'zcode.ai/api/v1',models:3,cred:'pool',pool:'2/4',health:'冷却 1',healthCls:'warn',p95:'1.4s',today:216,rate:'97.2%'},
    {name:'My Gateway',kind:'自定义',kindCls:'info',url:'gateway.example.com',models:6,cred:'missing',health:'缺 Key',healthCls:'err',p95:'—',today:0,rate:'—'},
    {name:'OpenRouter Free',kind:'免费',kindCls:'warn',url:'openrouter.ai/api/v1',models:14,cred:'ok',health:'限流',healthCls:'warn',p95:'2.3s',today:100,rate:'92.8%'}
  ];
  return board('b02','',shell('overview',`
    <div class="page-head"><div><h2 class="t">今天，网关运行良好。</h2><div class="s">7 个接入中 5 个正常，1 个凭据缺失，1 个池成员冷却。最近一次配置变更在 12 分钟前。</div></div><div class="actions"><button class="btn">运行自检</button><button class="btn primary">＋ 添加接入</button></div></div>
    <div class="status-strip"><div class="live"><span class="dot ok pulse"></span>运行中</div><div class="sep"></div><div class="meta"><span>监听 <b>127.0.0.1:8900</b></span><span>统一入口 <b>/unified/v1</b></span><span>默认模型 <b>deepseek-v4.1-flash</b></span><span>运行 <b>4h 15m</b></span></div><div class="hint">cfg_01J8 · 12 分钟前</div></div>
    <div class="kpis">
      ${kpi('今日请求','1,284','↑ 12.4% 较昨日','up',[18,22,20,26,24,31,29,36],'REQ')}
      ${kpi('成功率','99.4%','↑ 0.3% · 失败 7','up',[96,97,96,98,97,99,98,99],'OK')}
      ${kpi('Token 用量','9.2<small>M</small>','今日 6 个模型产生费用','',[12,15,14,19,22,20,26,31],'TOK')}
      ${kpi('估算费用','¥12.84','↑ ¥1.20 · 汇率 7.18','down',[8,10,9,12,13,12,15,18],'COST')}
    </div>
    <div class="grid-main">
      <div class="stack">
        <div class="box">
          <div class="box-head"><h2>流量趋势</h2><div class="legend" style="margin-left:14px"><span><i style="background:var(--signal)"></i>请求</span><span><i style="background:var(--danger)"></i>错误</span></div><div class="seg" style="margin-left:auto"><button class="active">24h</button><button>7d</button><button>30d</button></div><button class="btn sm ghost" style="margin-left:6px">请求 / Token / 错误</button></div>
          <div style="padding:10px 12px 2px">${areaChart({w:820,h:196,series:[{data:[42,38,35,32,30,36,48,62,78,88,96,92,104,112,108,116,122,118,126,131,128,136,142,148],color:'var(--signal)',fill:true},{data:[2,1,2,1,1,2,3,2,2,3,2,1,2,3,2,2,4,3,2,3,2,2,3,4],color:'var(--danger)'}],labels:['00','02','04','06','08','10','12','14','16','18','20','22','now'],ymax:160})}</div>
        </div>
        <div class="box">
          <div class="box-head"><h2>接入健康</h2><span class="hint">异常置顶 · 点击行打开抽屉</span><button class="btn sm ghost" style="margin-left:auto">查看全部</button></div>
          <div style="padding:0 0 2px">
            ${health.map(h=>`<div class="row" style="padding:8px 14px"><span class="dot ${h.healthCls}"></span><div class="grow"><div class="nm" style="font-size:12px">${h.name} ${h.default?badge('默认','ok'):''}</div><div class="sub">${h.url}</div></div>${badge(h.kind,h.kindCls)}<div class="num" style="width:64px">${h.today}<div class="sub" style="text-align:right">${h.rate}</div></div><div class="num" style="width:62px">${h.p95}</div><span class="hint" style="width:66px;text-align:right">${h.health}</span><span style="color:var(--faint)">›</span></div>`).join('')}
          </div>
        </div>
      </div>
      <div class="stack">
        <div class="box">
          <div class="box-head"><h2>需要处理</h2><span class="badge warn" style="margin-left:auto">3</span></div>
          <div>
            <div class="row" style="align-items:flex-start"><span class="avatar" style="color:var(--danger);border-color:rgba(240,113,103,.3);background:var(--danger-dim)">!</span><div class="grow"><div class="nm" style="font-size:12px">My Gateway 缺少 API Key</div><div class="sub">6 个模型不可调用 · 影响 1 个路由组</div></div><button class="btn sm">补 Key</button></div>
            <div class="row" style="align-items:flex-start"><span class="avatar" style="color:var(--amber);border-color:rgba(229,181,103,.3);background:var(--amber-dim)">◷</span><div class="grow"><div class="nm" style="font-size:12px">ZCode 池成员冷却中</div><div class="sub">账号 #3 · 剩余 42s · 已自动切换</div></div><button class="btn sm">查看</button></div>
            <div class="row" style="align-items:flex-start"><span class="avatar" style="color:var(--info);border-color:rgba(124,199,217,.3);background:var(--info-dim)">↻</span><div class="grow"><div class="nm" style="font-size:12px">1 个模板有更新</div><div class="sub">deepseek · v1 → v2 · 新增模型目录</div></div><button class="btn sm">更新</button></div>
          </div>
        </div>
        <div class="box"><div class="box-head"><h2>用量快照</h2><span class="hint">今日</span></div><div style="padding:12px 14px"><div style="display:flex;align-items:flex-end;gap:10px"><div><div class="hint" style="font-size:9.5px">TOKEN</div><div class="mono" style="font-size:24px;letter-spacing:-.03em">9.2M</div></div><div style="margin-left:auto;text-align:right"><div class="hint" style="font-size:9.5px">费用</div><div class="mono" style="font-size:16px">¥12.84</div></div>${sparkline([12,15,14,19,22,20,26,31],'var(--signal)',70,26)}</div><div style="display:flex;gap:8px;margin-top:10px"><button class="btn sm primary" style="flex:1">打开用量分析</button><button class="btn sm">导出</button></div></div></div>
        <div class="box"><div class="box-head"><h2>提示词审查</h2><span class="badge info">默认脱敏</span></div><div style="padding:12px 14px"><div style="display:flex;align-items:baseline;gap:8px"><div class="mono" style="font-size:22px">126</div><div class="hint">条记录 / 24h</div><div style="margin-left:auto">${badge('3 条异常','warn')}</div></div><div class="help" style="margin-top:6px">最近一次扫描 10:38 · 未记录响应体</div><button class="btn sm" style="width:100%;margin-top:10px">打开提示词审查 →</button></div></div>
      </div>
    </div>
  `),'02 · 总览');
}

function sUsage(){
  return board('b03','',shell('usage',`
    <div class="page-head"><div><h2 class="t">用量分析</h2><div class="s">先回答“花了多少、花在哪、哪里异常”，再展开到单条请求。</div></div><div class="actions"><button class="btn">导出 CSV</button><button class="btn">导出 JSON</button></div></div>
    <div class="scope"><div class="group"><span class="hint mono" style="font-size:9px">RANGE</span><button class="chip on">今日</button><button class="chip">昨日</button><button class="chip">24h</button><button class="chip">7d</button><button class="chip">30d</button><button class="chip">全部</button></div><div class="group"><span class="hint mono" style="font-size:9px">UPSTREAM</span><button class="chip">全部上游</button><button class="chip on">DeepSeek</button></div><div class="group"><span class="hint mono" style="font-size:9px">MODEL</span><button class="chip">全部模型</button></div><div class="group"><button class="chip">自动刷新 30s</button></div><div class="spacer"></div><button class="btn sm ghost">清除筛选</button></div>
    <div class="kpis five">
      ${kpi('请求数','1,284','↑ 12.4%','up',[18,22,20,26,24,31],'REQ')}
      ${kpi('总 Token','9.2M','↑ 8.1%','up',[12,15,14,19,22,20],'TOK')}
      ${kpi('成功率','99.4%','↑ 0.3%','up',[96,97,96,98,97,99],'OK')}
      ${kpi('估算费用','¥12.84','↑ ¥1.20','down',[8,10,9,12,13,12],'COST')}
      ${kpi('P95 耗时','820<small>ms</small>','↓ 60ms','up',[110,104,98,92,88,84],'LAT')}
    </div>
    <div class="grid-2" style="grid-template-columns:minmax(0,1fr) 330px;margin-bottom:12px">
      <div class="box"><div class="box-head"><h2>趋势</h2><div class="legend" style="margin-left:12px"><span><i style="background:var(--signal)"></i>请求</span><span><i style="background:var(--info)"></i>Token</span></div><div class="seg" style="margin-left:auto"><button class="active">折线</button><button>柱状</button></div><button class="btn sm ghost" style="margin-left:6px">请求</button></div><div style="padding:8px 10px 0">${areaChart({w:790,h:188,series:[{data:[42,38,35,32,30,36,48,62,78,88,96,92,104,112,108,116,122,118,126,131,128,136,142,148],color:'var(--signal)',fill:true},{data:[22,20,18,16,15,18,24,30,38,44,48,46,52,56,54,58,61,59,63,65,64,68,71,74],color:'var(--info)'}],labels:['00','02','04','06','08','10','12','14','16','18','20','22','now'],ymax:160})}</div></div>
      <div class="box"><div class="box-head"><h2>构成</h2><span class="hint">按上游</span></div><div style="padding:12px;display:flex;gap:12px;align-items:center"><div style="position:relative">${donut([{v:.46,c:'var(--signal)'},{v:.24,c:'var(--info)'},{v:.18,c:'var(--violet)'},{v:.12,c:'var(--amber)'}],98,11)}<div style="position:absolute;inset:0;display:grid;place-items:center;text-align:center"><div><div class="mono" style="font-size:15px">1,284</div><div class="hint" style="font-size:8px">REQ</div></div></div></div><div style="flex:1;font-size:10.5px;color:var(--muted);display:flex;flex-direction:column;gap:6px">${[['DeepSeek','46%','var(--signal)'],['Anthropic','24%','var(--info)'],['ZCode','18%','var(--violet)'],['其他','12%','var(--amber)']].map(x=>`<div style="display:flex;align-items:center"><i style="width:6px;height:6px;border-radius:2px;background:${x[2]};margin-right:6px"></i>${x[0]}<span class="mono" style="margin-left:auto">${x[1]}</span></div>`).join('')}</div></div><div style="border-top:1px solid var(--line);padding:8px 12px;display:flex;gap:14px;font-size:10px;color:var(--muted)"><span>缓存命中 <b class="mono" style="color:var(--text)">31%</b></span><span>平均 1.2s</span><span>错误 7</span></div></div>
    </div>
    <div class="table-wrap"><table><thead><tr><th>时间</th><th>模型</th><th>上游</th><th class="right">Token</th><th class="right">费用</th><th class="right">耗时</th><th>状态</th><th>会话</th></tr></thead><tbody>
      ${[['10:42:18','deepseek-v4.1-flash','deepseek','8,214','¥0.021','820ms','200','sess_8f2a'],['10:41:52','claude-sonnet-4.5','anthropic','12,806','¥0.184','1.4s','200','sess_2c91'],['10:40:31','glm-5.3-flash','zcode','5,102','—','1.2s','200','sess_77be'],['10:39:07','deepseek-v4.1-flash','deepseek','3,884','¥0.010','640ms','200','sess_8f2a'],['10:38:44','qwen3-max','dashscope','9,442','—','2.1s','429','sess_a310'],['10:37:20','gpt-5.2','openrouter','16,208','¥0.226','3.8s','200','sess_c842'],['10:36:02','deepseek-v4.1-flash','deepseek','2,116','¥0.006','510ms','200','sess_11ac']].map(r=>`<tr><td class="mono" style="font-size:11px">${r[0]}</td><td><div class="name" style="font-size:11.5px">${r[1]}</div></td><td>${r[2]}</td><td class="right mono">${r[3]}</td><td class="right mono">${r[4]}</td><td class="right mono">${r[5]}</td><td>${r[6]==='200'?badge('200','ok'):badge(r[6],'warn')}</td><td class="mono" style="font-size:10px;color:var(--faint)">${r[7]}</td></tr>`).join('')}
    </tbody></table></div>
    <div class="scrim"></div>
    <aside class="drawer">
      <div class="drawer-head"><div class="row1"><h2>请求详情</h2>${badge('200','ok')}<span class="sub" style="margin-left:auto">req_01J8K3F2</span><button class="icon-btn" style="width:26px;height:26px">×</button></div><div class="sub">2026-09-12 10:42:18 · deepseek / deepseek-v4.1-flash · sess_8f2a</div></div>
      <div class="tabs"><button class="active">概览</button><button>时序</button><button>请求</button><button>响应</button><button>重试链</button></div>
      <div class="drawer-body">
        <div class="kpis" style="grid-template-columns:repeat(3,1fr);margin-bottom:12px">${kpi('Token','8,214','输入 6,102 / 输出 2,112','',null,'')}${kpi('费用','¥0.021','缓存命中 31%','',null,'')}${kpi('耗时','820<small>ms</small>','P95 840ms','',null,'')}</div>
        <div class="section-title">时序瀑布</div>
        <div class="box" style="padding:12px 14px">
          ${[['排队','12ms',8,'var(--line2)'],['首 Token','214ms',26,'var(--info)'],['生成','594ms',66,'var(--signal)']].map(x=>`<div style="display:flex;align-items:center;gap:10px;margin-bottom:9px"><span class="hint" style="width:52px">${x[0]}</span><div style="flex:1;height:6px;background:var(--surface3);border-radius:4px;overflow:hidden"><i style="display:block;width:${x[2]}%;height:100%;background:${x[3]};border-radius:4px"></i></div><span class="mono" style="font-size:10px;width:46px;text-align:right">${x[1]}</span></div>`).join('')}
        </div>
        <div class="section-title">Token 与费用</div>
        <div class="kv"><span class="k">输入</span><span class="v mono">6,102 · 缓存命中 1,892</span><span class="k">输出</span><span class="v mono">2,112</span><span class="k">单价</span><span class="v mono">¥0.50 / ¥2.00 每百万</span><span class="k">计费</span><span class="v mono">¥0.00305 + ¥0.00422 = ¥0.00727</span></div>
        <div class="banner info" style="margin-top:10px"><span>ⓘ</span><div>费用按调用时单价快照计算；后续改价不会改写历史。</div></div>
        <div class="section-title">重试链</div>
        <div class="box" style="padding:10px 12px;font-size:11px;color:var(--muted)"><div style="display:flex;gap:8px;align-items:center"><span class="dot ok"></span><b style="color:var(--text)">deepseek</b><span>第 1 次尝试成功</span><span class="mono" style="margin-left:auto">820ms</span></div></div>
        <div class="section-title">脱敏请求头</div>
        <div class="code">authorization: Bearer ••••••••
x-bridge-upstream: deepseek
x-bridge-upstream-model: deepseek-v4.1-flash
content-type: application/json</div>
      </div>
      <div class="drawer-foot"><button class="btn sm">复制请求 ID</button><button class="btn sm">查看会话</button><span class="spacer"></span><button class="btn sm primary">打开提示词 →</button></div>
    </aside>
  `),'03 · 用量分析');
}

function sPrompts(){
  const rows=[
    ['10:42','sess_8f2a','deepseek-v4.1-flash',18,'12.4k','帮我重构这个 Rust 模块的错误处理，要求…',false],
    ['10:41','sess_2c91','claude-sonnet-4.5',6,'4.8k','Review this migration and list breaking changes',false],
    ['10:38','sess_a310','qwen3-max',24,'18.2k','总结这份合同的风险点，并给出修改建议',true],
    ['10:34','sess_77be','glm-5.3-flash',9,'6.1k','为这个接口生成 OpenAPI 3.1 文档',false],
    ['10:29','sess_c842','gpt-5.2',3,'2.2k','解释这段 SQL 为什么走不到索引',false]
  ];
  return board('b04','',shell('prompts',`
    <div class="page-head"><div><h2 class="t">提示词</h2><div class="s">审查请求内容、比较会话演进、评估工具执行；默认脱敏，不记录响应体。</div></div><div class="actions"><button class="btn">清理与保留</button><button class="btn primary">导出所选</button></div></div>
    <div class="banner info" style="margin-bottom:11px"><span>◈</span><div><b>记录已开启 · 默认脱敏 · 保留 7 天。</b> 仅记录请求消息与工具调用；Authorization、Cookie、API Key 在落库前替换为掩码。 <span class="mono" style="margin-left:6px">去设置 →</span></div></div>
    <div style="display:grid;grid-template-columns:minmax(0,1fr) 470px;gap:12px;height:calc(100% - 132px)">
      <div style="display:flex;flex-direction:column;min-height:0">
        <div class="toolbar"><div class="field" style="min-width:230px"><span class="mono" style="font-size:10px;color:var(--faint)">/</span><input placeholder="搜索提示词内容、会话 ID…"></div><select class="select"><option>最近 24 小时</option></select><select class="select"><option>全部模型</option></select><div class="right"><span class="hint">126 条</span><div class="seg"><button class="active">列表</button><button>会话</button></div></div></div>
        <div class="table-wrap" style="flex:1;min-height:0"><table><thead><tr><th>时间</th><th>会话</th><th>模型</th><th class="right">消息</th><th class="right">字符</th><th>预览</th><th></th></tr></thead><tbody>
          ${rows.map((r,i)=>`<tr class="${i===0?'selected':''}"><td class="mono" style="font-size:10.5px">${r[0]}</td><td class="mono" style="font-size:10px;color:var(--muted)">${r[1]}</td><td style="font-size:11px">${r[2]}</td><td class="right mono">${r[3]}</td><td class="right mono">${r[4]}</td><td style="font-size:11px;color:var(--muted);max-width:250px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">${r[5]}</td><td>${r[6]?badge('异常','warn'):'<span style="color:var(--faint)">›</span>'}</td></tr>`).join('')}
        </tbody></table></div>
        <div style="display:flex;align-items:center;gap:10px;padding-top:9px" class="hint"><span>显示 1–5 / 126</span><span style="margin-left:auto">保留至 2026-09-19</span><button class="btn sm ghost">上一页</button><button class="btn sm ghost">下一页</button></div>
      </div>
      <div class="box" style="display:flex;flex-direction:column;min-height:0">
        <div class="drawer-head" style="padding:14px 16px 10px"><div class="row1"><h2 style="font-size:15px">sess_8f2a · 第 18 轮</h2>${badge('已脱敏','info')}<span class="actions"><button class="icon-btn" style="width:25px;height:25px">×</button></span></div><div class="sub">2026-09-12 10:42:18 · deepseek-v4.1-flash · 12.4k 字符</div></div>
        <div class="tabs" style="padding:0 10px"><button class="active">消息</button><button>会话演进</button><button>工具评估</button></div>
        <div style="flex:1;overflow:hidden;padding:12px 14px">
          <div style="display:flex;flex-direction:column;gap:9px">
            <div style="border:1px solid var(--line);border-radius:9px;padding:9px 10px;background:var(--surface2)"><div style="display:flex;align-items:center;gap:6px;margin-bottom:5px">${badge('system','gray')}<span class="hint mono" style="margin-left:auto">1,204 字符</span></div><div style="font-size:11px;color:var(--muted);line-height:1.65">You are a senior Rust engineer. Review the provided module and… <span style="color:var(--faint)">展开</span></div></div>
            <div style="border:1px solid var(--line);border-radius:9px;padding:9px 10px;background:var(--surface2)"><div style="display:flex;align-items:center;gap:6px;margin-bottom:5px">${badge('user','blue')}<span class="hint mono" style="margin-left:auto">2,842 字符</span></div><div style="font-size:11px;color:var(--muted);line-height:1.65">帮我重构这个 Rust 模块的错误处理。要求：<br>1. 保留现有公开 API；<br>2. 用 thiserror 统一错误类型；<br>3. 不引入 panic… <span style="color:var(--faint)">展开</span></div></div>
            <div style="border:1px solid var(--line);border-radius:9px;padding:9px 10px;background:var(--surface2)"><div style="display:flex;align-items:center;gap:6px;margin-bottom:5px">${badge('assistant','ok')}<span class="hint mono" style="margin-left:auto">7,108 字符</span><span class="badge violet">含工具调用</span></div><div style="font-size:11px;color:var(--muted);line-height:1.65">我先读取模块与测试，再给出分步改动。 <span style="color:var(--faint)">展开 3 个工具调用</span></div></div>
            <div style="border:1px solid var(--line);border-radius:9px;padding:9px 10px;background:var(--surface2)"><div style="display:flex;align-items:center;gap:6px;margin-bottom:5px">${badge('tool','violet')}<span class="hint mono" style="margin-left:auto">1,246 字符</span><span class="badge warn">结果被截断</span></div><div class="mono" style="font-size:10px;color:var(--muted);line-height:1.6">read_file("src/error.rs") → 412 lines</div></div>
          </div>
          <div class="section-title" style="margin-top:14px">工具执行评估</div>
          <div class="box" style="padding:8px 10px"><div style="display:grid;grid-template-columns:1fr auto auto auto;gap:8px;font-size:10.5px;color:var(--muted)"><span>read_file</span><span class="mono">3</span><span class="mono">0 未匹配</span><span class="badge ok">正常</span><span>grep</span><span class="mono">1</span><span class="mono">1 重复</span><span class="badge warn">注意</span></div></div>
        </div>
        <div class="drawer-foot" style="padding:9px 14px"><button class="btn sm">下载 JSON</button><button class="btn sm">复制 Markdown</button><span class="spacer"></span><button class="btn sm danger">清空此会话</button></div>
      </div>
    </div>
  `),'04 · 提示词');
}

function sAccess(){
  const items=[
    {name:'DeepSeek 官方',id:'deepseek',kind:'官方',kindCls:'ok',url:'https://api.deepseek.com',protocol:'OpenAI 兼容 · 8 模型',models:'8',cred:'ok',health:'正常',healthCls:'ok',p95:'820ms',today:'842',rate:'99.6%',default:true},
    {name:'Anthropic 官方',id:'anthropic',kind:'官方',kindCls:'ok',url:'https://api.anthropic.com',protocol:'Anthropic 原生 · 5 模型',models:'5',cred:'ok',health:'正常',healthCls:'ok',p95:'1.1s',today:'126',rate:'100%'},
    {name:'ZCode 账号池',id:'zcode',kind:'账号池',kindCls:'violet',url:'https://zcode.ai/api/v1',protocol:'Anthropic 兼容 · 3 模型',models:'3',cred:'pool',pool:'2 / 4',health:'冷却 1',healthCls:'warn',p95:'1.4s',today:'216',rate:'97.2%'},
    {name:'My Gateway',id:'my-gateway',kind:'自定义',kindCls:'info',url:'https://gateway.example.com/v1',protocol:'OpenAI 兼容 · 6 模型',models:'6',cred:'missing',health:'缺 Key',healthCls:'err',p95:'—',today:'0',rate:'—'},
    {name:'OpenRouter Free',id:'openrouter',kind:'免费',kindCls:'warn',url:'https://openrouter.ai/api/v1',protocol:'OpenAI 兼容 · 14 模型',models:'14',cred:'ok',health:'限流',healthCls:'warn',p95:'2.3s',today:'100',rate:'92.8%'},
    {name:'CommandCode',id:'commandcode',kind:'插件',kindCls:'blue',url:'https://api.commandcode.ai',protocol:'OpenAI 兼容 · 3 模型',models:'3',cred:'ok',health:'正常',healthCls:'ok',p95:'980ms',today:'0',rate:'—'},
    {name:'DashScope',id:'dashscope',kind:'官方',kindCls:'ok',url:'https://dashscope.aliyuncs.com',protocol:'OpenAI 兼容 · 9 模型',models:'9',cred:'ok',health:'未定价',healthCls:'warn',p95:'2.1s',today:'0',rate:'—'}
  ];
  return board('b05','',shell('access',`
    <div class="page-head"><div><h2 class="t">接入管理</h2><div class="s">官方、自定义、免费来源、账号池和插件，统一列表、统一抽屉、统一健康口径。</div></div><div class="actions"><button class="btn">连通性自检</button><button class="btn">导出</button><button class="btn primary">＋ 添加接入</button></div></div>
    <div class="toolbar">
      <div class="field" style="min-width:250px"><span class="mono" style="font-size:10px;color:var(--faint)">/</span><input placeholder="搜索名称、地址或模型…"><kbd style="font-family:var(--mono);font-size:9px;border:1px solid var(--line2);padding:1px 4px;border-radius:3px;color:var(--faint)">/</kbd></div>
      <select class="select"><option>全部来源</option><option>官方</option><option>自定义</option><option>免费</option><option>账号池</option><option>插件</option></select>
      <select class="select"><option>全部健康状态</option><option>正常</option><option>需关注</option><option>不可用</option></select>
      <select class="select"><option>最近使用</option><option>名称 A–Z</option><option>请求数</option></select>
      <div class="right"><span class="hint">7 个接入 · 5 正常</span><div class="seg"><button class="active">列表</button><button>卡片</button></div><button class="icon-btn" style="width:29px;height:29px" title="列配置">☰</button></div>
    </div>
    <div class="table-wrap"><table><thead><tr><th style="width:30px">${cb()}</th><th>接入</th><th>类型</th><th>协议 / 地址</th><th class="right">模型</th><th>凭据</th><th>健康</th><th class="right">今日</th><th class="right" style="width:92px">操作</th></tr></thead><tbody>
      ${items.map(a=>accessRow(a)).join('')}
    </tbody></table></div>
    <div style="display:flex;align-items:center;gap:10px;padding-top:10px" class="hint"><span>显示 1–7 / 7</span><span style="margin-left:auto">最近刷新 10:42:18</span><button class="btn sm ghost">刷新</button></div>
    <div style="position:absolute;left:50%;bottom:22px;transform:translateX(-50%);background:var(--surface3);border:1px solid var(--line2);border-radius:10px;padding:8px 10px 8px 14px;display:flex;align-items:center;gap:8px;box-shadow:var(--shadow);z-index:10"><b style="font-size:11.5px">已选 2 项</b><span class="hint">deepseek · anthropic</span><span style="width:1px;height:18px;background:var(--line2);margin:0 4px"></span><button class="btn sm">启用</button><button class="btn sm">禁用</button><button class="btn sm">测试</button><button class="btn sm danger">删除</button><button class="icon-btn" style="width:24px;height:24px">×</button></div>
  `),'05 · 接入管理');
}

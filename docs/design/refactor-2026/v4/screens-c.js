/* Screens 12–17 */
function sEntryRoutes(){
  const upstreams=[['DeepSeek 官方','deepseek',8,true],['Anthropic 官方','anthropic',5,true],['ZCode 账号池','zcode',3,true],['OpenRouter Free','openrouter',14,false],['DashScope','dashscope',9,false],['CommandCode','commandcode',3,true]];
  return board('b12','',shell('entry',`
    <div class="page-head"><div><h2 class="t">模型与路由 · 统一入口</h2><div class="s">统一入口只暴露允许展示的上游；命名分组进一步收窄范围。路由组按成员顺序失败切换。</div></div><div class="actions"><button class="btn">复制 /unified/v1</button><button class="btn primary">保存变更</button></div></div>
    <div style="display:grid;grid-template-columns:minmax(0,1fr) minmax(0,1.05fr);gap:12px;height:calc(100% - 96px)">
      <div class="stack">
        <div class="box"><div class="box-head"><h2>可见上游</h2><span class="hint">统一入口默认返回全部可见项</span><span class="badge ok" style="margin-left:auto">4 / 6 开启</span></div><div>
          ${upstreams.map(u=>`<div class="row" style="padding:9px 14px"><div class="grow"><div class="nm" style="font-size:11.5px">${u[0]}</div><div class="sub mono">${u[1]} · ${u[2]} 个统一模型</div></div><span class="badge ${u[3]?'ok':'gray'}">${u[3]?'可见':'隐藏'}</span><div style="width:30px;height:16px;border-radius:9px;background:${u[3]?'var(--signal)':'var(--surface3)'};position:relative;flex:none"><span style="position:absolute;top:2px;${u[3]?'right:2px':'left:2px'};width:12px;height:12px;border-radius:50%;background:${u[3]?'#07130C':'var(--faint)'}"></span></div></div>`).join('')}
        </div></div>
        <div class="box"><div class="box-head"><h2>命名分组</h2><span class="hint">路径中的 {group} 只影响统一入口</span><button class="btn sm" style="margin-left:auto">＋ 新建分组</button></div><div style="padding:10px 12px;display:flex;flex-direction:column;gap:8px">
          <div style="border:1px solid var(--line);border-radius:9px;padding:9px 10px;background:var(--surface2)"><div style="display:flex;align-items:center;gap:7px"><b style="font-size:11.5px">production</b>${badge('3 上游','gray')}<span class="mono hint" style="margin-left:auto">/unified/production/v1</span><button class="btn sm ghost">复制</button></div><div class="chips" style="margin-top:6px"><span class="chip on">deepseek</span><span class="chip on">anthropic</span><span class="chip on">zcode</span></div></div>
          <div style="border:1px solid var(--line);border-radius:9px;padding:9px 10px;background:var(--surface2)"><div style="display:flex;align-items:center;gap:7px"><b style="font-size:11.5px">free-only</b>${badge('2 上游','gray')}<span class="mono hint" style="margin-left:auto">/unified/free-only/v1</span><button class="btn sm ghost">复制</button></div><div class="chips" style="margin-top:6px"><span class="chip on">openrouter</span><span class="chip">dashscope</span><span class="hint">+ 只有通过可见白名单才生效</span></div></div>
        </div></div>
      </div>
      <div class="stack">
        <div class="box"><div class="box-head"><h2>路由组</h2><span class="hint">有序成员 · 401/403/404/普通 400 不切换</span><button class="btn sm" style="margin-left:auto">＋ 新建</button></div><div>
          <div style="padding:11px 13px;border-bottom:1px solid var(--line)"><div style="display:flex;align-items:center;gap:7px"><b style="font-size:12px">生产对话</b>${badge('group/prod','ok')}<span class="hint" style="margin-left:auto">3 成员 · 今日 842</span></div>
            ${[['1','deepseek / deepseek-v4.1-flash','官方','820ms','ok'],['2','my-gateway / deepseek-v4.1-flash','自定义','1.2s','warn'],['3','openrouter / gpt-5.2','免费','3.8s','ok']].map(m=>`<div style="display:flex;align-items:center;gap:8px;padding:7px 0;border-top:1px solid rgba(36,48,43,.45)"><span class="mono hint" style="width:14px">${m[0]}</span><span style="font-size:11px;flex:1;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">${m[1]}</span>${badge(m[2],m[2]==='官方'?'ok':m[2]==='自定义'?'info':'warn')}<span class="mono hint" style="width:48px;text-align:right">${m[3]}</span><span class="dot ${m[4]}"></span><span style="color:var(--faint)">⠿</span></div>`).join('')}
            <div style="display:flex;gap:5px;margin-top:6px"><button class="btn sm ghost">↑</button><button class="btn sm ghost">↓</button><button class="btn sm ghost">编辑成员</button><button class="btn sm ghost" style="margin-left:auto">测试链路</button></div>
          </div>
          <div style="padding:11px 13px"><div style="display:flex;align-items:center;gap:7px"><b style="font-size:12px">低价优先</b>${badge('group/cheap','ok')}<span class="hint" style="margin-left:auto">2 成员 · 今日 100</span></div><div style="display:flex;align-items:center;gap:8px;padding:7px 0"><span class="mono hint" style="width:14px">1</span><span style="font-size:11px;flex:1">openrouter / gpt-5.2</span>${badge('免费','warn')}<span class="mono hint">3.8s</span><span class="dot ok"></span><span style="color:var(--faint)">⠿</span></div><div style="display:flex;align-items:center;gap:8px;padding:7px 0;border-top:1px solid rgba(36,48,43,.45)"><span class="mono hint" style="width:14px">2</span><span style="font-size:11px;flex:1">dashscope / qwen3-max</span>${badge('官方','ok')}<span class="mono hint">2.1s</span><span class="dot warn"></span><span style="color:var(--faint)">⠿</span></div></div>
        </div></div>
        <div class="box"><div class="box-head"><h2>生效预览</h2><span class="hint">group/prod</span></div><div style="padding:11px 13px"><div class="input" style="margin-bottom:9px"><span class="mono" style="font-size:11px">deepseek-v4.1-flash</span><span class="hint" style="margin-left:auto">请求模型名</span></div>
          <div style="font-size:10.5px;color:var(--muted);line-height:1.9"><div style="display:flex;gap:7px"><span class="mono" style="color:var(--signal)">1</span><span>deepseek / deepseek-v4.1-flash</span><span class="badge ok" style="margin-left:auto">首选</span></div><div style="display:flex;gap:7px"><span class="mono">2</span><span>my-gateway / deepseek-v4.1-flash</span><span class="badge warn" style="margin-left:auto">缺 Key · 跳过</span></div><div style="display:flex;gap:7px"><span class="mono">3</span><span>openrouter / gpt-5.2</span><span class="badge" style="margin-left:auto">最后兜底</span></div></div>
          <div class="banner" style="margin-top:9px"><span>ⓘ</span><div>401 / 403 / 404 / 普通 400 不切换；仅 429、5xx、超时进入下一成员。</div></div></div></div>
      </div>
    </div>
  `),'12 · 统一入口与路由组');
}

function sPools(){
  return board('b13','',shell('pools',`
    <div class="page-head"><div><h2 class="t">配置中心 · 凭据池</h2><div class="s">同上游多 Key 的优先级、轮换、冷却与配额；密钥只显示掩码，冷却原因可解释。</div></div><div class="actions"><button class="btn">领取记录</button><button class="btn primary">＋ 新建池</button></div></div>
    <div class="kpis">${kpi('池','3','绑定 3 个上游','',null,'POOLS')}${kpi('密钥','11','可用 9 / 冷却 1 / 禁用 1','',null,'KEYS')}${kpi('今日请求','1,284','池承载 42%','up',null,'REQ')}${kpi('配额耗尽','0','最近 7 天','',null,'QUOTA')}</div>
    <div style="display:grid;grid-template-columns:1fr 1fr;gap:12px">
      <div class="box">
        <div class="box-head"><h2>zcode-main</h2>${badge('优先级','gray')}${badge('绑定 zcode','violet')}<span class="hint" style="margin-left:auto">2 / 4 可用</span><button class="btn sm" style="margin-left:6px">编辑</button></div>
        <div style="padding:10px 13px;display:flex;align-items:center;gap:10px;border-bottom:1px solid var(--line)"><div style="flex:1"><div class="hint" style="font-size:9.5px">今日请求</div><div class="mono" style="font-size:15px">216</div></div><div style="flex:1"><div class="hint" style="font-size:9.5px">平均延迟</div><div class="mono" style="font-size:15px">1.2s</div></div><div style="flex:1"><div class="hint" style="font-size:9.5px">失败阈值</div><div class="mono" style="font-size:15px">3 次 / 60s</div></div><span class="badge warn">1 冷却</span></div>
        <table><thead><tr><th>#</th><th>成员</th><th>状态</th><th class="right">配额</th><th>冷却至</th><th>最近错误</th><th></th></tr></thead><tbody>
          <tr><td class="mono hint">1</td><td><div class="name" style="font-size:11px">zcode#1</div><div class="sub mono">sk-••••9f2a</div></td><td>${st('可用','ok')}</td><td class="right mono">72%</td><td class="hint">—</td><td class="hint">—</td><td><button class="icon-btn" style="width:22px;height:22px">⋯</button></td></tr>
          <tr><td class="mono hint">2</td><td><div class="name" style="font-size:11px">zcode#2</div><div class="sub mono">sk-••••a81c</div></td><td>${st('可用','ok')}</td><td class="right mono">48%</td><td class="hint">—</td><td class="hint">—</td><td><button class="icon-btn" style="width:22px;height:22px">⋯</button></td></tr>
          <tr><td class="mono hint">3</td><td><div class="name" style="font-size:11px">zcode#3</div><div class="sub mono">sk-••••3d07</div></td><td>${st('冷却中','warn')}</td><td class="right mono">12%</td><td class="mono" style="font-size:10px;color:var(--amber)">42s</td><td class="hint">429</td><td><button class="icon-btn" style="width:22px;height:22px">⋯</button></td></tr>
          <tr><td class="mono hint">4</td><td><div class="name" style="font-size:11px">zcode#4</div><div class="sub mono">sk-••••7e11</div></td><td>${st('已禁用','idle')}</td><td class="right mono">—</td><td class="hint">—</td><td class="hint">手动</td><td><button class="icon-btn" style="width:22px;height:22px">⋯</button></td></tr>
        </tbody></table>
        <div style="padding:9px 13px;display:flex;align-items:center;gap:6px;border-top:1px solid var(--line)"><span class="hint">下次冷却恢复</span><span class="mono" style="font-size:10px;color:var(--amber)">10:43:00</span><button class="btn sm ghost" style="margin-left:auto">重置冷却</button><button class="btn sm ghost">探测全部</button></div>
      </div>
      <div class="stack">
        <div class="box">
          <div class="box-head"><h2>deepseek-keys</h2>${badge('轮询','gray')}${badge('绑定 deepseek','ok')}<span class="hint" style="margin-left:auto">3 / 3 可用</span><button class="btn sm" style="margin-left:6px">编辑</button></div>
          <div style="padding:10px 13px;display:flex;align-items:center;gap:10px;border-bottom:1px solid var(--line)"><div style="flex:1"><div class="hint" style="font-size:9.5px">今日请求</div><div class="mono" style="font-size:15px">842</div></div><div style="flex:1"><div class="hint" style="font-size:9.5px">平均延迟</div><div class="mono" style="font-size:15px">820ms</div></div><div style="flex:1"><div class="hint" style="font-size:9.5px">轮换</div><div class="mono" style="font-size:15px">每 5 分钟</div></div><span class="badge ok">全部可用</span></div>
          <table><thead><tr><th>#</th><th>成员</th><th>状态</th><th class="right">配额</th><th>最近使用</th><th></th></tr></thead><tbody>
            <tr><td class="mono hint">1</td><td><div class="name" style="font-size:11px">ds#1</div><div class="sub mono">sk-••••11ac</div></td><td>${st('可用','ok')}</td><td class="right mono">81%</td><td class="mono hint">10:42</td><td><button class="icon-btn" style="width:22px;height:22px">⋯</button></td></tr>
            <tr><td class="mono hint">2</td><td><div class="name" style="font-size:11px">ds#2</div><div class="sub mono">sk-••••88bd</div></td><td>${st('可用','ok')}</td><td class="right mono">64%</td><td class="mono hint">10:40</td><td><button class="icon-btn" style="width:22px;height:22px">⋯</button></td></tr>
            <tr><td class="mono hint">3</td><td><div class="name" style="font-size:11px">ds#3</div><div class="sub mono">sk-••••f220</div></td><td>${st('可用','ok')}</td><td class="right mono">33%</td><td class="mono hint">10:39</td><td><button class="icon-btn" style="width:22px;height:22px">⋯</button></td></tr>
          </tbody></table>
        </div>
        <div class="box"><div class="box-head"><h2>ZCode 账号</h2><span class="hint">领取 / OAuth / 验证码池</span><button class="btn sm" style="margin-left:auto">立即领取</button></div><div style="padding:10px 13px"><div style="display:flex;align-items:center;gap:10px"><div><div class="hint" style="font-size:9.5px">可用账号</div><div class="mono" style="font-size:16px">4</div></div><div><div class="hint" style="font-size:9.5px">今日领取</div><div class="mono" style="font-size:16px">1</div></div><div><div class="hint" style="font-size:9.5px">OAuth 状态</div><div style="font-size:11px;color:var(--signal)">已授权</div></div><span class="badge warn" style="margin-left:auto">1 冷却</span></div><div class="help" style="margin-top:7px">危险操作（清空账号、重置授权）独立区，需输入名称确认。</div></div></div>
      </div>
    </div>
  `),'13 · 凭据池');
}

function sResources(){
  const plugins=[['CommandCode','1.4.2','provider','OpenAI 兼容','3','ok'],['Kilo Free','0.9.8','free-source','14 个免费模型','14','ok'],['ZCode','2.1.0','account','GLM 传输','3','ok'],['OpenRouter','1.0.3','aggregator','聚合 14 模型','14','warn'],['Local Echo','0.2.0','tool','回显测试','0','idle']];
  return board('b14','',shell('resources',`
    <div class="page-head"><div><h2 class="t">配置中心 · 资源库</h2><div class="s">安装只登记，接入才创建上游；免费来源先探测再接入，模板可一键用于添加接入。</div></div><div class="actions"><button class="btn">导入插件</button><button class="btn primary">全部探测</button></div></div>
    <div class="tabs" style="padding:0;margin-bottom:12px;border-bottom:1px solid var(--line)"><button class="active" style="padding-left:2px">插件 <span class="badge gray" style="margin-left:4px">5</span></button><button>免费来源 <span class="badge warn" style="margin-left:4px">1</span></button><button>模板 <span class="badge gray" style="margin-left:4px">8</span></button></div>
    <div style="display:grid;grid-template-columns:minmax(0,1fr) 330px;gap:12px">
      <div>
        <div class="banner" style="margin-bottom:10px"><span>ⓘ</span><div><b>安装只登记，接入才创建上游。</b> 插件提供能力声明与鉴权方式；未接入的插件不会出现在路由与模型中。</div></div>
        <div style="display:grid;grid-template-columns:repeat(3,1fr);gap:10px">
          ${plugins.map(p=>`<div class="card" style="padding:13px;min-height:150px;display:flex;flex-direction:column"><div style="display:flex;align-items:center;gap:7px"><span class="avatar" style="width:28px;height:28px;color:var(--blue);border-color:rgba(143,184,255,.25);background:var(--blue-dim)">${p[0].slice(0,2).toUpperCase()}</span><div style="min-width:0"><div style="font-weight:600;font-size:12px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">${p[0]}</div><div class="hint mono" style="font-size:9px">v${p[1]}</div></div>${p[5]==='ok'?badge('已启用','ok'):p[5]==='warn'?badge('需关注','warn'):badge('未接入','gray')}</div><div class="chips" style="margin-top:9px"><span class="chip">${p[2]}</span><span class="chip">${p[3]}</span></div><div style="display:flex;align-items:center;gap:6px;margin-top:auto;padding-top:10px"><span class="hint">${p[4]} 个模型</span><button class="btn sm" style="margin-left:auto">${p[5]==='idle'?'接入':'管理'}</button></div></div>`).join('')}
        </div>
        <div class="box" style="margin-top:10px"><div class="box-head"><h2>插件契约</h2><span class="hint">capabilities / auth / models</span><span class="badge ok" style="margin-left:auto">5 / 5 通过</span></div><div style="padding:10px 13px;display:grid;grid-template-columns:1fr 1fr;gap:8px 16px;font-size:10.5px;color:var(--muted)"><span>provider 声明</span><span class="mono" style="color:var(--signal)">ok</span><span>鉴权方式</span><span class="mono" style="color:var(--signal)">ok</span><span>模型清单</span><span class="mono" style="color:var(--signal)">ok</span><span>能力标签</span><span class="mono" style="color:var(--signal)">ok</span></div></div>
      </div>
      <div class="stack">
        <div class="box"><div class="box-head"><h2>免费来源</h2><span class="hint">Kilo</span><span class="badge warn" style="margin-left:auto">1 限流</span></div><div style="padding:11px 13px"><div style="display:flex;align-items:baseline;gap:7px"><div class="mono" style="font-size:20px">14</div><div class="hint">可用模型 / 18</div><div style="margin-left:auto">${badge('中风险','warn')}</div></div><div class="help" style="margin-top:5px">最近探测 10:36 · 3 个拒因：区域限制 / 配额 / 模型下线</div><div class="meter" style="margin-top:8px"><i class="warn" style="width:72%"></i></div><button class="btn sm" style="width:100%;margin-top:9px">查看来源与模型 →</button></div></div>
        <div class="box"><div class="box-head"><h2>模板</h2><span class="hint">8 个 · 1 个更新</span></div><div>${[['DeepSeek','v2 · 新增目录','ok'],['Anthropic','v1 · 稳定','ok'],['OpenAI','v1 · 稳定','ok'],['DashScope','v3 · 新增模型','info']].map(t=>`<div class="row" style="padding:8px 13px"><div class="grow"><div class="nm" style="font-size:11px">${t[0]}</div><div class="sub">${t[1]}</div></div>${badge(t[2]==='ok'?'最新':'有更新',t[2])}<button class="btn sm ghost">用此模板</button></div>`).join('')}</div></div>
        <div class="banner warn"><span>!</span><div><b>免费来源有配额与区域限制。</b> 探测失败会自动退避 10 分钟；接入前先确认模型可用数与拒因。</div></div>
      </div>
    </div>
  `),'14 · 资源库');
}

function sSettings(){
  return board('b15','',shell('settings',`
    <div class="page-head"><div><h2 class="t">配置中心 · 设置与高级维护</h2><div class="s">配置文件是唯一事实源；保存前自动备份，revision 冲突时选择重新加载或覆盖。</div></div><div class="actions"><button class="btn">配置审计</button><button class="btn">导出诊断</button></div></div>
    <div style="display:grid;grid-template-columns:180px minmax(0,1fr) 270px;gap:12px;height:calc(100% - 96px)">
      <div class="box" style="padding:8px"><div class="hint" style="font-size:9px;letter-spacing:.1em;padding:6px 8px">设置</div>${[['服务与全局','active'],['隐私与记录',''],['AI 配置接口',''],['高级维护','']].map(x=>`<button class="btn sm ghost" style="width:100%;justify-content:flex-start;${x[1]?'background:var(--signal-dim);color:var(--text)':'margin-top:2px;color:var(--muted)'}">${x[0]}</button>`).join('')}<div class="divider"></div><div class="hint" style="font-size:9px;letter-spacing:.1em;padding:6px 8px">快捷</div><button class="btn sm ghost" style="width:100%;justify-content:flex-start;color:var(--muted)">原始 JSON</button><button class="btn sm ghost" style="width:100%;justify-content:flex-start;color:var(--muted)">备份与恢复</button><button class="btn sm ghost" style="width:100%;justify-content:flex-start;color:var(--muted)">检查更新</button></div>
      <div class="stack" style="min-height:0;overflow:hidden">
        <div class="box"><div class="box-head"><h2>服务与全局</h2><span class="hint">保存后立即生效，端口变更需重启</span></div><div style="padding:12px 14px">
          <div class="form-grid">
            <div class="form-row"><label>监听地址</label><div class="input"><span class="mono">127.0.0.1:8900</span></div><div class="help">修改端口后需重启进程。</div></div>
            <div class="form-row"><label>默认上游</label><div class="input"><span>deepseek</span><span style="margin-left:auto;color:var(--faint)">▾</span></div></div>
            <div class="form-row"><label>默认模型</label><div class="input"><span class="mono">deepseek-v4.1-flash</span></div></div>
            <div class="form-row"><label>请求超时</label><div class="input"><span class="mono">120s</span></div></div>
          </div>
          <div class="divider"></div>
          <div style="display:flex;align-items:center;gap:10px"><div><div style="font-size:12px;font-weight:550">保存前自动备份</div><div class="hint">保留最近 20 份，可在高级维护恢复</div></div><span style="margin-left:auto;width:30px;height:16px;border-radius:9px;background:var(--signal);position:relative"><span style="position:absolute;top:2px;right:2px;width:12px;height:12px;border-radius:50%;background:#07130C"></span></span></div>
          <div style="display:flex;align-items:center;gap:10px;margin-top:10px"><div><div style="font-size:12px;font-weight:550">统一入口</div><div class="hint">/unified/v1 与命名分组</div></div><span style="margin-left:auto">${badge('已开启','ok')}</span></div>
        </div></div>
        <div class="box"><div class="box-head"><h2>隐私与记录</h2><span class="hint">默认脱敏</span></div><div style="padding:12px 14px"><div class="form-grid"><div class="form-row"><label>提示词记录</label><div class="input"><span>开启</span><span style="margin-left:auto;color:var(--faint)">▾</span></div></div><div class="form-row"><label>保留天数</label><div class="input"><span class="mono">7</span></div></div><div class="form-row full"><label>脱敏规则</label><div class="chips"><span class="chip on">Authorization</span><span class="chip on">Cookie</span><span class="chip on">API Key</span><span class="chip on">email</span><span class="chip">＋ 自定义</span></div></div></div><div class="banner info" style="margin-top:9px"><span>◈</span><div>响应体不记录；提示词在落库前替换为掩码。</div></div></div></div>
      </div>
      <div class="stack">
        <div class="box"><div class="box-head"><h2>配置状态</h2></div><div style="padding:11px 13px"><div class="kv"><span class="k">revision</span><span class="v mono">cfg_01J8</span><span class="k">上次保存</span><span class="v">10:30:12</span><span class="k">备份</span><span class="v mono">12 份</span><span class="k">待重启</span><span class="v" style="color:var(--signal)">无</span></div><div class="divider"></div><div class="hint" style="margin-bottom:6px">最近变更</div><div style="font-size:10.5px;color:var(--muted);line-height:1.7"><div>10:30 · 模板 v2 自动应用</div><div>10:12 · 新增池成员</div><div>09:48 · qwen3-max 定价</div></div></div></div>
        <div class="box"><div class="box-head"><h2>危险区</h2><span class="badge err" style="margin-left:auto">二次确认</span></div><div style="padding:11px 13px"><div class="help">删除接入、清空凭据、恢复备份会中断服务。操作需输入对象名称确认。</div><button class="btn sm danger" style="width:100%;margin-top:9px">恢复备份…</button><button class="btn sm danger" style="width:100%;margin-top:5px">清空提示词记录…</button></div></div>
        <div class="banner"><span>⌘</span><div>保存失败会保留输入并定位首个错误；revision 冲突提供“重新加载 / 覆盖”。</div></div>
      </div>
    </div>
    <div style="position:absolute;left:224px;right:0;bottom:0;height:52px;background:var(--surface2);border-top:1px solid var(--line2);display:flex;align-items:center;gap:10px;padding:0 26px"><span class="dot warn"></span><b style="font-size:11.5px">3 项未保存</b><span class="hint">监听地址、保留天数、脱敏规则</span><span style="margin-left:auto" class="hint">保存前将创建备份 bak_01J9</span><button class="btn sm ghost">放弃</button><button class="btn sm primary">保存变更</button></div>
  `),'15 · 设置与高级维护');
}

function sStates(){
  const state=(cap,title,body)=>`<div class="state-card"><div class="cap">${cap}</div><div style="font-size:12px;font-weight:600;margin-bottom:6px">${title}</div>${body}</div>`;
  return board('b16','tall',shell('overview',`
    <div class="page-head"><div><h2 class="t">状态集合</h2><div class="s">空态、加载、错误、成功、无权限、revision 冲突。所有状态都给出下一步，不只说“暂无数据”。</div></div><div class="actions">${badge('6 种状态','gray')}${badge('对比度 ≥ 4.5:1','ok')}</div></div>
    <div style="display:grid;grid-template-columns:repeat(3,1fr);gap:12px">
      ${state('EMPTY · 空系统','还没有接入','<div class="empty" style="padding:18px 8px"><div class="big">◧</div><div style="font-size:11.5px;color:var(--text);margin-bottom:4px">从一个官方模板开始</div><div class="hint" style="line-height:1.6">只填 Key，30 秒完成第一个接入。<br>也可以导入 JSON 或从免费来源添加。</div><div style="display:flex;gap:6px;justify-content:center;margin-top:10px"><button class="btn sm primary">添加接入</button><button class="btn sm">导入 JSON</button></div></div>')}
      ${state('LOADING · 骨架屏','正在加载接入','<div style="padding:6px 0">${[1,2,3,4].map(()=>`<div style="display:flex;gap:8px;align-items:center;padding:7px 0"><div class="skel" style="width:24px;height:24px;border-radius:6px"></div><div style="flex:1"><div class="skel" style="width:60%;margin-bottom:5px"></div><div class="skel" style="width:38%;height:7px"></div></div><div class="skel" style="width:46px"></div></div>`).join("")}</div><div class="hint" style="margin-top:4px">超过 800ms 显示“仍在加载…”，保留旧数据时标记“已过期”。</div>')}
      ${state('ERROR · 行内错误','保存失败','<div class="banner err" style="margin-bottom:8px"><span>!</span><div><b>revision 冲突</b><div class="hint" style="margin-top:2px">配置已被其他窗口修改。</div></div></div><div class="code" style="margin-bottom:8px">expected cfg_01J8<br>actual&nbsp;&nbsp; cfg_01J9</div><div style="display:flex;gap:6px"><button class="btn sm">重新加载</button><button class="btn sm danger">覆盖</button><button class="btn sm ghost">复制诊断</button></div>')}
      ${state('SUCCESS · 轻提示','接入已添加','<div style="display:flex;flex-direction:column;gap:8px"><div class="banner" style="border-color:var(--signal-line);background:var(--signal-dim);color:var(--signal)"><span>✓</span><div><b style="color:var(--text)">DeepSeek 官方已接入</b><div class="hint" style="margin-top:2px">8 个模型 · 探测通过 · P95 820ms</div></div></div><div class="kv"><span class="k">下一步</span><span class="v">设为默认或加入路由组</span><span class="k">调用地址</span><span class="v mono">/deepseek/v1</span></div><div style="display:flex;gap:6px;margin-top:4px"><button class="btn sm">去设置默认</button><button class="btn sm ghost">再添加一个</button></div></div>')}
      ${state('NO PERMISSION · 无权限','需要管理员权限','<div class="empty" style="padding:18px 8px"><div class="big">⚿</div><div style="font-size:11.5px;color:var(--text);margin-bottom:4px">当前角色只能查看</div><div class="hint" style="line-height:1.6">保存、删除、密钥替换需要管理员。<br>可以复制诊断信息交给管理员。</div><div style="display:flex;gap:6px;justify-content:center;margin-top:10px"><button class="btn sm">复制诊断</button><button class="btn sm ghost">申请权限</button></div></div>')}
      ${state('STALE · 数据过期','显示的是缓存数据','<div class="banner warn" style="margin-bottom:8px"><span>◷</span><div><b>连接中断，数据可能已过期</b><div class="hint" style="margin-top:2px">最后更新 10:42:18 · 正在重试</div></div></div><div style="opacity:.55"><div class="row" style="padding:6px 0"><span class="dot ok"></span><div class="grow"><div class="nm" style="font-size:11px">DeepSeek 官方</div></div><span class="mono hint">842</span></div><div class="row" style="padding:6px 0"><span class="dot warn"></span><div class="grow"><div class="nm" style="font-size:11px">ZCode 账号池</div></div><span class="mono hint">216</span></div></div><button class="btn sm" style="margin-top:4px">立即重试</button>')}
    </div>
    <div class="section-title" style="margin-top:18px">INTERACTION RULES</div>
    <div class="grid-3">
      <div class="box" style="padding:12px 14px"><b style="font-size:12px">加载</b><div class="help" style="margin-top:4px">骨架屏优先；超过 800ms 显示文案；保留旧数据时标记“已过期”。</div></div>
      <div class="box" style="padding:12px 14px"><b style="font-size:12px">错误</b><div class="help" style="margin-top:4px">行内 banner + 重试 + 复制诊断；不整页报错，不丢失已填内容。</div></div>
      <div class="box" style="padding:12px 14px"><b style="font-size:12px">危险操作</b><div class="help" style="margin-top:4px">二次确认；删除接入/池/插件需输入名称；默认保留凭据。</div></div>
    </div>
  `),'16 · 状态集合');
}

function sMobile(){
  return board('b17','mobile',`<div class="app">
    <div class="mobile-top"><span class="mark" style="width:24px;height:24px;border:1px solid var(--signal-line);border-radius:7px;display:grid;place-items:center;color:var(--signal);background:var(--signal-dim);font-family:var(--mono);font-size:11px">◧</span><b style="font-size:13px">Model-Gate</b><span class="badge ok" style="margin-left:auto">运行中</span><button class="icon-btn" style="width:28px;height:28px">☰</button></div>
    <div class="mobile-body">
      <div style="padding:4px 2px 10px"><div style="font-size:17px;font-weight:650;letter-spacing:-.02em">今天，网关运行良好。</div><div class="hint" style="margin-top:3px">5 正常 · 1 缺 Key · 1 冷却</div></div>
      <div class="kpis" style="margin-bottom:10px">${kpi('今日请求','1,284','↑ 12.4%','up',[18,22,20,26,24,31],'')}${kpi('成功率','99.4%','↑ 0.3%','up',[96,97,96,98,97,99],'')}${kpi('Token','9.2M','6 模型','',null,'')}${kpi('费用','¥12.84','↑ ¥1.20','down',null,'')}</div>
      <div class="box" style="margin-bottom:10px"><div class="box-head"><h2>需要处理</h2><span class="badge warn" style="margin-left:auto">3</span></div><div><div class="row" style="align-items:flex-start;padding:8px 11px"><span class="avatar" style="width:20px;height:20px;font-size:9px;color:var(--danger);border-color:rgba(240,113,103,.3);background:var(--danger-dim)">!</span><div class="grow"><div class="nm" style="font-size:10.5px">My Gateway 缺少 Key</div><div class="sub">6 模型不可用</div></div><span style="color:var(--faint)">›</span></div><div class="row" style="align-items:flex-start;padding:8px 11px"><span class="avatar" style="width:20px;height:20px;font-size:9px;color:var(--amber);border-color:rgba(229,181,103,.3);background:var(--amber-dim)">◷</span><div class="grow"><div class="nm" style="font-size:10.5px">ZCode 池成员冷却</div><div class="sub">42s 后恢复</div></div><span style="color:var(--faint)">›</span></div></div></div>
      <div class="box" style="margin-bottom:10px"><div class="box-head" style="padding:9px 11px"><h2 style="font-size:11px">流量趋势</h2><span class="hint" style="margin-left:auto;font-size:9px">24h</span></div><div style="padding:6px 6px 0">${areaChart({w:340,h:110,series:[{data:[42,38,35,32,30,36,48,62,78,88,96,92,104,112,108,116,122,118,126,131,128,136,142,148],color:'var(--signal)',fill:true}],labels:['00','06','12','18','now'],ymax:160})}</div></div>
      <div class="box"><div class="box-head" style="padding:9px 11px"><h2 style="font-size:11px">接入健康</h2><span class="hint" style="margin-left:auto;font-size:9px">5 正常</span></div><div><div class="row" style="padding:7px 11px"><span class="dot ok"></span><div class="grow"><div class="nm" style="font-size:10.5px">DeepSeek 官方</div><div class="sub">842 · 99.6%</div></div><span class="mono hint">820ms</span></div><div class="row" style="padding:7px 11px"><span class="dot warn"></span><div class="grow"><div class="nm" style="font-size:10.5px">ZCode 账号池</div><div class="sub">216 · 97.2%</div></div><span class="mono hint">1.4s</span></div></div></div>
    </div>
    <div class="mobile-tabs"><button class="on"><span>⌂</span>总览</button><button><span>◔</span>用量</button><button><span>⇄</span>接入</button><button><span>≋</span>日志</button><button><span>⚙</span>配置</button></div>
  </div>`,'17 · 窄屏');
}

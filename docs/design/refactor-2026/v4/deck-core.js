/* Model-Gate v4 · design deck core */
const NAV = [
  { group: '工作台', items: [ { id:'overview', label:'总览', ico:'⌂' } ] },
  { group: '分析', items: [ { id:'usage', label:'用量分析', ico:'◔' }, { id:'prompts', label:'提示词', ico:'✎', count:'12' } ] },
  { group: '运行', items: [ { id:'access', label:'接入管理', ico:'⇄', count:'7' }, { id:'logs', label:'日志与诊断', ico:'≋', count:'3', alert:true } ] },
  { group: '配置', items: [ { id:'config', label:'配置中心', ico:'⚙' } ] }
];

const TITLES = {
  overview: ['总览','OVERVIEW'],
  usage: ['用量分析','USAGE'],
  prompts: ['提示词','PROMPTS'],
  access: ['接入管理','ACCESS'],
  logs: ['日志与诊断','LOGS & DIAGNOSTICS'],
  config: ['配置中心','CONFIGURATION'],
  models: ['模型与路由 · 模型目录','CONFIG / MODELS'],
  pricing: ['模型与路由 · 单价表','CONFIG / PRICING'],
  entry: ['模型与路由 · 统一入口','CONFIG / UNIFIED'],
  pools: ['凭据池','CONFIG / CREDENTIALS'],
  resources: ['资源库','CONFIG / RESOURCES'],
  settings: ['设置与高级维护','CONFIG / SETTINGS']
};

function sidebar(active){
  return `<aside class="sidebar">
    <div class="brand"><span class="mark">◧</span><span class="name">Model-Gate</span><span class="ver">v4.0</span></div>
    ${NAV.map(g=>`<div class="nav-group">${g.group}</div><div class="nav">${g.items.map(it=>`
      <button class="${it.id===active?'active':''}"><span class="ico">${it.ico}</span>${it.label}${it.count?`<span class="count ${it.alert?'alert':''}">${it.count}</span>`:''}</button>`).join('')}</div>`).join('')}
    <div class="side-foot">
      <div class="row"><span class="dot ok pulse"></span><span>运行中 · 127.0.0.1:8900</span></div>
      <div class="row"><span class="dot ok"></span><span>数据库正常 · 4h 15m</span></div>
      <div class="row"><span>revision</span><span class="mono" style="margin-left:auto;color:var(--muted)">cfg_01J8</span></div>
    </div>
  </aside>`;
}

function topbar(active, extra=''){
  const [t,k] = TITLES[active] || [active,active.toUpperCase()];
  return `<header class="topbar">
    <div class="crumb"><span class="k">MODEL-GATE</span><span style="color:var(--faint)">/</span><h1>${t}</h1><span class="badge gray" style="margin-left:2px">${k}</span></div>
    <div class="top-search"><span class="mono" style="font-size:10px;color:var(--faint)">/</span><input placeholder="搜索接入、模型、配置任务…"><kbd>⌘K</kbd></div>
    <button class="icon-btn">◐</button>
    ${extra || '<button class="btn primary">＋ 添加接入</button>'}
  </header>`;
}

function shell(active, body, opts={}){
  return `<div class="app">${sidebar(active)}<div class="main">${topbar(active, opts.extra)}<main class="content ${opts.scroll?'scroll':''}">${body}</main></div></div>`;
}

function board(id, cls, html, tag){
  return `<section class="board ${cls||''}" id="${id}" data-name="${tag||id}">${html}<span class="board-tag">${tag||id}</span></section>`;
}

/* charts */
let gid = 0;
function sparkline(pts, color='var(--signal)', w=62, h=22){
  const max=Math.max(...pts), min=Math.min(...pts), span=max-min||1;
  const step=w/(pts.length-1);
  const d=pts.map((p,i)=>`${i?'L':'M'}${(i*step).toFixed(1)},${(h-2-((p-min)/span)*(h-4)).toFixed(1)}`).join(' ');
  return `<svg class="spark" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}"><path d="${d}" fill="none" stroke="${color}" stroke-width="1.3"/></svg>`;
}

function areaChart({w=820,h=190,series,labels,ymax,unit=''}){
  const id='g'+(++gid);
  const pad={l:38,r:10,t:12,b:22};
  const iw=w-pad.l-pad.r, ih=h-pad.t-pad.b;
  const max=ymax||Math.max(...series.flatMap(s=>s.data))*1.15||1;
  const x=i=>pad.l+(i*(iw/(labels.length-1)));
  const y=v=>pad.t+ih-(v/max)*ih;
  const grid=[0,.25,.5,.75,1].map(p=>{
    const yy=pad.t+ih*p;
    return `<line x1="${pad.l}" y1="${yy}" x2="${w-pad.r}" y2="${yy}" stroke="var(--line)" stroke-width="1" stroke-dasharray="2 4"/><text x="${pad.l-7}" y="${yy+3}" text-anchor="end" class="axis">${Math.round(max*(1-p))}</text>`;
  }).join('');
  const paths=series.map((s,si)=>{
    const d=s.data.map((v,i)=>`${i?'L':'M'}${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(' ');
    const area=`${d} L${x(labels.length-1).toFixed(1)},${(pad.t+ih).toFixed(1)} L${x(0).toFixed(1)},${(pad.t+ih).toFixed(1)} Z`;
    return `${s.fill?`<path d="${area}" fill="url(#${id}-${si})"/>`:''}<path d="${d}" fill="none" stroke="${s.color}" stroke-width="1.7"/>`;
  }).join('');
  const defs=series.map((s,si)=>s.fill?`<linearGradient id="${id}-${si}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${s.color}" stop-opacity=".18"/><stop offset="1" stop-color="${s.color}" stop-opacity="0"/></linearGradient>`:'').join('');
  const xl=labels.map((l,i)=>`<text x="${x(i)}" y="${h-4}" text-anchor="middle" class="axis">${l}</text>`).join('');
  return `<svg class="chart" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none"><defs>${defs}</defs>${grid}${paths}${xl}</svg>`;
}

function bars({w=360,h=150,data,labels,color='var(--signal)'}){
  const pad={l:6,r:6,t:8,b:20};
  const iw=w-pad.l-pad.r, ih=h-pad.t-pad.b;
  const max=Math.max(...data)||1, bw=iw/data.length*.55;
  return `<svg class="chart" viewBox="0 0 ${w} ${h}"><line x1="0" y1="${h-pad.b}" x2="${w}" y2="${h-pad.b}" stroke="var(--line)"/>${data.map((v,i)=>{const bh=(v/max)*ih;const xx=pad.l+(i+.5)*(iw/data.length)-bw/2;return `<rect x="${xx}" y="${h-pad.b-bh}" width="${bw}" height="${bh}" rx="2" fill="${i===data.length-1?color:'var(--line2)'}"/><text x="${xx+bw/2}" y="${h-5}" text-anchor="middle" class="axis">${labels[i]}</text>`;}).join('')}</svg>`;
}

function donut(parts, size=104, stroke=12){
  const r=(size-stroke)/2, c=size/2, circ=2*Math.PI*r;
  let off=0;
  return `<svg width="${size}" height="${size}" viewBox="0 0 ${size} ${size}">
    <circle cx="${c}" cy="${c}" r="${r}" fill="none" stroke="var(--surface3)" stroke-width="${stroke}"/>
    ${parts.map(p=>{const len=circ*p.v; const el=`<circle cx="${c}" cy="${c}" r="${r}" fill="none" stroke="${p.c}" stroke-width="${stroke}" stroke-dasharray="${len} ${circ-len}" stroke-dashoffset="${-off}" transform="rotate(-90 ${c} ${c})"/>`; off+=len; return el;}).join('')}
    <circle cx="${c}" cy="${c}" r="${r-stroke-2}" fill="var(--surface)"/>
  </svg>`;
}

/* fragments */
const st=(text,cls='ok')=>`<span class="st"><span class="dot ${cls}"></span>${text}</span>`;
const badge=(text,cls='gray')=>`<span class="badge ${cls}">${text}</span>`;
const cb=(on=false)=>`<span class="cb ${on?'on':''}"></span>`;

function kpi(label,val,delta,deltaCls,spark,corner){
  return `<div class="kpi"><div class="lbl">${label}</div><div class="val">${val}</div><div class="delta ${deltaCls||''}">${delta}</div>${spark?sparkline(spark,deltaCls==='down'?'var(--danger)':'var(--signal)'):''}${corner?`<span class="corner">${corner}</span>`:''}</div>`;
}

function accessRow(a, selected=false){
  return `<tr class="${selected?'selected':''}">
    <td>${cb(selected)}</td>
    <td><div class="name">${a.name} ${a.default?'<span class="badge ok">默认</span>':''}</div><div class="sub">${a.id}</div></td>
    <td>${badge(a.kind,a.kindCls)}</td>
    <td><div class="url">${a.url}</div><div class="sub">${a.protocol}</div></td>
    <td class="right mono">${a.models}</td>
    <td>${a.cred==='ok'?st('已配置','ok'):a.cred==='pool'?`<span class="badge violet">池 ${a.pool}</span>`:badge('缺失','warn')}</td>
    <td>${st(a.health,a.healthCls)}<div class="sub mono">P95 ${a.p95}</div></td>
    <td class="right mono">${a.today}<div class="sub">${a.rate}</div></td>
    <td class="right"><button class="btn sm ghost">测试</button> <button class="icon-btn" style="width:24px;height:24px;display:inline-grid">⋯</button></td>
  </tr>`;
}

function configDomain(n,title,desc,stats,issue,action,color){
  return `<div class="card" style="padding:16px 17px;min-height:208px;display:flex;flex-direction:column">
    <div style="display:flex;align-items:center;gap:8px">
      <span class="avatar" style="width:28px;height:28px;color:${color};border-color:${color}33;background:${color}14">${n}</span>
      <div><div style="font-weight:600;font-size:13.5px">${title}</div><div class="hint" style="margin-top:1px">${desc}</div></div>
      <span class="badge ${issue.cls}" style="margin-left:auto">${issue.text}</span>
    </div>
    <div style="display:grid;grid-template-columns:repeat(2,1fr);gap:8px;margin-top:14px">
      ${stats.map(s=>`<div style="border:1px solid var(--line);border-radius:8px;padding:8px 9px;background:var(--surface2)"><div class="hint" style="font-size:9.5px">${s[0]}</div><div class="mono" style="font-size:16px;margin-top:2px">${s[1]}</div></div>`).join('')}
    </div>
    <div style="display:flex;align-items:center;gap:6px;margin-top:auto;padding-top:12px">
      <button class="btn sm">${action}</button><span class="hint">${issue.hint||''}</span>
    </div>
  </div>`;
}

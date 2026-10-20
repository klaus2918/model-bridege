# Model-Gate exe · live-panel 动态架构图（历史设计稿）

> ⚠️ **历史存档，非当前产品文档**。本目录基于 Model-Gate 2.7 的 **Python 版**源码结构制作，
> 所述模块（`entry.py`、`modelgate/app.py`、`proxy.py`、`api.py`、`db.py`、`config.py`、
> `protocols/`、`single.py`、`pricing.py`）**已随 Rust-only 改造全部删除**。
> 当前产品只有 Rust 实现，模块划分见 `rust/model-gate/src/`。
>
> 仓库已移除 Python 工具链，因此**下方的重新生成步骤在当前仓库不可执行**；
> 它们仅作为历史记录保留，供追溯该设计稿的来源。
> 需要反映现状的架构说明请看 `docs/architecture/rust-desktop-shell.md`。

用 [live-panel](https://github.com/ythx-101/live-panel-skill) 生成的「一直在运行」的架构图：布局固定，线上有数据包流动、状态条翻转、日志滚动、计数器跳动。数字均为示意（未接入真实遥测）。

## 产物

| 文件 | 说明 |
|---|---|
| `model-gate-live-panel.html` | **主产物**：自包含循环网页，双击用浏览器打开即可播放（155 KB） |
| `model-gate-live-panel.mp4` | 30 秒 H.264 视频（1200×1500@30，含静音音轨，1.9 MB），可直接分享/嵌入；体积较大，**本地生成、不入库** |
| `preview.png` | 一帧静态预览（1200×1500） |
| `model-gate-config.json` | live-panel 配置（可直接改文案/坐标/机器） |
| `model-gate-spec.json` | scaffold 用的极简布局 spec（改完可重新生成） |
| `frames/` | 校验帧（3 帧；其中一帧与 `preview.png` 重复，已去重） |

> 仓库已不再保留 `patch_log_machines.py` 等 Python 辅助脚本；重新生成需要自备
> live-panel 工具与 Python 环境，步骤见下方「历史步骤」。

## 图里有什么

- 纵向主链：Agent 客户端 → Model-Gate.exe（启动/桌面壳）→ 本地 HTTP 服务 :8900 → Bridge 内核 → 三类协议适配 → 四条上游（cch / go / zen / commandcode）
- 数据与配置侧栏：SQLite 明细（WAL、ttft/cost）、配置与密钥（文件优先、密钥仅驻内存）、日志（5 MB×3 轮转）
- 左侧「运行态回退」触发器：端口被占用、上游 5xx/429、密钥过期、保留策略
- 状态栏：请求数 / tokens 计数器；底部滚动访问日志；数据包沿每条连线流动

## 重新生成（历史步骤，当前仓库不可执行）

> 依赖外部 live-panel 工具与 Python 解释器；仓库已不再维护这些脚本。

```powershell
# live-panel 工具目录（外部依赖，按本机实际位置设置；仓库不包含该工具）
$LP = "C:\path\to\live-panel-skill"

# 1) spec → config（自动布局）
py -3 "$LP\scripts\scaffold.py" `
  --spec docs\live-panel\model-gate-spec.json `
  --out  docs\live-panel\model-gate-config.json

# 2) 渲染循环 HTML
py -3 "$LP\scripts\render.py" `
  --config docs\live-panel\model-gate-config.json `
  --html-out docs\live-panel\model-gate-live-panel.html

# 3) 逐帧自检（布局溢出/重叠 + 重放确定性）
py -3 "$LP\scripts\check_frames.py" `
  --config docs\live-panel\model-gate-config.json `
  --out-dir docs\live-panel\frames --repeat
```

校验结果：124 个时间点、0 布局问题、重放逐像素一致。

> 需要 mp4 时给 `render.py` 加 `--out xxx.mp4`（需 Chrome/Chromium 与 ffmpeg）。网页 URL 加 `?manual` 可关闭自动循环。

## 事实来源（历史）

内容基于 Model-Gate 2.x **Python 版**源码结构（初版梳理于 v2.7.0，模块划分在 2.x 内保持稳定）：`entry.py`、`modelgate/app.py`、`proxy.py`、`api.py`、`db.py`、`config.py`、`protocols/`、`single.py`、`pricing.py`。数字为动画示意，不代表真实运行数据。上述 Python 模块已全部删除。

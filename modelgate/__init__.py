# -*- coding: utf-8 -*-
"""Model-Gate 桌面代理：模型接入、模型配置、token 用量统计与本地数据库留存。

模块划分：
- config   配置读写（bridge.config.json 兼容 + SQLite settings）
- db       SQLite 用量明细与设置（WAL + 线程锁）
- protocols  anthropic / openai / commandcode 三套协议适配
- proxy    本地 HTTP 代理服务（/health、/v1/models、/v1/chat/completions、前缀路由）
- app      pywebview 窗口 / 托盘 / 单实例
"""

__version__ = "2.19.0"
APP_NAME = "Model-Gate"
APP_TITLE = "Model-Gate - 模型代理与用量统计"

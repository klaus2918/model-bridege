# -*- coding: utf-8 -*-
"""骨架冒烟：配置加载、上游归一化、SQLite 写入与聚合。"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import modelgate
from modelgate import config as cfgmod
from modelgate import db as dbmod


def main() -> int:
    cfg = cfgmod.load_config("bridge.config.json")
    ups = cfgmod.upstreams_of(cfg)
    print("version:", modelgate.__version__)
    print("upstreams:", [(u["name"], u["protocol"], u["baseUrl"]) for u in ups])
    print("models:", cfgmod.model_list(cfg, ups))
    for u in ups:
        print("  key source", u["name"], "->", u["apiKeySource"])
    picked = cfgmod.pick_upstream(cfg, ups, "glm5.3")
    print("pick(glm5.3):", picked["name"])
    picked2 = cfgmod.pick_upstream(cfg, ups, "unknown-model")
    print("pick(unknown-model):", picked2["name"])

    db = dbmod.open_db("data/_smoke.db")
    db.record_usage(
        upstream="cch", protocol="anthropic", requested_model="glm5.3",
        upstream_model="glm5.3", prompt_tokens=10, completion_tokens=5,
        cache_read_tokens=3, duration_ms=12, status=200, stream=True,
    )
    db.record_usage(
        upstream="go", protocol="openai", requested_model="space-bunny",
        upstream_model="space-bunny", prompt_tokens=4, completion_tokens=6,
        duration_ms=30, status=500, error="boom",
    )
    print("totals:", db.totals())
    print("summary(day):", db.summary(group_by="day"))
    print("summary(model):", db.summary(group_by="model"))
    print("summary(upstream):", db.summary(group_by="upstream"))
    print("count:", db.count_usage())
    print("list:", len(db.list_usage(limit=10)))
    db.set_setting("k", "v")
    print("setting:", db.get_setting("k"))
    db.save_config_override({"port": 8900})
    print("override:", db.load_config_override())
    db.close()
    print("SMOKE OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

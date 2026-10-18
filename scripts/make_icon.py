# -*- coding: utf-8 -*-
"""生成 assets/ModelGate.ico（打包前置步骤）。

图标语义：深蓝底 + 左侧一个汇聚点 + 右侧三个上游节点，
对应「多上游 → 单网关 → 下游客户端」的模型接入拓扑。
"""
from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "assets" / "ModelGate.ico"


def draw(size: int):
    from PIL import Image, ImageDraw

    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([0, 0, size - 1, size - 1], radius=size // 5,
                        fill=(30, 64, 175, 255))
    margin = size // 4
    cx, cy = margin, size // 2
    width = max(2, size // 18)
    r = max(2, size // 11)
    nodes = [(size - margin, margin), (size - margin, cy), (size - margin, size - margin)]
    for nx, ny in nodes:
        d.line([(cx, cy), (nx, ny)], fill=(255, 255, 255, 255), width=width)
    d.ellipse([cx - r * 2, cy - r * 2, cx + r * 2, cy + r * 2],
              fill=(255, 255, 255, 255))
    for nx, ny in nodes:
        d.ellipse([nx - r, ny - r, nx + r, ny + r], fill=(255, 255, 255, 255))
    return img


def main() -> int:
    try:
        from PIL import Image  # noqa: F401
    except Exception as exc:
        print(f"[make-icon] 跳过：Pillow 不可用（{exc}）")
        return 0

    OUT.parent.mkdir(parents=True, exist_ok=True)
    base = draw(256)
    sizes = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
    base.save(OUT, format="ICO", sizes=sizes)
    print(f"[make-icon] 已生成 {OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

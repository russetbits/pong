#!/usr/bin/env python3
"""pong GUI 快照光栅化：gui_snapshot.json → gui_snapshot.png
（examples/gui_snapshot.rs 导出 egui tessellation mesh + 字体图集；本脚本做软件渲染闭环预览）
用法：PYTHONPATH=tools/cache/pylibs python3 tools/gui_render.py [json路径] [png路径]
需要：numpy（pylibs 已有；若缺 pip 装 tools/cache/pylibs）+ cairosvg 无关（纯 numpy 光栅化）
"""
import json, sys, os
import numpy as np

src = sys.argv[1] if len(sys.argv) > 1 else "gui_snapshot.json"
dst = sys.argv[2] if len(sys.argv) > 2 else "gui_snapshot.png"
d = json.load(open(src))
W, H = d["w"], d["h"]
W, H = int(W), int(H)

# 纹理加载：rgba_hex → (H,W,4) uint8
texs = {}
for k, t in d["textures"].items():
    hexs = t["rgba_hex"]
    arr = np.frombuffer(bytes.fromhex(hexs), dtype=np.uint8).reshape(t["h"], t["w"], 4)
    texs[k] = arr
print(f"texs: {[(k, v.shape) for k, v in texs.items()]}")

canvas = np.zeros((H, W, 4), dtype=np.float32)  # RGBA premul-ish
canvas[..., 3] = 255.0

def sample(tex, u, v):
    th, tw = tex.shape[:2]
    x = min(max(int(u * tw), 0), tw - 1)
    y = min(max(int(v * th), 0), th - 1)
    return tex[y, x]

for p in d["prims"]:
    cx0, cy0, cx1, cy1 = p["clip"]
    verts = p["verts"]; idx = p["idx"]
    v = np.array([[vv["x"], vv["y"], vv["u"], vv["v"]] for vv in verts], dtype=np.float32)
    col = np.array([vv["c"] for vv in verts], dtype=np.float32)
    # 键盘取纹理：egui mesh 顶点无 tex id——egui 0.33 每个 mesh 单纹理（texture id 隐含 default Managed(0)）
    tex = texs.get("Managed(0)")
    for i in range(0, len(idx), 3):
        a, b, c = idx[i], idx[i+1], idx[i+2]
        p0, p1, p2 = v[a], v[b], v[c]
        c0, c1, c2 = col[a], col[b], col[c]
        # 背景三角形（u=v=0 全域）直接顶点色填充
        xmin = max(int(min(p0[0], p1[0], p2[0])), int(cx0), 0)
        xmax = min(int(max(p0[0], p1[0], p2[0])) + 1, int(cx1), W)
        ymin = max(int(min(p0[1], p1[1], p2[1])), int(cy0), 0)
        ymax = min(int(max(p0[1], p1[1], p2[1])) + 1, int(cy1), H)
        if xmin >= xmax or ymin >= ymax:
            continue
        xs, ys = np.meshgrid(np.arange(xmin, xmax) + 0.5, np.arange(ymin, ymax) + 0.5)
        d1 = (p1[0]-p0[0])*(ys-p0[1]) - (p1[1]-p0[1])*(xs-p0[0])
        d2 = (p2[0]-p1[0])*(ys-p1[1]) - (p2[1]-p1[1])*(xs-p1[0])
        d3 = (p0[0]-p2[0])*(ys-p2[1]) - (p0[1]-p2[1])*(xs-p2[0])
        inside = ((d1 >= 0) & (d2 >= 0) & (d3 >= 0)) | ((d1 <= 0) & (d2 <= 0) & (d3 <= 0))
        if not inside.any():
            continue
        area = (p1[0]-p0[0])*(p2[1]-p0[1]) - (p1[1]-p0[1])*(p2[0]-p0[0])
        if abs(area) < 1e-9:
            continue
        w0 = ((p1[0]-xs)*(p2[1]-ys) - (p1[1]-ys)*(p2[0]-xs)) / area
        w1 = ((p2[0]-xs)*(p0[1]-ys) - (p2[1]-ys)*(p0[0]-xs)) / area
        w2 = 1 - w0 - w1
        # uv 采样（字体 glyph 三角形 uv>0；背景 uv 全 0）
        uu = w0*p0[2] + w1*p1[2] + w2*p2[2]
        vv_ = w0*p0[3] + w1*p1[3] + w2*p2[3]
        # 逐点（慢但 640x480 没问题）
        yy, xx = np.where(inside)
        for dy, dx in zip(yy, xx):
            px, py = xmin + dx, ymin + dy
            wsum = w0[dy, dx]*c0 + w1[dy, dx]*c1 + w2[dy, dx]*c2
            u = w0[dy, dx]*p0[2] + w1[dy, dx]*p1[2] + w2[dy, dx]*p2[2]
            vv2 = w0[dy, dx]*p0[3] + w1[dy, dx]*p1[3] + w2[dy, dx]*p2[3]
            if u > 1e-5 or vv2 > 1e-5:
                if tex is not None:
                    tx = sample(tex, u, vv2)
                    # 字体图集白色 glyph alpha 通道 = 覆盖度；乘顶点色
                    a = tx[3] / 255.0
                    cc2 = wsum[:3] * a + canvas[py, px, :3] * (1 - a)
                    canvas[py, px, :3] = cc2
                else:
                    canvas[py, px, :3] = wsum[:3]
            else:
                canvas[py, px, :3] = wsum[:3]

# 白底更接近 Windows 浅色主题？egui 默认 dark——保持 dark
out = canvas.astype(np.uint8)
# 转 PNG：用 cairosvg? 不——直接手写 PNG（zlib+struct，RGBA 无压缩行）
import zlib, struct
def write_png(path, arr):
    h, w = arr.shape[:2]
    raw = b"".join(b"\x00" + arr[y].tobytes() for y in range(h))
    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xffffffff)
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw))
    png += chunk(b"IEND", b"")
    open(path, "wb").write(png)
write_png(dst, out)
print("PNG written:", dst, f"{os.path.getsize(dst)} bytes")

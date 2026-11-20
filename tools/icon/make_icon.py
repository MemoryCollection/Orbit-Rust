#!python
"""把 assets/orbit-icon.png 的白底抠成透明，并重新生成多尺寸 assets/orbit.ico。

原图是"白底 + 圆角深色方块"，四角白底是不透明白色——嵌进 exe 后在任务栏/
Alt+Tab 上就是四个白角。这里按连通性把外圈白底标成透明，边缘残留的
抗锯齿像素按"白底与深色主体之间的覆盖率"反算 alpha，缩到 16px 也不会
出现白边或黑边。

用法（仓库根目录）：python tools/icon/make_icon.py
"""

from collections import deque
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[2]
SRC = ROOT / "assets" / "orbit-icon.png"
OUT_PNG = ROOT / "assets" / "orbit-icon.png"
OUT_ICO = ROOT / "assets" / "orbit.ico"

# 判定"纯白底"的门槛：三个通道都不低于它就算背景（图标主体最深，绝不会这么白）
WHITE_MIN = 246
# 边缘带宽度（像素）：只有紧贴白底的这几圈才需要反算 alpha
BAND = 3
ICO_SIZES = [256, 128, 64, 48, 40, 32, 24, 20, 16]


def flatten_white(img: Image.Image) -> Image.Image:
    """白底 → 透明（含边缘抗锯齿），返回 RGBA。"""
    img = img.convert("RGBA")
    w, h = img.size
    px = img.load()

    def is_white(x: int, y: int) -> bool:
        r, g, b, _ = px[x, y]
        return r >= WHITE_MIN and g >= WHITE_MIN and b >= WHITE_MIN

    # 1) 从四边 BFS，标记与外界连通的白底。图标内部若有大片白色（猫脸）
    #    也不会被波及——它不和外圈相连。
    outside = bytearray(w * h)
    q = deque()
    for x in range(w):
        for y in (0, h - 1):
            if is_white(x, y) and not outside[y * w + x]:
                outside[y * w + x] = 1
                q.append((x, y))
    for y in range(h):
        for x in (0, w - 1):
            if is_white(x, y) and not outside[y * w + x]:
                outside[y * w + x] = 1
                q.append((x, y))
    while q:
        x, y = q.popleft()
        for dx, dy in ((1, 0), (-1, 0), (0, 1), (0, -1)):
            nx, ny = x + dx, y + dy
            if 0 <= nx < w and 0 <= ny < h and not outside[ny * w + nx] and is_white(nx, ny):
                outside[ny * w + nx] = 1
                q.append((nx, ny))

    # 2) 白底全透明；贴着白底的一圈（抗锯齿过渡）反算覆盖率
    for y in range(h):
        for x in range(w):
            i = y * w + x
            r, g, b, _ = px[x, y]
            if outside[i]:
                px[x, y] = (r, g, b, 0)
                continue
            # 是否在边缘带内（周围 BAND 像素内有背景）
            near = False
            for dy in range(-BAND, BAND + 1):
                for dx in range(-BAND, BAND + 1):
                    nx, ny = x + dx, y + dy
                    if 0 <= nx < w and 0 <= ny < h and outside[ny * w + nx]:
                        near = True
                        break
                if near:
                    break
            if not near:
                continue
            lum = 0.299 * r + 0.587 * g + 0.114 * b
            if lum <= 60.0:
                continue  # 已经是主体本身，不动
            # 用邻域里"确定是主体"的像素亮度当白底叠上去的底色
            body = [0.0, 0.0]
            for dy in range(-3, 4):
                for dx in range(-3, 4):
                    nx, ny = x + dx, y + dy
                    if not (0 <= nx < w and 0 <= ny < h) or outside[ny * w + nx]:
                        continue
                    rr, gg, bb, _ = px[nx, ny]
                    if max(rr, gg, bb) >= WHITE_MIN:
                        continue
                    body[0] += 0.299 * rr + 0.587 * gg + 0.114 * bb
                    body[1] += 1.0
            base = body[0] / body[1] if body[1] > 0 else 42.0
            base = min(max(base, 12.0), 120.0)
            a = (255.0 - lum) / (255.0 - base)
            a = min(max(a, 0.0), 1.0)
            if a <= 0.02:
                px[x, y] = (r, g, b, 0)
                continue
            # straight alpha 反解：observed = a*C + (1-a)*white
            out = []
            for c in (r, g, b):
                v = (c - (1.0 - a) * 255.0) / a
                out.append(int(round(min(max(v, 0.0), 255.0))))
            px[x, y] = (out[0], out[1], out[2], int(round(a * 255.0)))
    return img


def main() -> None:
    src = Image.open(SRC)
    cut = flatten_white(src)

    # 裁到内容边界再补成正方形：原图 867×851 不是正方形，直接缩 256 会轻微变形
    bbox = cut.getbbox()
    if bbox:
        cut = cut.crop(bbox)
    side = max(cut.size)
    canvas = Image.new("RGBA", (side, side), (0, 0, 0, 0))
    canvas.paste(cut, ((side - cut.width) // 2, (side - cut.height) // 2))

    canvas.save(OUT_PNG)

    # ICO：每个尺寸单独 LANCZOS 缩放（PIL 直接给 sizes 也是逐个缩，但主图
    # 只有 867px，这里先缩到 256 再交给它，保证 256 那档是高质量降采样）
    big = canvas.resize((256, 256), Image.LANCZOS)
    big.save(OUT_ICO, format="ICO", sizes=[(s, s) for s in ICO_SIZES])

    # 自检：四角必须全透明
    alpha = canvas.load()
    for x, y in [(0, 0), (side - 1, 0), (0, side - 1), (side - 1, side - 1)]:
        assert alpha[x, y][3] == 0, f"角上还有底色：{(x, y)} → {alpha[x, y]}"
    print(f"ok: {OUT_PNG.name} {canvas.size} / {OUT_ICO.name} {ICO_SIZES}")


if __name__ == "__main__":
    main()

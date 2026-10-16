#!/usr/bin/env python3
"""工作区侧栏图标：在 24×24 网格上画线性图标，写进 icons.ttf 与 fontello.json。

风格参照 TradingView / Bloomberg Launchpad / Refinitiv 的左栏：统一 2 单位描边、圆端圆角、
不填色（个别实心点除外），一个工作区一个专属图形，不复用。

用法（只依赖 fontTools，主环境（WealthSpring 仓库 .venv）自带）：
    .venv/bin/python scripts/ws_icons.py

可重复运行：先删掉本脚本管辖码位（WS_BASE 起）的旧字形与配置再重写。
码位与 `src/style.rs` 的 `Icon` 枚举一一对应，改这里要同步改那里。

几何做法：描边拆成「每段一个矩形 + 每个顶点一个圆」，圆环/弧是内外两圈的环带，
互相重叠的轮廓同向，靠 TrueType 的非零环绕规则合并（swash/cosmic-text 按非零填充）。
"""

import json
import math
import re
from pathlib import Path

from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTFont

ROOT = Path(__file__).resolve().parent.parent
TTF = ROOT / "assets/fonts/icons.ttf"
CFG = ROOT / "assets/fonts/fontello.json"

WS_BASE = 0xE900  # 59648 起；旧字形在 0xE800–0xE821 与 0xF0xx，不冲突
SW = 2.2  # 描边宽（网格单位）
SCALE = 900 / 24  # 网格 → 字体单位
X0 = 30  # 左侧留白
ADV = round(X0 * 2 + 24 * SCALE)
TOP = 825  # 网格 y=0 对应的字体 y（基线向上为正）


# ── 基本几何（网格坐标，y 向下）──────────────────────────────────────


def circle_pts(cx, cy, r, n=None):
    n = n or max(16, int(r * 10))
    return [(cx + r * math.cos(2 * math.pi * i / n), cy + r * math.sin(2 * math.pi * i / n)) for i in range(n)]


def arc_pts(cx, cy, r, a0, a1, n=None):
    n = n or max(8, int(abs(a1 - a0) * r * 2))
    return [(cx + r * math.cos(a0 + (a1 - a0) * i / n), cy + r * math.sin(a0 + (a1 - a0) * i / n)) for i in range(n + 1)]


class Icon:
    def __init__(self):
        self.fills = []  # 实心轮廓
        self.holes = []  # 挖空轮廓

    def dot(self, x, y, r=SW / 2):
        self.fills.append(circle_pts(x, y, r, 16 if r <= 1.5 else None))

    def line(self, *pts, closed=False):
        pts = list(pts)
        if closed:
            pts.append(pts[0])
        h = SW / 2
        for (x0, y0), (x1, y1) in zip(pts, pts[1:]):
            dx, dy = x1 - x0, y1 - y0
            ln = math.hypot(dx, dy)
            if ln == 0:
                continue
            nx, ny = -dy / ln * h, dx / ln * h
            self.fills.append([(x0 + nx, y0 + ny), (x1 + nx, y1 + ny), (x1 - nx, y1 - ny), (x0 - nx, y0 - ny)])
        for x, y in pts:
            self.dot(x, y)

    def ring(self, cx, cy, r):
        self.fills.append(circle_pts(cx, cy, r + SW / 2))
        self.holes.append(circle_pts(cx, cy, r - SW / 2))

    def arc(self, cx, cy, r, a0, a1):
        """角度用度，0° 指向右，顺时针（y 向下）。"""
        a0, a1 = math.radians(a0), math.radians(a1)
        outer = arc_pts(cx, cy, r + SW / 2, a0, a1)
        inner = arc_pts(cx, cy, r - SW / 2, a0, a1)
        self.fills.append(outer + inner[::-1])
        for a in (a0, a1):
            self.dot(cx + r * math.cos(a), cy + r * math.sin(a))

    def rect(self, x0, y0, x1, y1, r=2.0):
        """圆角矩形描边。"""
        self.round_rect(x0 - SW / 2, y0 - SW / 2, x1 + SW / 2, y1 + SW / 2, r + SW / 2, self.fills)
        self.round_rect(x0 + SW / 2, y0 + SW / 2, x1 - SW / 2, y1 - SW / 2, max(r - SW / 2, 0), self.holes)

    def solid(self, x0, y0, x1, y1, r=0.5):
        self.round_rect(x0, y0, x1, y1, r, self.fills)

    @staticmethod
    def round_rect(x0, y0, x1, y1, r, into):
        if r <= 0:
            into.append([(x0, y0), (x1, y0), (x1, y1), (x0, y1)])
            return
        pts = []
        for cx, cy, a in ((x1 - r, y0 + r, -90), (x1 - r, y1 - r, 0), (x0 + r, y1 - r, 90), (x0 + r, y0 + r, 180)):
            pts += arc_pts(cx, cy, r, math.radians(a), math.radians(a + 90), 6)
        into.append(pts)

    def poly(self, *pts):
        self.fills.append(list(pts))


# ── 16 个工作区图标 ──────────────────────────────────────────────────


def candles():  # 官方原生：K 线
    i = Icon()
    i.line((7, 3), (7, 21))
    i.solid(4.5, 7, 9.5, 16, 1)
    i.line((17, 4), (17, 8))
    i.line((17, 15), (17, 20))
    i.rect(15, 8, 19, 15, 1)
    return i


def newspaper():  # 新闻资讯
    i = Icon()
    i.rect(7.5, 4, 20.5, 20, 1.5)
    i.line((7.5, 9), (4, 9), (4, 18))
    i.arc(6, 18, 2, 90, 180)
    i.line((11, 8.5), (17, 8.5))
    i.line((11, 12), (17, 12))
    i.line((11, 15.5), (15, 15.5))
    return i


def globe():  # 全球市场
    i = Icon()
    i.ring(12, 12, 9)
    i.line((3.5, 12), (20.5, 12))
    e = [(12 + 4 * math.sin(t), 12 - 9 * math.cos(t)) for t in [math.pi * k / 16 for k in range(33)]]
    i.line(*e)
    return i


def database():  # 数据录制：数据湖
    i = Icon()

    def ellipse_arc(cy, a0, a1):
        return [(12 + 8 * math.cos(t), cy + 3 * math.sin(t)) for t in [a0 + (a1 - a0) * k / 24 for k in range(25)]]

    i.line(*ellipse_arc(5.5, 0, 2 * math.pi))
    i.line((4, 5.5), (4, 18.5))
    i.line((20, 5.5), (20, 18.5))
    i.line(*ellipse_arc(12, 0, math.pi))
    i.line(*ellipse_arc(18.5, 0, math.pi))
    return i


def history():  # Tardis 历史回放：带逆时针箭头的时钟
    i = Icon()
    i.arc(12.5, 12, 8.5, -150, 180)
    i.line((4, 12), (2.5, 9.5))
    i.line((4, 12), (6.8, 10.5))
    i.line((12.5, 7.5), (12.5, 12), (15.5, 14))
    return i


def terminal():  # 接口观察终端
    i = Icon()
    i.rect(3, 4, 21, 20, 2)
    i.line((7, 9), (10, 12), (7, 15))
    i.line((12.5, 15.5), (17, 15.5))
    return i


def ladder():  # 订单流特征：成交量分布（Volume Profile）横条
    i = Icon()
    i.line((3, 2.5), (3, 21.5))
    for y, w in ((4.5, 8), (8.5, 13), (12.5, 18), (16.5, 11), (20.5, 6)):
        i.line((6, y), (6 + w - 3, y))
    return i


def layers():  # 订单流层析：分层切片
    i = Icon()
    i.line((12, 3), (21, 7.5), (12, 12), (3, 7.5), closed=True)
    i.line((3, 12), (12, 16.5), (21, 12))
    i.line((3, 16.5), (12, 21), (21, 16.5))
    return i


def nodes():  # 策略中心：策略流程节点
    i = Icon()
    i.rect(3, 3, 9, 9, 1.5)
    i.rect(15, 3, 21, 9, 1.5)
    i.rect(9, 15, 15, 21, 1.5)
    i.line((6, 9), (6, 12), (18, 12), (18, 9))
    i.line((12, 12), (12, 15))
    return i


def flask():  # Alpha Factory：研究烧瓶
    i = Icon()
    i.line((9.5, 3), (9.5, 9.5), (4, 19), (5, 21), (19, 21), (20, 19), (14.5, 9.5), (14.5, 3))
    i.line((8, 3), (16, 3))
    i.poly((7, 15.5), (17, 15.5), (19.2, 19.4), (18.6, 20), (5.4, 20), (4.8, 19.4))
    return i


def shield():  # C4 影子：合格判定
    i = Icon()
    i.line((12, 2.5), (20, 5.5), (20, 11), (18.5, 15.5), (12, 21.5), (5.5, 15.5), (4, 11), (4, 5.5), closed=True)
    i.line((8.5, 12), (11, 14.5), (15.5, 9.5))
    return i


def payoff():  # 期权/0DTE：看涨期权损益曲线
    i = Icon()
    i.line((3, 3), (3, 21), (21, 21))
    i.line((3, 15), (11, 15), (20, 5))
    i.dot(11, 15, 1.8)  # 行权价
    return i


def pie():  # 预测市场：概率饼图
    i = Icon()
    i.arc(11, 13, 8, -90 + 6, 270 - 6)
    i.poly(*([(13, 11)] + arc_pts(13, 11, 8, math.radians(-90), math.radians(0))))
    return i


def equity():  # 回测：权益曲线（与期权的 L 形坐标轴区分，只留底线）
    i = Icon()
    i.line((3, 20.5), (21, 20.5))
    i.line((3, 15.5), (8.5, 9.5), (12.5, 13.5), (20.5, 5))
    i.line((15, 5), (20.5, 5), (20.5, 10.5))
    return i


def pulse():  # 实时数据回测：心跳线（实时）
    i = Icon()
    i.line((2.5, 12), (7, 12), (9.5, 5), (14, 19), (16.5, 12), (21.5, 12))
    return i


def chip():  # 资源：处理器
    i = Icon()
    i.rect(6, 6, 18, 18, 2)
    i.solid(9.5, 9.5, 14.5, 14.5, 1)
    for p in (9.5, 14.5):
        i.line((p, 2.5), (p, 6))
        i.line((p, 18), (p, 21.5))
        i.line((2.5, p), (6, p))
        i.line((18, p), (21.5, p))
    return i


def flow():  # 策略画布：两个节点、端口上的圆点、一根折过去的连线
    i = Icon()
    i.rect(2.5, 3.5, 10.5, 10.5, 1.5)
    i.rect(13.5, 13.5, 21.5, 20.5, 1.5)
    i.line((10.5, 7), (15, 7), (9, 17), (13.5, 17))
    i.dot(10.5, 7, 1.7)
    i.dot(13.5, 17, 1.7)
    return i


def calendar():  # 金融日历：日历页 + 两个挂环 + 标记日
    i = Icon()
    i.rect(3, 5, 21, 21, 2)
    i.line((3.5, 10), (20.5, 10))
    i.line((8, 2.5), (8, 7))
    i.line((16, 2.5), (16, 7))
    i.solid(6.5, 13, 9.5, 16, 0.8)
    i.dot(12, 14.5, 1.2)
    i.dot(16.5, 14.5, 1.2)
    i.dot(7.5, 18.5, 1.2)
    i.dot(12, 18.5, 1.2)
    return i


# ── 绘图工具图标（docs/46 §13）：锚点画成实心圆点，与 TradingView 左栏同一套语汇 ──────────


def _eye(i):
    top = [(12 + 9.5 * math.cos(t), 12 - 6 * math.sin(t)) for t in [math.pi * k / 16 for k in range(17)]]
    bot = [(12 + 9.5 * math.cos(t), 12 + 6 * math.sin(t)) for t in [math.pi * k / 16 for k in range(17)]]
    i.line(*top)
    i.line(*bot)
    i.dot(12, 12, 2.8)


def _pencil(i):
    i.line((4, 20), (4.5, 15.5), (15, 5), (19, 9), (8.5, 19.5), closed=True)
    i.line((12.5, 7.5), (16.5, 11.5))


def dr_cursor():
    i = Icon()
    i.line((6, 3), (6, 19), (10, 15.5), (13, 21), (15.5, 19.8), (12.6, 14.3), (18, 14), closed=True)
    return i


def dr_hline():
    i = Icon()
    i.line((2.5, 12), (21.5, 12))
    i.dot(12, 12, 2.6)
    return i


def dr_vline():
    i = Icon()
    i.line((12, 2.5), (12, 21.5))
    i.dot(12, 12, 2.6)
    return i


def dr_trend():
    i = Icon()
    i.line((5, 19), (19, 5))
    i.dot(5, 19, 2.6)
    i.dot(19, 5, 2.6)
    return i


def dr_ray():
    i = Icon()
    i.line((4, 20), (21.5, 2.5))
    i.dot(4, 20, 2.6)
    i.dot(11, 13, 2.6)
    return i


def dr_polyline():
    i = Icon()
    pts = [(3, 18), (8.5, 7), (14, 15), (21, 5)]
    i.line(*pts)
    for x, y in pts:
        i.dot(x, y, 2.2)
    return i


def dr_channel():
    i = Icon()
    i.line((3, 14), (16, 4))
    i.line((8, 20), (21, 10))
    i.dot(3, 14, 2.2)
    i.dot(16, 4, 2.2)
    i.dot(21, 10, 2.2)
    return i


def dr_fib_retrace():
    i = Icon()
    for y in (4, 9.5, 14.5, 20):
        i.line((3, y), (21, y))
    i.dot(6, 20, 2.4)
    i.dot(18, 4, 2.4)
    return i


def dr_fib_ext():
    i = Icon()
    pts = [(3, 19), (7.5, 7), (11, 14)]
    i.line(*pts)
    for x, y in pts:
        i.dot(x, y, 2.1)
    for y in (4, 9.5, 15, 20.5):
        i.line((14.5, y), (21, y))
    return i


def dr_rect():
    i = Icon()
    i.rect(4, 6, 20, 18, 1)
    i.dot(4, 6, 2.4)
    i.dot(20, 18, 2.4)
    return i


def dr_text():
    i = Icon()
    i.line((5, 5), (19, 5))
    i.line((5, 5), (5, 7.5))
    i.line((19, 5), (19, 7.5))
    i.line((12, 5), (12, 20))
    i.line((9, 20), (15, 20))
    return i


def dr_callout():
    i = Icon()
    i.rect(3, 3.5, 21, 15, 2)
    i.line((8, 15.5), (7, 21), (12.5, 15.5))
    i.line((7, 7.5), (17, 7.5))
    i.line((7, 11), (13.5, 11))
    return i


def dr_price_range():
    i = Icon()
    i.line((6, 3), (18, 3))
    i.line((6, 21), (18, 21))
    i.line((12, 4.5), (12, 19.5))
    i.line((8.5, 8), (12, 4.5), (15.5, 8))
    i.line((8.5, 16), (12, 19.5), (15.5, 16))
    return i


def dr_date_range():
    i = Icon()
    i.line((3, 6), (3, 18))
    i.line((21, 6), (21, 18))
    i.line((4.5, 12), (19.5, 12))
    i.line((8, 8.5), (4.5, 12), (8, 15.5))
    i.line((16, 8.5), (19.5, 12), (16, 15.5))
    return i


def dr_long():
    i = Icon()
    i.rect(3, 3, 21, 21, 1.5)
    i.line((3, 14), (21, 14))
    i.line((12, 11.5), (12, 6))
    i.line((9, 9), (12, 6), (15, 9))
    i.solid(6, 16.5, 18, 18.5, 0.5)
    return i


def dr_short():
    i = Icon()
    i.rect(3, 3, 21, 21, 1.5)
    i.line((3, 10), (21, 10))
    i.line((12, 12.5), (12, 18))
    i.line((9, 15), (12, 18), (15, 15))
    i.solid(6, 5.5, 18, 7.5, 0.5)
    return i


def dr_vol_profile():
    i = Icon()
    i.line((3, 2.5), (3, 21.5))
    i.line((21, 2.5), (21, 21.5))
    for y, w in ((4.5, 5), (8.5, 9), (12.5, 14), (16.5, 8), (20, 4)):
        i.solid(5, y - 1.3, 5 + w, y + 1.3, 0.4)
    return i


def dr_marker():
    i = Icon()
    i.poly((12, 4), (20, 17), (4, 17))
    i.line((5, 21), (19, 21))
    return i


def dr_arrow():
    i = Icon()
    i.line((4, 20), (16, 8))
    i.poly((20.5, 3.5), (19.6, 12), (12, 4.4))
    return i


def dr_magnet():
    i = Icon()
    i.arc(12, 11, 7, 0, 180)
    i.line((5, 11), (5, 4.5))
    i.line((19, 11), (19, 4.5))
    i.solid(3.5, 2.5, 6.5, 6.5, 0.5)
    i.solid(17.5, 2.5, 20.5, 6.5, 0.5)
    return i


def dr_continuous():
    i = Icon()
    _pencil(i)
    i.dot(14, 20.5, 1.3)
    i.dot(17.5, 20.5, 1.3)
    i.dot(21, 20.5, 1.3)
    return i


def dr_pencil():
    i = Icon()
    _pencil(i)
    return i


def dr_eye():
    i = Icon()
    _eye(i)
    return i


def dr_eye_off():
    i = Icon()
    _eye(i)
    i.line((4, 3.5), (20, 20.5))
    return i


def dr_more():
    i = Icon()
    for x in (5, 12, 19):
        i.dot(x, 12, 2)
    return i


# 顺序 = 码位顺序（WS_BASE + 下标），与 style.rs 的 Icon 枚举一致
ICONS = [
    ("ws-candles", candles),
    ("ws-newspaper", newspaper),
    ("ws-globe", globe),
    ("ws-database", database),
    ("ws-history", history),
    ("ws-terminal", terminal),
    ("ws-ladder", ladder),
    ("ws-layers", layers),
    ("ws-nodes", nodes),
    ("ws-flask", flask),
    ("ws-shield", shield),
    ("ws-payoff", payoff),
    ("ws-pie", pie),
    ("ws-equity", equity),
    ("ws-pulse", pulse),
    ("ws-chip", chip),
    ("ws-calendar", calendar),
    ("ws-flow", flow),
    # 绘图工具（docs/46 §13）
    ("dr-cursor", dr_cursor),
    ("dr-hline", dr_hline),
    ("dr-vline", dr_vline),
    ("dr-trend", dr_trend),
    ("dr-ray", dr_ray),
    ("dr-polyline", dr_polyline),
    ("dr-channel", dr_channel),
    ("dr-fib-retrace", dr_fib_retrace),
    ("dr-fib-ext", dr_fib_ext),
    ("dr-rect", dr_rect),
    ("dr-text", dr_text),
    ("dr-callout", dr_callout),
    ("dr-price-range", dr_price_range),
    ("dr-date-range", dr_date_range),
    ("dr-long", dr_long),
    ("dr-short", dr_short),
    ("dr-vol-profile", dr_vol_profile),
    ("dr-marker", dr_marker),
    ("dr-arrow", dr_arrow),
    ("dr-magnet", dr_magnet),
    ("dr-continuous", dr_continuous),
    ("dr-pencil", dr_pencil),
    ("dr-eye", dr_eye),
    ("dr-eye-off", dr_eye_off),
    ("dr-more", dr_more),
]


# ── 写字体 ───────────────────────────────────────────────────────────


def to_font(x, y):
    return round(X0 + x * SCALE), round(TOP - y * SCALE)


def signed_area(pts):
    return sum(x0 * y1 - x1 * y0 for (x0, y0), (x1, y1) in zip(pts, pts[1:] + pts[:1])) / 2


def contours(icon):
    """字体坐标下的轮廓：实心顺时针（面积 < 0）、挖空逆时针。"""
    out = []
    for pts, want_cw in [(p, True) for p in icon.fills] + [(p, False) for p in icon.holes]:
        fp = []
        for p in pts:
            q = to_font(*p)
            if not fp or fp[-1] != q:
                fp.append(q)
        if len(fp) > 1 and fp[0] == fp[-1]:
            fp.pop()
        if len(fp) < 3:
            continue
        if (signed_area(fp) < 0) != want_cw:
            fp.reverse()
        out.append(fp)
    return out


def svg_path(cs):
    """fontello.json 用的 SVG 路径（y 向下，基线在 850）。"""
    parts = []
    for c in cs:
        parts.append("M" + "L".join(f"{x} {850 - y}" for x, y in c) + "Z")
    return "".join(parts)


def main():
    font = TTFont(TTF)
    cmap_tables = [t for t in font["cmap"].tables if t.isUnicode()]
    glyf, hmtx = font["glyf"], font["hmtx"]
    order = font.getGlyphOrder()

    # 清掉旧的本脚本字形
    managed = {WS_BASE + k for k in range(256)}
    stale = {n for t in cmap_tables for c, n in list(t.cmap.items()) if c in managed}
    for t in cmap_tables:
        for c in [c for c in t.cmap if c in managed]:
            del t.cmap[c]
    order = [n for n in order if n not in stale]
    for n in stale:
        glyf.glyphs.pop(n, None)
        hmtx.metrics.pop(n, None)

    cfg = json.loads(CFG.read_text())
    cfg["glyphs"] = [g for g in cfg["glyphs"] if g["code"] not in managed]

    for k, (name, fn) in enumerate(ICONS):
        code = WS_BASE + k
        cs = contours(fn())
        pen = TTGlyphPen(None)
        for c in cs:
            pen.moveTo(c[0])
            for p in c[1:]:
                pen.lineTo(p)
            pen.closePath()
        g = pen.glyph()
        g.recalcBounds(glyf)
        glyf.glyphs[name] = g
        hmtx.metrics[name] = (ADV, g.xMin)
        order.append(name)
        for t in cmap_tables:
            t.cmap[code] = name
        cfg["glyphs"].append(
            {
                "uid": f"wsicon{code:x}",
                "css": name,
                "code": code,
                "src": "custom_icons",
                "selected": True,
                "svg": {"path": svg_path(cs), "width": ADV},
                "search": [name],
            }
        )

    font.setGlyphOrder(order)
    glyf.glyphOrder = order
    font["maxp"].numGlyphs = len(order)
    if "post" in font and font["post"].formatType == 2.0:
        font["post"].extraNames = []
        font["post"].mapping = {}
    # 新字形没有 hinting 指令；字体级 fpgm/prep 保留不动
    font.save(TTF)
    text = json.dumps(cfg, indent=4, ensure_ascii=False)
    # 与 fontello 导出的格式一致：短数组写在一行
    text = re.sub(r'\[\n\s+("[^"\n]*")\n\s+\]', r"[\1]", text)
    CFG.write_text(text + "\n")
    print(f"写入 {len(ICONS)} 个图标：U+{WS_BASE:X}–U+{WS_BASE + len(ICONS) - 1:X}")


if __name__ == "__main__":
    main()

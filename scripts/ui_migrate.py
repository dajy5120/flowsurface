#!/usr/bin/env python3
"""界面重构批 5–7（docs/35）：把一个面板文件里的硬编码机械地迁到 token。

    python3 scripts/ui_migrate.py src/ws/feature_matrix_view.rs [...]

做三件事，其余（行内 Color::from_rgb、特殊常量）留给人工逐处判断：
1. `const C_XXX: Color = ...;` 按语义表换成 `crate::ui::pal::xxx()`（删掉常量定义，替换全部引用）
2. `.size(数字)` → `crate::ui::text::s_*()`（按就近原则归到字号角色）
3. `.padding(数字)` / `.padding([a, b])` → 间距阶梯
不认识的常量名原样保留并打印出来。
"""
import re
import sys

PAL = {
    'C_TXT': 'txt()', 'C_HEAD': 'head()', 'C_DIM': 'dim()', 'C_PEND': 'pend()',
    'C_OK': 'ok()', 'C_BAD': 'bad()', 'C_WARN': 'warn()', 'C_GOLD': 'warn()',
    'C_POS': 'up()', 'C_NEG': 'down()', 'C_BUY': 'up()', 'C_SELL': 'down()',
    'C_UP': 'up()', 'C_DOWN': 'down()', 'C_EQUITY': 'up()', 'C_DD': 'down()',
    'C_GREEN': 'up()', 'C_RED': 'down()',
    'C_BLUE': 'info()', 'C_FEAT': 'series(0)', 'C_CHART': 'series(5)', 'C_PURPLE': 'series(5)',
    'C_NOW': 'reference()', 'C_LINK': 'accent()', 'C_LINK_HOVER': 'accent()',
    'C_LINE': 'line()', 'C_BORDER': 'line()', 'C_BODY_LINE': 'line()', 'C_GRID': 'grid()',
    'C_HEAD_LINE': 'head_line()', 'C_BODY_GROUP': 'group_line()', 'C_HEAD_GROUP': 'group_line()',
    'C_BAND': 'band()', 'C_SECT': 'band()', 'C_CARD_BG': 'card_bg()', 'C_AXIS': 'axis()',
    'C_STALE': 'pend()', 'C_NEUTRAL': 'dim()',
}

SPACE = [2, 4, 6, 8, 12, 16, 24, 32, 48]


def space_idx(v: float) -> int:
    return min(range(len(SPACE)), key=lambda i: (abs(SPACE[i] - v), i))


def size_fn(v: float) -> str:
    if v <= 10.5:
        f = 's_meta'
    elif v <= 11.5:
        f = 's_small'
    elif v <= 12.5:
        f = 's_body'
    elif v <= 13.5:
        f = 's_emph'
    elif v <= 16.5:
        f = 's_section'
    else:
        f = 's_title'
    return f'crate::ui::text::{f}()'


def migrate(path: str) -> None:
    s = open(path, encoding='utf-8').read()
    before = s
    unknown = []
    # 1. 颜色常量
    for m in re.finditer(r'^\s*(?:pub(?:\(crate\))?\s+)?const (C_[A-Z0-9_]+): *(?:iced::)?Color *=[^;]*;[^\n]*\n', s, re.M):
        name = m.group(1)
        if name not in PAL:
            unknown.append(name)
    for name, fn in PAL.items():
        if name in unknown:
            continue
        # 连同常量上方的文档注释一起删，否则留下孤儿 `///`（clippy: empty_line_after_doc_comments）
        s, n = re.subn(r'(?:^[ \t]*///[^\n]*\n)*^\s*(?:pub(?:\(crate\))?\s+)?const ' + name + r': *(?:iced::)?Color *=[^;]*;[^\n]*\n', '', s, flags=re.M)
        if n:
            s = re.sub(r'\b' + name + r'\b', 'crate::ui::pal::' + fn, s)
    # 2. 字号
    s = re.sub(r'\.size\((\d+(?:\.\d+)?)\)', lambda m: '.size(' + size_fn(float(m.group(1))) + ')', s)
    # 3. 内边距
    def pad1(m):
        v = float(m.group(1))
        if v == 0:
            return '.padding(iced::Padding::ZERO)'
        return f'.padding(crate::ui::metrics::space({space_idx(v)}))'
    s = re.sub(r'\.padding\((\d+(?:\.\d+)?)\)', pad1, s)
    s = re.sub(
        r'\.padding\(\[\s*(\d+(?:\.\d+)?)\s*,\s*(\d+(?:\.\d+)?)\s*\]\)',
        lambda m: f'.padding(crate::ui::metrics::pad2({space_idx(float(m.group(1)))}, {space_idx(float(m.group(2)))}))',
        s,
    )
    if s != before:
        open(path, 'w', encoding='utf-8').write(s)
    print(f'{path}: 迁移完成' + (f'；未识别常量 {unknown}' if unknown else ''))




# ── 行内颜色（批 6–7）──────────────────────────────────────────────────
#
#     python3 scripts/ui_migrate.py --inline status src/ws/recorder_view.rs
#     python3 scripts/ui_migrate.py --inline market src/ws/radar_view.rs
#
# 按色相 / 明度 / 上下文把 `Color::from_rgb(a)(...)` 归到语义：
# - 灰：明度 > 0.72 正文、> 0.58 次要、其余三级
# - 与分组标题 / 大标题同一行（s_section / s_title）的彩色 → 正文色（UPDS：层级靠字号与位置）
# - 绿 / 红：status 模式 → 成功 / 危险；market 模式 → 涨 / 跌（随涨跌约定）
# - 黄橙 → 警告；青蓝 → 信息；紫 → 系列 6
# - from_rgba 保留原透明度（可以是表达式）

import colorsys


def classify(r, g, b, mode, heading):
    h, l, s = colorsys.rgb_to_hls(r, g, b)
    if s < 0.18 or max(r, g, b) - min(r, g, b) < 0.12:
        return 'txt()' if l > 0.72 else ('dim()' if l > 0.58 else 'pend()')
    if heading:
        return 'head()'
    deg = h * 360
    if 80 <= deg < 170:
        return 'up()' if mode == 'market' else 'ok()'
    if deg < 25 or deg >= 330:
        return 'down()' if mode == 'market' else 'bad()'
    if 25 <= deg < 80:
        return 'warn()'
    if 170 <= deg < 255:
        return 'info()'
    return 'series(5)'


NUM = r'\s*(-?\d+(?:\.\d+)?)\s*'


def inline(path, mode):
    lines = open(path, encoding='utf-8').read().split('\n')
    n = 0
    for i, line in enumerate(lines):
        heading = 's_section()' in line or 's_title()' in line

        def rgb(m):
            nonlocal n
            n += 1
            return 'crate::ui::pal::' + classify(float(m.group(1)), float(m.group(2)), float(m.group(3)), mode, heading)

        def rgba(m):
            nonlocal n
            n += 1
            c = classify(float(m.group(1)), float(m.group(2)), float(m.group(3)), mode, heading)
            return f'crate::ui::pal::alpha(crate::ui::pal::{c}, {m.group(4).strip()})'

        line = re.sub(r'(?:iced::)?Color::from_rgb8?\(' + NUM + ',' + NUM + ',' + NUM + r'\)', rgb, line)
        line = re.sub(r'(?:iced::)?Color::from_rgba\(' + NUM + ',' + NUM + ',' + NUM + r',([^()]*(?:\([^()]*\)[^()]*)*)\)', rgba, line)
        lines[i] = line
    open(path, 'w', encoding='utf-8').write('\n'.join(lines))
    print(f'{path}: 行内颜色 {n} 处（{mode} 模式）')


if __name__ == '__main__' and len(sys.argv) > 2 and sys.argv[1] == '--inline':
    for p in sys.argv[3:]:
        inline(p, sys.argv[2])
    sys.exit(0)


if __name__ == '__main__':
    for p in sys.argv[1:]:
        migrate(p)

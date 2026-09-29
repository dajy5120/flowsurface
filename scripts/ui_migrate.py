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


if __name__ == '__main__':
    for p in sys.argv[1:]:
        migrate(p)

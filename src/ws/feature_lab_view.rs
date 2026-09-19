//! 特征库面板 — 视图（docs/30）。
//!
//! ## 这一页的排版顺序是刻意的
//!
//! 1. **样本是否够判定**（够不够，一句话）
//! 2. **多重比较账**（做了几次检验、朴素命中几个、偶然期望几个、FDR 后剩几个）
//! 3. 检验结果表
//! 4. 覆盖健康表
//! 5. 注册表（定义与机制假设）
//!
//! 把 ①② 放在检验结果**之前**，是因为看到一张按 p 值排序的表时，人几乎必然
//! 先盯住最上面那行。先告诉他「35 个检验里偶然就该有 1.75 个 p<0.05」，
//! 他再看那一行时才不会把它当发现。

use iced::widget::{column, container, row, scrollable, text};
use iced::{Color, Element, Length};

use super::feature_lab_readout::{self as ro, FeatureLab};

const C_HEAD: Color = Color::from_rgb(0.70, 0.80, 0.95);
const C_DIM: Color = Color::from_rgb(0.50, 0.54, 0.60);
const C_TXT: Color = Color::from_rgb(0.84, 0.87, 0.92);
const C_OK: Color = Color::from_rgb(0.30, 0.80, 0.48);
const C_BAD: Color = Color::from_rgb(0.90, 0.38, 0.38);
const C_WARN: Color = Color::from_rgb(0.90, 0.72, 0.32);

fn cell<'a, M: 'a>(t: String, w: f32, c: Color) -> Element<'a, M> {
    container(text(t).size(11).color(c)).width(Length::Fixed(w)).into()
}
fn dim<'a, M: 'a>(t: String) -> Element<'a, M> {
    text(t).size(10).color(C_DIM).into()
}
fn sec<'a, M: 'a>(t: String) -> Element<'a, M> {
    text(t).size(14).color(C_HEAD).into()
}
fn num(v: Option<f64>) -> String {
    match v {
        None => "—".into(),
        Some(x) if x == 0.0 => "0".into(),
        Some(x) if x.abs() >= 1e4 || x.abs() < 1e-3 => format!("{x:.2e}"),
        Some(x) => format!("{x:.4}"),
    }
}

/// 族的颜色。分族着色是为了让「某一族整体没信号」这种模式一眼可见——
/// 逐行读 35 行数字是看不出族级结构的。
fn fam_color(f: &str) -> Color {
    match f {
        "ret" => Color::from_rgb(0.55, 0.75, 0.95),
        "vol" => Color::from_rgb(0.85, 0.72, 0.95),
        "flow" => Color::from_rgb(0.40, 0.85, 0.60),
        "impact" => Color::from_rgb(0.95, 0.70, 0.45),
        "geom" => Color::from_rgb(0.70, 0.70, 0.75),
        "pm" => Color::from_rgb(0.95, 0.60, 0.70),
        "cross" => Color::from_rgb(0.60, 0.90, 0.90),
        _ => C_TXT,
    }
}

pub fn pane_body<'a, M: 'a>() -> Element<'a, M> {
    let st: FeatureLab = ro::snapshot();
    let mut b = column![sec("订单流与市场微观结构 · 特征库（docs/30）".into())]
        .spacing(8)
        .padding(10);

    if !st.present {
        return b
            .push(
                text(
                    "暂无快照。生成：python -m factory.prediction.feature_lab\n\
                     （只读已落盘数据，不连交易所）",
                )
                .size(11)
                .color(C_DIM),
            )
            .into();
    }
    if !st.ready {
        return b.push(text(st.reason.clone()).size(12).color(C_WARN)).into();
    }

    // ① 样本是否够判定 —— 放最前面
    let ok = ro::sample_sufficient(&st);
    let (dot, dc, msg) = if ok {
        ("●", C_OK, format!("样本足够：{} 轮 ≥ 需要的 {:.0} 轮", st.power.have, st.power.multi))
    } else {
        (
            "○",
            C_WARN,
            format!(
                "样本不足：现有 {} 轮，检出 2 个概率点的优势需 {:.0} 轮（约 {:.0} 天）——\
                 下面表里的任何「显著」都还不该当成发现",
                st.power.have, st.power.multi, st.power.days
            ),
        )
    };
    b = b.push(row![
        text(format!("{dot} ")).size(15).color(dc),
        text(msg).size(12).color(dc),
    ]);
    b = b.push(dim(format!(
        "{} 轮 / {} 快照，样本 UP 率 {:.3}　快照 {}　刷新 {}",
        st.n_round, st.n_snap, st.base_rate, st.stamp, st.refreshed
    )));

    // ② 多重比较账
    b = b.push(sec("多重比较".into()));
    b = b.push(row![
        cell(format!("检验 {}", st.n_tests), 90.0, C_TXT),
        cell(format!("朴素 p<0.05 命中 {}", st.naive_hits), 160.0,
             if st.naive_hits as f64 > st.expected_by_chance { C_WARN } else { C_DIM }),
        cell(format!("偶然期望 {:.1}", st.expected_by_chance), 110.0, C_DIM),
        cell(format!("FDR(q={:.2}) 通过 {}", st.q, st.fdr_hits), 150.0,
             if st.fdr_hits > 0 { C_OK } else { C_DIM }),
    ]);
    b = b.push(dim(
        "朴素命中要和「偶然期望」比，不是和 0 比。FDR 用 Benjamini–Hochberg 控制\
         被拒绝的那批里假阳性的比例；Bonferroni 会把门槛压到 α/n，真实的 1.5 点优势\
         永远达不到。"
            .into(),
    ));
    b = b.push(dim(format!(
        "单检验检出 2 点需 {:.0} 轮；{} 个检验需 {:.0} 轮——**特征越多，样本要求越高**，\
         这是「全都做进去」的明码标价。",
        st.power.single, st.power.n_tests, st.power.multi
    )));

    // ③ 检验结果
    b = b.push(sec("条件检验 · y ~ logit(市场价) + 特征".into()));
    b = b.push(dim(
        "控制市场价之后特征还有没有**增量**信息。CI 走按轮聚类 bootstrap——\
         同一轮内的快照共享结果标签，按快照算会把区间缩小十几倍。"
            .into(),
    ));
    b = b.push(row![
        cell("特征".into(), 130.0, C_DIM),
        cell("族".into(), 60.0, C_DIM),
        cell("系数".into(), 70.0, C_DIM),
        cell("95%CI".into(), 150.0, C_DIM),
        cell("每σ概率点".into(), 90.0, C_DIM),
        cell("p".into(), 70.0, C_DIM),
        cell("判定".into(), 90.0, C_DIM),
    ]);
    for r in &st.rows {
        let (verdict, vc) = if r.fdr_pass {
            ("FDR 通过", C_OK)
        } else if r.naive_sig {
            ("仅朴素显著", C_WARN)
        } else {
            ("—", C_DIM)
        };
        b = b.push(row![
            cell(r.feature.clone(), 130.0, C_TXT),
            cell(r.family.clone(), 60.0, fam_color(&r.family)),
            cell(format!("{:+.3}", r.coef), 70.0, C_TXT),
            cell(format!("[{:+.3},{:+.3}]", r.ci_lo, r.ci_hi), 150.0, C_DIM),
            cell(format!("{:+.2}", r.pts_per_sd * 100.0), 90.0, C_TXT),
            cell(format!("{:.3}", r.p), 70.0, C_DIM),
            cell(verdict.into(), 90.0, vc),
        ]);
    }

    // ④ 覆盖健康
    b = b.push(sec("覆盖与健康".into()));
    b = b.push(dim(
        "先看这张表再看上面那张：覆盖率低的特征，它的「显著」多半来自剩下那部分的\
         选择偏差；退化（常数或样本过少）的特征会让回归矩阵奇异。"
            .into(),
    ));
    b = b.push(row![
        cell("特征".into(), 130.0, C_DIM),
        cell("族".into(), 60.0, C_DIM),
        cell("覆盖".into(), 70.0, C_DIM),
        cell("p05".into(), 100.0, C_DIM),
        cell("p50".into(), 100.0, C_DIM),
        cell("p95".into(), 100.0, C_DIM),
        cell("".into(), 80.0, C_DIM),
    ]);
    for c in &st.coverage {
        let cc = if !c.present || c.cover < 0.5 {
            C_BAD
        } else if c.cover < 0.9 {
            C_WARN
        } else {
            C_TXT
        };
        let flag = if !c.present {
            "缺列"
        } else if c.degenerate {
            "⚠退化"
        } else if c.control {
            "控制"
        } else {
            ""
        };
        b = b.push(row![
            cell(c.name.clone(), 130.0, C_TXT),
            cell(c.family.clone(), 60.0, fam_color(&c.family)),
            cell(format!("{:.1}%", c.cover * 100.0), 70.0, cc),
            cell(num(c.p05), 100.0, C_DIM),
            cell(num(c.p50), 100.0, C_DIM),
            cell(num(c.p95), 100.0, C_DIM),
            cell(flag.into(), 80.0, if flag.starts_with('⚠') { C_WARN } else { C_DIM }),
        ]);
    }

    // ⑤ 注册表
    b = b.push(sec(format!("注册表 · {} 个特征", st.registry.len())));
    b = b.push(dim(
        "机制假设是**必填**：说不出「它为什么可能预测 5 分钟后的涨跌」的特征不该进库。\
         这是防过拟合的第一道闸，比任何统计检验都靠前。"
            .into(),
    ));
    let mut last_fam = String::new();
    for r in &st.registry {
        if r.family != last_fam {
            last_fam = r.family.clone();
            b = b.push(text(format!("[{}]", r.family)).size(12).color(fam_color(&r.family)));
        }
        b = b.push(row![
            cell(r.name.clone(), 130.0, C_TXT),
            cell(
                r.window_s.map(|w| format!("{w}s")).unwrap_or_else(|| "—".into()),
                55.0,
                C_DIM
            ),
            cell(r.source.clone(), 75.0, C_DIM),
            cell(
                match r.prior {
                    1 => "先验 +".into(),
                    -1 => "先验 −".into(),
                    _ => String::new(),
                },
                60.0,
                C_DIM
            ),
            container(text(r.doc.clone()).size(10).color(C_TXT)).width(Length::Fixed(330.0)),
        ]);
        b = b.push(container(dim(format!("　假设：{}", r.hypothesis))).padding(1));
    }

    container(scrollable(b)).width(Length::Fill).height(Length::Fill).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 族颜色各不相同() {
        // 分族着色的意义在于「某族整体没信号」一眼可见，颜色撞了就失去意义。
        let fams = ["ret", "vol", "flow", "impact", "geom", "pm", "cross"];
        for (i, a) in fams.iter().enumerate() {
            for bb in &fams[i + 1..] {
                let (x, y) = (fam_color(a), fam_color(bb));
                assert!(
                    (x.r - y.r).abs() + (x.g - y.g).abs() + (x.b - y.b).abs() > 0.15,
                    "{a} 与 {bb} 的颜色太接近"
                );
            }
        }
    }

    #[test]
    fn 数字格式不丢小量级() {
        // 波动率特征量级在 1e-5，格式化成 "0.0000" 会让整列看起来全是零
        assert!(num(Some(1.2e-5)).contains('e'));
        assert_eq!(num(None), "—");
        assert_eq!(num(Some(0.0)), "0");
        assert_eq!(num(Some(0.1234)), "0.1234");
    }
}

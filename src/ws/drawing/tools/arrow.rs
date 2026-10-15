//! 箭头（docs/46 G5 的示例工具）：两个锚点之间的线，终点画箭头。**只有这一个文件**——
//! 没改引擎、没改菜单、没改存盘，构建时自动登记。

use super::*;

pub struct Arrow;

pub static TOOL: Arrow = Arrow;

impl ToolDef for Arrow {
    fn kind(&self) -> Kind {
        Kind("arrow")
    }

    fn label(&self) -> &'static str {
        "箭头"
    }

    fn anchors(&self) -> usize {
        2
    }

    fn group(&self) -> &'static str {
        "线条"
    }

    fn shortcut(&self) -> &'static str {
        "Alt+A"
    }

    fn prims(&self, _v: &DrawView, _d: &Drawing, pts: &[Point]) -> Vec<Prim> {
        let (Some(&a), Some(&b)) = (pts.first(), pts.get(1)) else { return Vec::new() };
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len = dx.hypot(dy).max(f32::EPSILON);
        let (ux, uy) = (dx / len, dy / len);
        let (head, half) = (12.0_f32.min(len), 5.0);
        let base = Point::new(b.x - ux * head, b.y - uy * head);
        let tip = vec![b, Point::new(base.x - uy * half, base.y + ux * half), Point::new(base.x + uy * half, base.y - ux * half)];
        vec![Prim::Line(a, base, Role::Main), Prim::Poly(tip, Role::Solid)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrow_is_registered_and_draws_a_head() {
        let k = Kind("arrow");
        assert_eq!(Kind::from_name("arrow"), Some(k), "放进 tools/ 就登记了");
        assert_eq!((k.anchors(), k.clicks(), k.label()), (2, 2, "箭头"));
        assert_eq!(super::super::for_shortcut("a", false), Some(k));
        let d = Drawing::new(k, vec![]);
        let ps = TOOL.prims(&crate::ws::drawing::geometry::tests_view(), &d, &[Point::new(0.0, 0.0), Point::new(100.0, 0.0)]);
        assert_eq!(ps.len(), 2);
        assert!(matches!(&ps[1], Prim::Poly(p, Role::Solid) if p[0] == Point::new(100.0, 0.0)), "箭头尖在终点");
    }
}

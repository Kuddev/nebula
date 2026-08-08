//! 把设计裁定从「文档里的建议」变成「测试里的红线」。
//!
//! # 为什么需要它
//!
//! 2026-07-29 用户反馈：*「这些细节我发现有时候每次加新功能都要说一次；
//! 因此你需要给我们项目 ui 层强约束」*。
//!
//! [`super`] 的模块文档早就写着「层契约（新增代码必须遵守）……不写魔数」，
//! 而 `widgets.rs` 自己就在写 `s(7.0)`、`s(9.0)`。这不是谁不认真——**只靠
//! 注释表达的约束，等价于没有约束**。写新功能的人不会先去读隔壁模块的文档，
//! 而复制粘贴一段能跑的旧代码是所有人的默认动作。
//!
//! 所以这里把约束下沉到 `cargo test`：违规会让构建变红，并且错误信息直接
//! 告诉你正确写法是什么。
//!
//! # 违规数只能降，不能升
//!
//! 存量违规有几十处，一次清完既不现实也不安全（很多在难以回归验证的路径
//! 上）。所以每条规则带一个**预算**＝当前实际违规数：
//!
//! - 新代码引入违规 → 超预算 → 测试失败。**新债被立刻挡住。**
//! - 顺手清理了存量 → 低于预算 → 测试也失败，要求把预算调低。
//!   这一条同样重要：否则预算会随时间虚高，规则慢慢失去约束力。
//!
//! 净效果是违规数**单调下降**，且不需要谁去记得推进它。
//!
//! # 边界
//!
//! 这是文本扫描，不是类型系统，能被绕过（改个写法就躲开匹配）。它防的是
//! 「无意识地复制了旧配方」，不是「蓄意规避」。真正的编译期强制要等
//! `UiQuad` 的圆角参数换成枚举，那是 `nebula_ui` crate 抽出来之后的事。

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::path::{Path, PathBuf};

    /// 圆角实参允许的字面量。`0.0` 是直角，其余走 [`super::super::tokens::radius`]。
    ///
    /// 注意这里比对的是**源码文本**而不是数值：目的就是逼调用方写
    /// `radius::OVERLAY * scale` 而不是碰巧等值的 `s(8.0)`——后者在圆角阶梯
    /// 整体调整时不会跟着变，是"改不干净"的根源。
    const RADIUS_LITERAL_ALLOWLIST: &[&str] = &["0.0"];

    fn source_files() -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let mut out = Vec::new();
        walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src/display"), &mut out);
        // 本文件必须排除：规则是用字符串字面量表达的（`"UiQuad::glow("`），
        // 扫描自己会把规则本身连同它的自测样本一起算成违规。
        out.retain(|path| label(path) != "guardrails.rs");
        out.sort();
        assert!(!out.is_empty(), "扫不到源文件，规则会静默放水——先修路径");
        out
    }

    fn label(path: &Path) -> String {
        path.file_name().unwrap_or_default().to_string_lossy().into_owned()
    }

    /// 顶层逗号分割一次函数调用的实参。嵌套调用（`Rgba::new(r, g, b, a)`）
    /// 里的逗号不算，否则参数序号会错位。
    fn split_args(src: &str, open_paren: usize) -> Option<Vec<String>> {
        let bytes = src.as_bytes();
        let mut depth = 0i32;
        let mut args = Vec::new();
        let mut start = open_paren + 1;
        let mut index = open_paren;
        while index < bytes.len() {
            match bytes[index] {
                b'(' | b'[' => depth += 1,
                b')' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        args.push(src[start..index].trim().to_owned());
                        return Some(args);
                    }
                },
                b',' if depth == 1 => {
                    args.push(src[start..index].trim().to_owned());
                    start = index + 1;
                },
                _ => {},
            }
            index += 1;
        }
        None
    }

    fn line_of(src: &str, offset: usize) -> usize {
        src[..offset].bytes().filter(|&b| b == b'\n').count() + 1
    }

    /// 每次 `UiQuad::solid(x, y, w, h, radius, color)` 的圆角实参。
    fn radius_arguments(src: &str) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        let needle = "UiQuad::solid(";
        let mut from = 0;
        while let Some(found) = src[from..].find(needle) {
            let at = from + found;
            let open = at + needle.len() - 1;
            if let Some(mut args) = split_args(src, open) {
                // rustfmt 给多行调用加尾随逗号，切分后会多出一个空尾项。
                if args.last().is_some_and(String::is_empty) {
                    args.pop();
                }
                if args.len() == 6 {
                    out.push((line_of(src, at), args[4].replace(char::is_whitespace, "")));
                }
            }
            from = at + needle.len();
        }
        out
    }

    /// 圆角是否写死了一个不该写死的字面量。
    ///
    /// 放行的形式：token 常量、以及从别处派生的表达式（`th * 0.5` 的胶囊、
    /// `knob / 2.0` 的圆形、上游传下来的 `corner`）。只揪 `s(常数)` 和裸常数。
    fn is_hardcoded_radius(arg: &str) -> bool {
        let inner = arg.strip_prefix("s(").and_then(|rest| rest.strip_suffix(')')).unwrap_or(arg);
        if !inner.bytes().all(|b| b.is_ascii_digit() || b == b'.') || inner.is_empty() {
            return false;
        }
        !RADIUS_LITERAL_ALLOWLIST.contains(&inner)
    }

    /// 一条数量上限规则的检查。`budget` 是当前实际违规数。
    fn check_budget(
        rule: &str,
        why: &str,
        fix: &str,
        budget: usize,
        hits: Vec<(String, usize, String)>,
    ) {
        let found = hits.len();
        if found == budget {
            return;
        }
        let mut report = String::new();
        let _ = write!(report, "\n\n【UI 强约束】{rule}\n\n{why}\n\n正确写法：{fix}\n\n");
        if found > budget {
            let _ = write!(
                report,
                "违规数 {found} > 预算 {budget}——新增了 {} 处。\n\
                 预算是给存量债留的口子，不接受新债；请改用上面的写法。\n\n\
                 全部命中：\n",
                found - budget
            );
        } else {
            let _ = write!(
                report,
                "违规数 {found} < 预算 {budget}——存量清掉了 {} 处，很好。\n\
                 现在请把本规则的预算改成 {found}，把上限调低一格。\n\
                 （不收紧的话预算会虚高，规则慢慢就管不住人了。）\n\n\
                 剩余命中：\n",
                budget - found
            );
        }
        for (file, line, detail) in hits.iter().take(40) {
            let _ = write!(report, "  {file}:{line}  {detail}\n");
        }
        if found > 40 {
            let _ = write!(report, "  …… 其余 {} 处省略\n", found - 40);
        }
        panic!("{report}");
    }

    #[test]
    fn radius_comes_from_the_token_ladder() {
        let mut hits = Vec::new();
        for path in source_files() {
            let Ok(src) = std::fs::read_to_string(&path) else { continue };
            for (line, arg) in radius_arguments(&src) {
                if is_hardcoded_radius(&arg) {
                    hits.push((label(&path), line, format!("圆角写死为 {arg}")));
                }
            }
        }
        check_budget(
            "圆角必须来自 tokens::radius 阶梯",
            "2026-07-29 侦察：八个浮层用了六种圆角（14/12/12/10/9/8），\n\
             界面因此读不出体系。写死的数字在阶梯整体调整时不会跟着变——\n\
             这正是「配色/圆角改不干净」的结构性原因。",
            "radius::OVERLAY * scale（浮层）/ radius::CONTROL * scale（控件）\n\
                      / radius::CHIP * scale（chip 键帽），或直接用 surface::push_* 配方",
            68,
            hits,
        );
    }

    #[test]
    fn stroke_rings_come_from_the_surface_recipe() {
        let mut hits = Vec::new();
        for path in source_files() {
            let Ok(src) = std::fs::read_to_string(&path) else { continue };
            if label(&path) == "surface.rs" {
                continue; // 配方的定义处，它就是那个唯一来源
            }
            for (index, line) in src.lines().enumerate() {
                if line.contains("- s(1.0)") {
                    hits.push((
                        label(&path),
                        index + 1,
                        "手写的 (x-1, y-1, w+2, h+2) 描边".to_owned(),
                    ));
                }
            }
        }
        check_budget(
            "发丝描边必须走 surface::push_stroke",
            "手写描边普遍内外用同一个半径，圆角处描边会变粗——「边缘发毛」的\n\
             来源之一。push_stroke 让外半径 = 内半径 + 描边宽，内外弧同心。",
            "surface::push_stroke(quads, rect, corner, scale, sk.hairline)",
            43,
            hits,
        );
    }

    #[test]
    fn depth_comes_from_shadow_not_glow() {
        let mut hits = Vec::new();
        for path in source_files() {
            let Ok(src) = std::fs::read_to_string(&path) else { continue };
            let lines: Vec<&str> = src.lines().collect();
            for (index, line) in lines.iter().enumerate() {
                if !line.contains("UiQuad::glow(") {
                    continue;
                }
                // 星云品牌位的粒子/脉冲本来就该发光——那是画面主体，不是在给
                // 浮层垫高度。豁免必须在紧邻的上一行显式写出标记，所以它不会
                // 悄悄扩散：想用就得先写下理由，reviewer 一眼看得见。
                let exempt = index > 0 && lines[index - 1].contains("brand-glow:");
                if !exempt {
                    hits.push((label(&path), index + 1, "用 glow 冒充阴影".to_owned()));
                }
            }
        }
        check_budget(
            "Z 轴层级用 shadow，不用 glow",
            "glow 是「发光」——它向外扩散亮度，不建立高度关系，和外阴影叠加\n\
             还会让边缘浑浊。浮层浮不起来时加 glow 是在解决错误的问题。",
            "surface::push_surface(..., Elevation::Popover, ...)，它内部用 UiQuad::shadow",
            9,
            hits,
        );
    }

    // ---- 扫描器自身的正确性 ----
    //
    // 这套检查的价值完全建立在「数得准」上：漏数会静默放水，多数会挡住无辜的
    // 提交。所以解析器本身要有测试。

    #[test]
    fn nested_calls_do_not_shift_the_argument_index() {
        let src = "UiQuad::solid(x, y, w, h, s(12.0), Rgba::new(1, 2, 3, 4))";
        let args = radius_arguments(src);
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].1, "s(12.0)", "Rgba::new 里的逗号不能被当成顶层分隔");
    }

    #[test]
    fn multi_line_calls_are_counted_at_their_opening_line() {
        let src =
            "line one\nUiQuad::solid(\n    x,\n    y,\n    w,\n    h,\n    s(9.0),\n    color,\n)";
        let args = radius_arguments(src);
        assert_eq!(args, vec![(2, "s(9.0)".to_owned())]);
    }

    #[test]
    fn derived_radii_are_not_flagged() {
        // 胶囊、圆形、上游传下来的 corner 都是合法的——只揪写死的常数。
        for ok in ["0.0", "corner", "th*0.5", "knob/2.0", "radius::OVERLAY*scale", "thumb_r"] {
            assert!(!is_hardcoded_radius(ok), "{ok} 不该被判违规");
        }
        for bad in ["s(12.0)", "s(8.0)", "14.0"] {
            assert!(is_hardcoded_radius(bad), "{bad} 应该被判违规");
        }
    }
}

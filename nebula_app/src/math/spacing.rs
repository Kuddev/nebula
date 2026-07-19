//! TeX 原子间距表和二元运算符上下文归一化。

use super::ir::{AtomClass, MathStyle};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MathSpacing {
    None,
    Thin,
    Medium,
    Thick,
}

/// 将 TeX 的 mu 间距换算为像素；脚本样式会关闭表中带括号的间距项。
pub(crate) fn atom_spacing(left: AtomClass, right: AtomClass, style: MathStyle, em: f32) -> f32 {
    use AtomClass::{Bin, Close, Inner, Op, Open, Ord, Punct, Rel};
    use MathSpacing::{Medium, None, Thick, Thin};

    let spacing = match (left, right) {
        (Ord, Ord | Open | Close | Punct) => None,
        (Ord, Op | Inner) => Thin,
        (Ord, Bin) => Medium,
        (Ord, Rel) => Thick,

        (Op, Ord | Op | Inner) => Thin,
        (Op, Rel) => Thick,
        (Op, Open | Close | Punct) => None,

        (Bin, Ord | Op | Open | Inner) => Medium,

        (Rel, Ord | Op | Open | Inner) => Thick,
        (Rel, Rel | Close | Punct) => None,

        (Open, _) => None,

        (Close, Ord | Open | Close | Punct) => None,
        (Close, Op | Inner) => Thin,
        (Close, Bin) => Medium,
        (Close, Rel) => Thick,

        (Punct, Bin) => None,
        (Punct, _) => Thin,

        (Inner, Ord | Op | Open | Punct | Inner) => Thin,
        (Inner, Bin) => Medium,
        (Inner, Rel) => Thick,
        (Inner, Close) => None,

        // 这些组合会在 `normalize_binary` 中变为 Ord；保留 None 使异常 IR 也确定退化。
        (Op, Bin) | (Bin, Bin | Rel | Close | Punct) | (Rel, Bin) => None,
    };

    if matches!(style, MathStyle::Script | MathStyle::ScriptScript)
        && !matches!(spacing, MathSpacing::None)
    {
        return 0.0;
    }

    let mu = em / 18.0;
    match spacing {
        None => 0.0,
        Thin => 3.0 * mu,
        Medium => 4.0 * mu,
        Thick => 5.0 * mu,
    }
}

/// TeX 会把缺少合法左右操作数的 Bin 原子视为 Ord，避免行首/关系符旁出现运算间距。
pub(crate) fn normalize_binary(classes: &mut [Option<AtomClass>]) {
    let significant: Vec<usize> =
        classes.iter().enumerate().filter_map(|(index, class)| class.map(|_| index)).collect();

    for (position, index) in significant.iter().copied().enumerate() {
        if classes[index] != Some(AtomClass::Bin) {
            continue;
        }
        let left = position.checked_sub(1).and_then(|i| classes[significant[i]]);
        let right = significant.get(position + 1).and_then(|i| classes[*i]);
        let invalid_left = left.is_none()
            || matches!(
                left,
                Some(
                    AtomClass::Bin
                        | AtomClass::Op
                        | AtomClass::Rel
                        | AtomClass::Open
                        | AtomClass::Punct
                )
            );
        let invalid_right = right.is_none()
            || matches!(right, Some(AtomClass::Rel | AtomClass::Close | AtomClass::Punct));
        if invalid_left || invalid_right {
            classes[index] = Some(AtomClass::Ord);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_atoms_are_normalized_at_invalid_boundaries() {
        let mut classes = [
            Some(AtomClass::Bin),
            Some(AtomClass::Ord),
            Some(AtomClass::Bin),
            None,
            Some(AtomClass::Rel),
            Some(AtomClass::Bin),
        ];
        normalize_binary(&mut classes);
        assert_eq!(classes[0], Some(AtomClass::Ord));
        assert_eq!(classes[2], Some(AtomClass::Ord));
        assert_eq!(classes[5], Some(AtomClass::Ord));
    }

    #[test]
    fn script_styles_suppress_inter_atom_spacing() {
        let text = atom_spacing(AtomClass::Ord, AtomClass::Bin, MathStyle::Text, 18.0);
        assert_eq!(text, 4.0);
        assert_eq!(atom_spacing(AtomClass::Ord, AtomClass::Bin, MathStyle::Script, 18.0), 0.0);
    }
}

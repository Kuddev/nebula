//! TeX 输入的线性预算扫描器；任何宏展开都必须晚于这一层。

use super::{MathError, MathErrorKind, MathLimits};

/// 这些命令会定义/展开动态控制序列、读写外部资源或改变 TeX 命令表。
/// 符号和排版命令不在此重复维护白名单，由固定版本 pulldown-latex 继续判定。
const FORBIDDEN_COMMANDS: &[&str] = &[
    "DeclareMathOperator",
    "RequirePackage",
    "catcode",
    "chardef",
    "closein",
    "closeout",
    "countdef",
    "csname",
    "def",
    "delcode",
    "dimendef",
    "directlua",
    "dump",
    "edef",
    "endcsname",
    "everycr",
    "everydisplay",
    "everyhbox",
    "everyjob",
    "everymath",
    "everypar",
    "everyvbox",
    "expandafter",
    "font",
    "fontdimen",
    "futurelet",
    "gdef",
    "global",
    "include",
    "includegraphics",
    "immediate",
    "input",
    "lccode",
    "let",
    "long",
    "lowercase",
    "mathchardef",
    "mathcode",
    "muskipdef",
    "newcommand",
    "newcount",
    "newdimen",
    "newenvironment",
    "newif",
    "newread",
    "newskip",
    "newtoks",
    "newwrite",
    "noexpand",
    "openin",
    "openout",
    "outer",
    "protected",
    "providecommand",
    "read",
    "readline",
    "renewcommand",
    "renewenvironment",
    "shipout",
    "skipdef",
    "special",
    "the",
    "toksdef",
    "uccode",
    "uppercase",
    "usepackage",
    "write",
    "xdef",
];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ValidationStats {
    pub(crate) bytes_scanned: usize,
    pub(crate) control_sequences: usize,
    pub(crate) max_depth: usize,
}

pub(crate) fn validate(source: &str, limits: MathLimits) -> Result<ValidationStats, MathError> {
    if source.len() > limits.max_source_bytes {
        return Err(MathError::new(MathErrorKind::SourceTooLong, limits.max_source_bytes));
    }

    let bytes = source.as_bytes();
    let mut stats = ValidationStats::default();
    let mut groups = Vec::with_capacity(limits.max_depth.min(16));
    let mut environments: Vec<(&str, usize)> = Vec::with_capacity(4);
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            },
            b'\\' => {
                let command_offset = i;
                i += 1;
                if i >= bytes.len() {
                    continue;
                }

                if bytes[i].is_ascii_alphabetic() || bytes[i] == b'@' {
                    let command_start = i;
                    while i < bytes.len() && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'@') {
                        i += 1;
                    }
                    let command = &source[command_start..i];
                    stats.control_sequences += 1;
                    if FORBIDDEN_COMMANDS.contains(&command) {
                        return Err(MathError::new(
                            MathErrorKind::ForbiddenCommand,
                            command_offset,
                        ));
                    }

                    if matches!(command, "begin" | "end") {
                        let Some((name, end)) = environment_name(source, i, command_offset)? else {
                            continue;
                        };
                        i = end;
                        if command == "begin" {
                            environments.push((name, command_offset));
                            update_depth(
                                groups.len(),
                                environments.len(),
                                command_offset,
                                limits,
                                &mut stats,
                            )?;
                        } else {
                            let Some((open, _)) = environments.pop() else {
                                return Err(MathError::new(
                                    MathErrorKind::UnbalancedEnvironment,
                                    command_offset,
                                ));
                            };
                            if open != name {
                                return Err(MathError::new(
                                    MathErrorKind::UnbalancedEnvironment,
                                    command_offset,
                                ));
                            }
                        }
                    }
                } else {
                    // TeX 控制符由反斜杠加一个完整 Unicode 字符组成；被转义的花括号不入栈。
                    i += source[i..].chars().next().map(char::len_utf8).unwrap_or(1);
                }
            },
            b'{' => {
                groups.push(i);
                update_depth(groups.len(), environments.len(), i, limits, &mut stats)?;
                i += 1;
            },
            b'}' => {
                if groups.pop().is_none() {
                    return Err(MathError::new(MathErrorKind::UnbalancedGroup, i));
                }
                i += 1;
            },
            byte if byte.is_ascii() => i += 1,
            _ => i += source[i..].chars().next().map(char::len_utf8).unwrap_or(1),
        }
    }

    if let Some(offset) = groups.last().copied() {
        return Err(MathError::new(MathErrorKind::UnbalancedGroup, offset));
    }
    if let Some((_, offset)) = environments.last().copied() {
        return Err(MathError::new(MathErrorKind::UnbalancedEnvironment, offset));
    }

    stats.bytes_scanned = source.len();
    Ok(stats)
}

fn update_depth(
    groups: usize,
    environments: usize,
    offset: usize,
    limits: MathLimits,
    stats: &mut ValidationStats,
) -> Result<(), MathError> {
    let depth = groups.saturating_add(environments);
    if depth > limits.max_depth {
        return Err(MathError::new(MathErrorKind::NestingTooDeep, offset));
    }
    stats.max_depth = stats.max_depth.max(depth);
    Ok(())
}

fn environment_name(
    source: &str,
    mut offset: usize,
    command_offset: usize,
) -> Result<Option<(&str, usize)>, MathError> {
    let bytes = source.as_bytes();
    while offset < bytes.len() && bytes[offset].is_ascii_whitespace() {
        offset += 1;
    }
    if bytes.get(offset) != Some(&b'{') {
        return Ok(None);
    }

    let name_start = offset + 1;
    let Some(relative_end) = bytes[name_start..].iter().position(|byte| *byte == b'}') else {
        return Err(MathError::new(MathErrorKind::UnbalancedEnvironment, command_offset));
    };
    let name_end = name_start + relative_end;
    if bytes[name_start..name_end].contains(&b'{') || name_start == name_end {
        return Err(MathError::new(MathErrorKind::UnbalancedEnvironment, command_offset));
    }
    Ok(Some((&source[name_start..name_end], name_end + 1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::{DEFAULT_LIMITS, MathErrorKind};

    #[test]
    fn source_byte_limit_is_exact_for_utf8_input() {
        let valid = "x".repeat(DEFAULT_LIMITS.max_source_bytes);
        assert_eq!(validate(&valid, DEFAULT_LIMITS).unwrap().bytes_scanned, valid.len());

        let invalid = format!("{valid}x");
        assert_eq!(
            validate(&invalid, DEFAULT_LIMITS).unwrap_err().kind,
            MathErrorKind::SourceTooLong
        );
    }

    #[test]
    fn group_and_environment_depth_share_one_budget() {
        let valid = format!("{}x{}", "{".repeat(64), "}".repeat(64));
        assert_eq!(validate(&valid, DEFAULT_LIMITS).unwrap().max_depth, 64);

        let invalid = format!("{}x{}", "{".repeat(65), "}".repeat(65));
        assert_eq!(
            validate(&invalid, DEFAULT_LIMITS).unwrap_err().kind,
            MathErrorKind::NestingTooDeep
        );

        let nested = r"\begin{matrix}\begin{aligned}x\end{aligned}\end{matrix}";
        assert_eq!(validate(nested, DEFAULT_LIMITS).unwrap().max_depth, 2);
    }

    #[test]
    fn escaped_braces_and_comments_do_not_mutate_scanner_state() {
        let source = "\\{x\\} % \\def\\x{bad}\n\\frac{1}{2}";
        let stats = validate(source, DEFAULT_LIMITS).unwrap();
        assert_eq!(stats.bytes_scanned, source.len());
        assert_eq!(stats.max_depth, 1);
    }

    #[test]
    fn command_table_and_io_mutation_are_rejected_before_parser() {
        for command in [
            "def",
            "gdef",
            "edef",
            "xdef",
            "let",
            "futurelet",
            "newcommand",
            "renewcommand",
            "providecommand",
            "newenvironment",
            "renewenvironment",
            "DeclareMathOperator",
            "catcode",
            "mathcode",
            "csname",
            "input",
            "include",
            "openin",
            "openout",
            "read",
            "write",
            "includegraphics",
            "usepackage",
            "directlua",
            "special",
        ] {
            let source = format!("\\{command} x");
            let error = validate(&source, DEFAULT_LIMITS).unwrap_err();
            assert_eq!(error.kind, MathErrorKind::ForbiddenCommand, "{command}");
            assert_eq!(error.source_offset, 0, "{command}");
        }
    }

    #[test]
    fn malformed_groups_and_environments_have_deterministic_errors() {
        assert_eq!(validate("}", DEFAULT_LIMITS).unwrap_err().kind, MathErrorKind::UnbalancedGroup);
        assert_eq!(
            validate("{x", DEFAULT_LIMITS).unwrap_err().kind,
            MathErrorKind::UnbalancedGroup
        );
        assert_eq!(
            validate(r"\begin{matrix}x\end{cases}", DEFAULT_LIMITS).unwrap_err().kind,
            MathErrorKind::UnbalancedEnvironment
        );
    }

    #[test]
    fn common_tier_one_formula_is_linear_and_allowed() {
        let source = r"\left(\frac{\alpha_i}{\sqrt[3]{x}}\right)+\sum_{n=0}^{\infty}n";
        let stats = validate(source, DEFAULT_LIMITS).unwrap();
        assert_eq!(stats.bytes_scanned, source.len());
        assert!(stats.control_sequences >= 7);
        assert!(stats.max_depth <= DEFAULT_LIMITS.max_depth);
    }
}

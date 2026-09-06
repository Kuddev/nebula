pub(super) fn substitute(template: &str, args: &[(&str, &str)]) -> String {
    let extra = args.iter().map(|(_, value)| value.len()).sum::<usize>();
    let mut output = String::with_capacity(template.len().saturating_add(extra));
    let mut rest = template;
    while let Some(position) = rest.find(['{', '}']) {
        output.push_str(&rest[..position]);
        rest = &rest[position..];
        if rest.starts_with("{{") || rest.starts_with("}}") {
            output.push_str(&rest[..1]);
            rest = &rest[2..];
            continue;
        }
        if !rest.starts_with('{') {
            output.push('}');
            rest = &rest[1..];
            continue;
        }
        let Some(end) = rest.find('}') else { break };
        let name = &rest[1..end];
        let replacement = args.iter().find(|(argument, _)| *argument == name);
        output.push_str(replacement.map(|(_, value)| *value).unwrap_or(&rest[..end + 1]));
        rest = &rest[end + 1..];
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::substitute;

    #[test]
    fn argument_values_are_never_interpreted_as_more_placeholders() {
        assert_eq!(
            substitute("{name}: {status}", &[("name", "{status}"), ("status", "200")]),
            "{status}: 200"
        );
    }

    #[test]
    fn preserves_unicode_missing_arguments_and_escaped_braces() {
        assert_eq!(
            substitute("你好 {name} / {missing} {{code}}", &[("name", "世界")]),
            "你好 世界 / {missing} {code}"
        );
        assert_eq!(substitute("unfinished {name", &[]), "unfinished {name");
    }
}

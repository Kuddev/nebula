use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use serde::de::{self, MapAccess, Visitor};

enum Node {
    Text(String),
    Group(BTreeMap<String, Node>),
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<Deserializer>(deserializer: Deserializer) -> Result<Self, Deserializer::Error>
    where
        Deserializer: de::Deserializer<'de>,
    {
        struct NodeVisitor;
        impl<'de> Visitor<'de> for NodeVisitor {
            type Value = Node;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a translation string or object")
            }

            fn visit_str<Error: de::Error>(self, value: &str) -> Result<Node, Error> {
                Ok(Node::Text(value.to_owned()))
            }

            fn visit_string<Error: de::Error>(self, value: String) -> Result<Node, Error> {
                Ok(Node::Text(value))
            }

            fn visit_map<Map: MapAccess<'de>>(self, mut map: Map) -> Result<Node, Map::Error> {
                let mut entries = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, Node>()? {
                    if key.is_empty() || entries.insert(key.clone(), value).is_some() {
                        return Err(de::Error::custom(format!("empty or duplicate key: {key:?}")));
                    }
                }
                Ok(Node::Group(entries))
            }
        }
        deserializer.deserialize_any(NodeVisitor)
    }
}

pub fn parse(source: &str) -> Result<BTreeMap<String, String>, String> {
    fn flatten(
        node: Node,
        prefix: &str,
        output: &mut BTreeMap<String, String>,
    ) -> Result<(), String> {
        match node {
            Node::Text(message) => {
                if prefix.is_empty() || message.trim().is_empty() {
                    return Err(format!("empty message or id: {prefix:?}"));
                }
                if output.insert(prefix.to_owned(), message).is_some() {
                    return Err(format!("duplicate flattened id: {prefix}"));
                }
            },
            Node::Group(entries) => {
                for (key, value) in entries {
                    let key = if prefix.is_empty() { key } else { format!("{prefix}.{key}") };
                    flatten(value, &key, output)?;
                }
            },
        }
        Ok(())
    }
    let root = serde_json::from_str(source).map_err(|error| error.to_string())?;
    let mut output = BTreeMap::new();
    flatten(root, "", &mut output)?;
    Ok(output)
}

pub fn placeholders(message: &str) -> Result<std::collections::BTreeSet<&str>, String> {
    let mut names = std::collections::BTreeSet::new();
    let mut rest = message;
    while let Some(position) = rest.find(['{', '}']) {
        rest = &rest[position..];
        if rest.starts_with("{{") || rest.starts_with("}}") {
            rest = &rest[2..];
            continue;
        }
        if !rest.starts_with('{') {
            return Err(format!("unmatched closing brace: {message}"));
        }
        let end = rest.find('}').ok_or_else(|| format!("unclosed placeholder: {message}"))?;
        let name = &rest[1..end];
        if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(format!("invalid placeholder {name:?}"));
        }
        names.insert(name);
        rest = &rest[end + 1..];
    }
    Ok(names)
}

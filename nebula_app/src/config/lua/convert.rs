use std::collections::HashSet;
use std::fmt::{self, Display, Formatter};

use mlua::{Lua, Table, Value};

use super::runtime::{ARRAY_MARKER, BUILDER_STORE};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaValueError {
    pub field_path: String,
    pub message: String,
}

impl Display for LuaValueError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        if self.field_path.is_empty() {
            formatter.write_str(&self.message)
        } else {
            write!(formatter, "{}: {}", self.field_path, self.message)
        }
    }
}

impl std::error::Error for LuaValueError {}

pub fn lua_value_to_toml(lua: &Lua, value: Value) -> Result<toml::Value, LuaValueError> {
    convert(lua, value, String::new(), &mut HashSet::new())
}

fn convert(
    lua: &Lua,
    value: Value,
    path: String,
    active_tables: &mut HashSet<usize>,
) -> Result<toml::Value, LuaValueError> {
    match value {
        Value::Boolean(value) => Ok(toml::Value::Boolean(value)),
        Value::Integer(value) => Ok(toml::Value::Integer(value)),
        Value::Number(value) if value.is_finite() => Ok(toml::Value::Float(value)),
        Value::Number(_) => Err(error(path, "non-finite numbers are not valid configuration")),
        Value::String(value) => value
            .to_str()
            .map(|value| toml::Value::String(value.to_owned()))
            .map_err(|_| error(path, "configuration strings must be valid UTF-8")),
        Value::Table(table) => convert_table(lua, table, path, active_tables),
        other => Err(error(
            path,
            format!("Lua value of type '{}' cannot be used in configuration", other.type_name()),
        )),
    }
}

fn convert_table(
    lua: &Lua,
    table: Table,
    path: String,
    active_tables: &mut HashSet<usize>,
) -> Result<toml::Value, LuaValueError> {
    let address = table.to_pointer() as usize;
    if !active_tables.insert(address) {
        return Err(error(path, "recursive Lua tables are not supported"));
    }

    let result = (|| {
        let table = builder_store(&table).unwrap_or(table);
        let explicit_array = table
            .metatable()
            .and_then(|metatable| metatable.get::<bool>(ARRAY_MARKER).ok())
            .unwrap_or(false);

        let mut integer_entries = Vec::new();
        let mut string_entries = Vec::new();
        for pair in table.clone().pairs::<Value, Value>() {
            let (key, value) = pair.map_err(|err| error(path.clone(), err.to_string()))?;
            match key {
                Value::Integer(index) if index > 0 => integer_entries.push((index, value)),
                Value::String(key) => {
                    let key = key
                        .to_str()
                        .map_err(|_| error(path.clone(), "configuration keys must be valid UTF-8"))?
                        .to_owned();
                    string_entries.push((key, value));
                },
                other => {
                    return Err(error(
                        path.clone(),
                        format!(
                            "configuration table keys must be strings or positive integers, got {}",
                            other.type_name()
                        ),
                    ));
                },
            }
        }

        if !integer_entries.is_empty() && !string_entries.is_empty() {
            return Err(error(path, "mixed array and object tables are not supported"));
        }
        if explicit_array && !string_entries.is_empty() {
            return Err(error(path, "nebula.array() cannot contain string keys"));
        }

        if explicit_array || !integer_entries.is_empty() {
            integer_entries.sort_by_key(|(index, _)| *index);
            let mut values = Vec::with_capacity(integer_entries.len());
            for (offset, (index, value)) in integer_entries.into_iter().enumerate() {
                let expected = offset as i64 + 1;
                if index != expected {
                    return Err(error(
                        index_path(&path, expected),
                        format!("sparse Lua array: expected index {expected}, found {index}"),
                    ));
                }
                values.push(convert(lua, value, index_path(&path, index), active_tables)?);
            }
            return Ok(toml::Value::Array(values));
        }

        let mut values = toml::Table::new();
        for (key, value) in string_entries {
            let child_path = field_path(&path, &key);
            values.insert(key, convert(lua, value, child_path, active_tables)?);
        }
        Ok(toml::Value::Table(values))
    })();

    active_tables.remove(&address);
    result
}

fn builder_store(table: &Table) -> Option<Table> {
    table.metatable()?.get::<Table>(BUILDER_STORE).ok()
}

fn field_path(parent: &str, field: &str) -> String {
    if parent.is_empty() { field.to_owned() } else { format!("{parent}.{field}") }
}

fn index_path(parent: &str, index: i64) -> String {
    if parent.is_empty() { format!("[{index}]") } else { format!("{parent}[{index}]") }
}

fn error(path: String, message: impl Into<String>) -> LuaValueError {
    LuaValueError { field_path: path, message: message.into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_objects_arrays_and_unicode() {
        let lua = Lua::new();
        let value = lua
            .load(
                r#"return {
                    title = "中文 🚀",
                    enabled = true,
                    count = 7,
                    ratio = 0.5,
                    items = { "a", "b" },
                    nested = { key = "value" },
                }"#,
            )
            .eval()
            .unwrap();

        let converted = lua_value_to_toml(&lua, value).unwrap();
        assert_eq!(converted["title"].as_str(), Some("中文 🚀"));
        assert_eq!(converted["items"].as_array().unwrap().len(), 2);
        assert_eq!(converted["nested"]["key"].as_str(), Some("value"));
    }

    #[test]
    fn rejects_mixed_sparse_nonfinite_and_recursive_tables() {
        let lua = Lua::new();
        for source in [
            "local t={ [1]='a', key='b' }; return t",
            "return { [1]='a', [3]='c' }",
            "return { value=0/0 }",
            "local t={}; t.self=t; return t",
        ] {
            let value = lua.load(source).eval().unwrap();
            assert!(lua_value_to_toml(&lua, value).is_err(), "{source}");
        }
    }

    #[test]
    fn array_marker_disambiguates_empty_table() {
        let lua = Lua::new();
        let empty = lua.create_table().unwrap();
        let metatable = lua.create_table().unwrap();
        metatable.set(ARRAY_MARKER, true).unwrap();
        empty.set_metatable(Some(metatable)).unwrap();
        assert!(lua_value_to_toml(&lua, Value::Table(empty)).unwrap().is_array());
    }
}

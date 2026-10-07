use std::io::{self, Write};

use clap::ValueEnum;
use comfy_table::Table;
use serde::Serialize;
use serde_json::Value;

pub(crate) fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace(['_', '-', '.'], "");
    [
        "password",
        "passwd",
        "passphrase",
        "presharedkey",
        "psk",
        "secret",
        "token",
        "credential",
        "apikey",
        "privatekey",
        "authorization",
    ]
    .iter()
    .any(|word| key.contains(word))
}

/// Hide secret-valued fields using the same rules as the raw API command.
pub fn redact_secrets(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if sensitive_key(key) {
                    *child = Value::String("<redacted>".into());
                } else {
                    redact_secrets(child);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact_secrets),
        _ => {}
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Format {
    Json,
    Yaml,
    Table,
}

impl Format {
    pub fn resolve(requested: Option<Self>, is_terminal: bool) -> Self {
        requested.unwrap_or(if is_terminal { Self::Table } else { Self::Json })
    }
}

pub fn write_bytes(writer: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    writer.write_all(bytes)?;
    writer.flush()
}

pub fn write_data(
    writer: &mut impl Write,
    format: Format,
    data: &impl Serialize,
) -> anyhow::Result<()> {
    let mut text = match format {
        Format::Json => serde_json::to_string_pretty(data)?,
        Format::Yaml => serde_yaml_ng::to_string(data)?,
        Format::Table => {
            let value = serde_json::to_value(data)?;
            let mut table = Table::new();
            match &value {
                serde_json::Value::Object(object) => {
                    table.set_header(object.keys().map(|key| key.to_uppercase()));
                    table.add_row(object.values().map(table_cell));
                }
                serde_json::Value::Array(items) => {
                    if items.iter().all(serde_json::Value::is_object)
                        && items
                            .iter()
                            .any(|item| item.as_object().is_some_and(|row| !row.is_empty()))
                    {
                        let mut columns = Vec::new();
                        for item in items {
                            for key in item.as_object().expect("object row").keys() {
                                if !columns.contains(key) {
                                    columns.push(key.clone());
                                }
                            }
                        }
                        table.set_header(columns.iter().map(|key| key.to_uppercase()));
                        for item in items {
                            table.add_row(columns.iter().map(|key| table_cell(&item[key])));
                        }
                    } else {
                        table.set_header(["DATA"]);
                        for item in items {
                            table.add_row([table_cell(item)]);
                        }
                    }
                }
                other => {
                    table.set_header(["DATA"]);
                    table.add_row([table_cell(other)]);
                }
            }
            table.to_string()
        }
    };
    if !text.ends_with('\n') {
        text.push('\n');
    }
    write_bytes(writer, text.as_bytes())?;
    Ok(())
}

fn table_cell(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tracks_terminal_and_override_wins() {
        assert_eq!(Format::resolve(None, true), Format::Table);
        assert_eq!(Format::resolve(None, false), Format::Json);
        assert_eq!(Format::resolve(Some(Format::Json), true), Format::Json);
        assert_eq!(Format::resolve(Some(Format::Table), false), Format::Table);
    }

    #[test]
    fn raw_api_arrays_and_scalars_can_be_rendered_as_tables() {
        for value in [serde_json::json!(["edge", 7]), serde_json::json!(42)] {
            let mut output = Vec::new();
            write_data(&mut output, Format::Table, &value).unwrap();
            let output = String::from_utf8(output).unwrap();
            assert!(output.contains("DATA"));
            match value {
                serde_json::Value::Array(_) => {
                    assert!(output.contains("edge"));
                    assert!(output.contains('7'));
                }
                _ => assert!(output.contains("42")),
            }
        }
    }

    #[test]
    fn yaml_output_preserves_trailing_newlines_in_api_strings() {
        let value = "line\n\n";
        let mut output = Vec::new();
        write_data(&mut output, Format::Yaml, &value).unwrap();
        let decoded: String = serde_yaml_ng::from_slice(&output).unwrap();
        assert_eq!(decoded, value);
    }

    #[test]
    fn object_rows_use_insertion_order_union_missing_null_and_compact_nested_values() {
        let mut row = serde_json::Map::new();
        row.insert("name".into(), serde_json::json!("AP"));
        row.insert("model".into(), serde_json::Value::Null);
        let mut later = serde_json::Map::new();
        later.insert("name".into(), serde_json::json!("Switch"));
        later.insert("ports".into(), serde_json::json!([1, 2]));
        let rows = serde_json::Value::Array(vec![row.into(), later.into()]);
        let mut bytes = Vec::new();
        write_data(&mut bytes, Format::Table, &rows).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.find("NAME").unwrap() < text.find("MODEL").unwrap());
        assert!(text.find("MODEL").unwrap() < text.find("PORTS").unwrap());
        assert!(text.contains("null"));
        assert!(text.contains("[1,2]"));
        assert!(!text.contains("DATA"));
    }

    #[test]
    fn zero_column_object_rows_keep_the_data_cells() {
        let mut bytes = Vec::new();
        write_data(&mut bytes, Format::Table, &serde_json::json!([{}, {}])).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("DATA"));
        assert_eq!(text.matches("{}").count(), 2);
    }
}

mod schema;
mod validation;

use gel_protocol::value::Value as GelValue;
use std::collections::HashMap;
use std::path::PathBuf;
use toml::Value as TomlValue;

use crate::connect::Connection;

pub async fn sync_config(local_toml: &PathBuf, conn: &mut Connection) -> anyhow::Result<()> {
    if !local_toml.exists() {
        return Ok(());
    }

    // read toml
    let local_conf = tokio::fs::read_to_string(local_toml).await?;
    let toml = toml::de::Deserializer::new(&local_conf);
    let local_conf: LocalConfig = serde_path_to_error::deserialize(toml)?;
    let Some(config) = local_conf.local.and_then(|l| l.config) else {
        // there is no [config] table, don't sync
        return Ok(());
    };

    // validate
    let schema = schema::default_schema();
    let flat_config = validation::validate(config, &schema)?;

    // configure
    conn.execute("START TRANSACTION;", &()).await?;
    configure(conn, flat_config).await?;
    conn.execute("COMMIT;", &()).await?;

    Ok(())
}

async fn configure(
    conn: &mut Connection,
    flat_config: HashMap<String, Value>,
) -> anyhow::Result<()> {
    for (name, value) in flat_config {
        match value {
            Value::Nested { values, .. } => {
                for (key, value) in values {
                    set_value(conn, value, &name, &key).await?;
                }
            }
            Value::Set(values) => {
                let query = format!("configure current branch reset {name};");
                println!("Executing query: {query}");
                for value in values {
                    insert_value(conn, value).await?;
                }
            }
            _ => {
                panic!("expected object or set, got {value:?}");
            }
        }
    }
    Ok(())
}

async fn set_value(
    conn: &mut Connection,
    value: Value,
    config: &str,
    name: &str,
) -> anyhow::Result<()> {
    let config = match config {
        "cfg::Config" => "cfg",
        config => config,
    };
    match value {
        Value::Injected(value) => {
            let query = format!("configure current branch set {config}::{name} := {value};");
            println!("Executing query: {query}");
            conn.execute(&query, &()).await?;
        }
        Value::Set(values) => {
            let mut args = HashMap::new();
            let values = values
                .into_iter()
                .map(|value| {
                    let (value, value_args) = value.compile(args.len());
                    args.extend(value_args);
                    value
                })
                .collect::<Vec<_>>()
                .join(",\n\t");
            let query =
                format!("configure current branch set {config}::{name} := {{\n\t{values}\n}};");
            println!("Executing query: {query}\n\twith args: {args:?}");
            let args = args
                .iter()
                .map(|(k, v)| (k.as_str(), v.clone().into()))
                .collect::<HashMap<_, gel_protocol::value_opt::ValueOpt>>();
            conn.execute(&query, &args).await?;
        }
        Value::Array(values) => {
            let mut args = HashMap::new();
            let values = values
                .into_iter()
                .map(|v| {
                    let (value, value_args) = v.compile(args.len());
                    args.extend(value_args);
                    value
                })
                .collect::<Vec<_>>()
                .join(",\n\t");
            let query =
                format!("configure current branch set {config}::{name} := [\n\t{values}\n];");
            println!("Executing query: {query}\n\twith args: {args:?}");
            let args = args
                .iter()
                .map(|(k, v)| (k.as_str(), v.clone().into()))
                .collect::<HashMap<_, gel_protocol::value_opt::ValueOpt>>();
            conn.execute(&query, &args).await?;
        }
        _ => {
            anyhow::bail!("Unsupported value type for setting: {value:?}");
        }
    }
    Ok(())
}

async fn insert_value(conn: &mut Connection, value: Value) -> anyhow::Result<()> {
    let Value::Nested { typ, values } = value else {
        anyhow::bail!("Unsupported value type for inserting: {value:?}");
    };
    let mut args = HashMap::new();
    let values = values
        .into_iter()
        .map(|(name, val)| {
            let (val, value_args) = val.compile(args.len());
            args.extend(value_args);
            format!("{name} := {val}")
        })
        .collect::<Vec<_>>()
        .join(",\n\t");
    let query = format!("configure current branch insert {typ} {{\n\t{values}\n}};");
    println!("Executing query: {query}");
    let args = args
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone().into()))
        .collect::<HashMap<_, gel_protocol::value_opt::ValueOpt>>();
    conn.execute(&query, &args).await?;
    Ok(())
}

/// Format of gel.local.toml
#[derive(Debug, serde::Deserialize)]
pub struct LocalConfig {
    local: Option<Local>,
}

#[derive(Debug, serde::Deserialize)]
pub struct Local {
    config: Option<TomlValue>,
}

#[derive(Debug)]
pub enum Value {
    Injected(String),
    Array(Vec<Value>),
    Set(Vec<Value>),
    Nested {
        typ: String,
        values: HashMap<String, Value>,
    },
}

impl TryFrom<TomlValue> for Value {
    type Error = anyhow::Error;

    fn try_from(value: TomlValue) -> anyhow::Result<Self> {
        Ok(match value {
            TomlValue::String(value) => {
                match value.strip_prefix("{{").and_then(|s| s.strip_suffix("}}")) {
                    Some(value) => Value::Injected(value.to_string()),
                    None => Value::Injected(value.into()),
                }
            }
            TomlValue::Integer(value) => Value::Injected(value.to_string()),
            TomlValue::Float(value) => Value::Injected(value.to_string()),
            TomlValue::Boolean(value) => Value::Injected(value.to_string()),
            TomlValue::Datetime(
                datetimetz @ toml::value::Datetime {
                    date: Some(_),
                    time: Some(_),
                    offset: Some(_),
                },
            ) => Value::Injected(format!("<datetime>{}", datetimetz)),
            TomlValue::Datetime(
                datetime @ toml::value::Datetime {
                    date: Some(_),
                    time: Some(_),
                    offset: None,
                },
            ) => Value::Injected(format!("<cal::local_datetime>{}", datetime)),
            TomlValue::Datetime(toml::value::Datetime {
                date: Some(date),
                time: None,
                offset: None,
            }) => Value::Injected(format!("<cal::local_date>{}", date)),
            TomlValue::Datetime(toml::value::Datetime {
                date: None,
                time: Some(time),
                offset: None,
            }) => Value::Injected(format!("<cal::local_time>{}", time,)),
            TomlValue::Datetime(value) => {
                Err(anyhow::anyhow!("Invalid datetime value: {}", value))?
            }
            TomlValue::Array(values) => {
                let values = values
                    .into_iter()
                    .map(Self::try_from)
                    .collect::<anyhow::Result<Vec<_>>>()?;
                if values.iter().any(Value::is_object) {
                    Value::Set(values)
                } else {
                    Value::Array(values)
                }
            }
            TomlValue::Table(mut table) => {
                let Some(TomlValue::String(typ)) = table.remove("_tname") else {
                    anyhow::bail!("missing tname");
                };
                let values = table
                    .into_iter()
                    .map(|(k, v)| Self::try_from(v).map(|v| (k, v)))
                    .collect::<anyhow::Result<_>>()?;
                Self::Nested { typ, values }
            }
        })
    }
}

impl Value {
    pub fn is_object(&self) -> bool {
        match self {
            Value::Set(values) => values.iter().any(Self::is_object),
            Value::Nested { .. } => true,
            _ => false,
        }
    }

    fn compile(self, _arg_index: usize) -> (String, HashMap<String, GelValue>) {
        match self {
            Value::Injected(value) => (value, HashMap::new()),
            _ => {
                panic!("Unsupported value type to compile: {self:?}");
            }
        }
    }
}

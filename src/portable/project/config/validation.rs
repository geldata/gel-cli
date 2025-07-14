use anyhow::Context;
use edgeql_parser::helpers::quote_string as ql;
use indexmap::IndexMap;
use indexmap::map::Entry;
use toml::Value as TomlValue;

use super::Value;
use super::schema::{Property, PropertyKind, Schema};

pub fn validate(value: TomlValue, schema: &Schema) -> anyhow::Result<IndexMap<String, Value>> {
    let result = schema.validate(value, &[])?;

    let Some(mut flat_config) = result.flat_config else {
        return Ok(Default::default());
    };

    dbg!(&result.result);
    dbg!(&flat_config);

    merge_flat_config(
        &mut flat_config,
        [(
            schema
                .get_type()
                .expect("root schema must have type")
                .to_string(),
            result.result,
        )],
    )?;
    dbg!(&flat_config);
    Ok(flat_config)
}

impl Schema {
    /// Compares the schema to the toml value and makes sure that the value is of correct type.
    pub fn validate(&self, value: TomlValue, path: &[&str]) -> anyhow::Result<ValidateResult> {
        use Schema::*;
        use TomlValue as Toml;

        match (self, value) {
            (_, Toml::String(value)) if value.starts_with("{{") && value.ends_with("}}") => {
                Ok(Value::Injected(
                    value
                        .strip_prefix("{{")
                        .and_then(|s| s.strip_suffix("}}"))
                        .unwrap()
                        .to_string(),
                )
                .into())
            }
            (Primitive { typ } | Enum { typ, .. }, Toml::Array(_)) => {
                Err(anyhow::anyhow!("expected {typ} but got array"))
            }
            (Primitive { typ } | Enum { typ, .. }, Toml::Table(_)) => {
                Err(anyhow::anyhow!("expected {typ} but got table"))
            }
            (Primitive { typ }, value) => match (typ.as_str(), value) {
                ("str", Toml::String(value)) => Ok(Value::Injected(ql(&value)).into()),
                ("int64", Toml::Integer(value)) => Ok(Value::Injected(value.to_string()).into()),
                ("int32", Toml::Integer(value)) => Ok(Value::Injected(value.to_string()).into()),
                ("int16", Toml::Integer(value)) => Ok(Value::Injected(value.to_string()).into()),
                ("float64", Toml::Float(value)) => Ok(Value::Injected(value.to_string()).into()),
                ("float32", Toml::Float(value)) => Ok(Value::Injected(value.to_string()).into()),
                ("bool", Toml::Boolean(value)) => Ok(Value::Injected(value.to_string()).into()),
                ("duration", Toml::String(value)) => {
                    Ok(Value::Injected(format!("<duration>{}", ql(&value))).into())
                }
                (_, value) => Err(anyhow::anyhow!("expected {typ} but got {value:?}")),
            },
            (Enum { typ, choices }, Toml::String(value)) => {
                if choices.contains(&value) {
                    Ok(Value::Injected(format!("<{}>{}", typ, ql(&value))).into())
                } else {
                    Err(anyhow::anyhow!(
                        "expected one of {choices:?} but got {value}"
                    ))
                }
            }
            (Object { .. }, Toml::Table(value)) => Ok(self.validate_object(value, path)?),
            (Union(schemas), Toml::Table(mut value)) => {
                if let Some(Toml::String(tname)) = value.remove("_tname") {
                    for schema in schemas {
                        if schema.get_type() == Some(&tname) {
                            return schema.validate_object(value, path);
                        }
                    }
                    Err(anyhow::anyhow!("unknown type in union: {tname}"))
                } else {
                    Err(anyhow::anyhow!("expected _tname field in union object"))
                }
            }
            (schema, value) => Err(anyhow::anyhow!("expected {schema:?}, value: {value:?}")),
        }
        .with_context(|| path.join("."))
    }
    fn validate_array(&self, value: TomlValue, path: &[&str]) -> anyhow::Result<Vec<Value>> {
        let ctx = || path.join(".");
        let TomlValue::Array(array) = value else {
            return Err(anyhow::anyhow!("expected array")).with_context(ctx);
        };
        Ok(array
            .into_iter()
            .enumerate()
            .map(|(i, v)| {
                let i = i.to_string();
                let sub_path = &[path, &[&i]].concat();
                self.validate(v, sub_path)
                    .and_then(|r| r.take_result().with_context(|| sub_path.join(".")))
            })
            .collect::<anyhow::Result<_>>()?)
    }
    fn validate_object_array(
        &self,
        array: toml::value::Array,
        path: &[&str],
        flat_config: &mut IndexMap<String, Value>,
    ) -> anyhow::Result<Vec<Value>> {
        array
            .into_iter()
            .enumerate()
            .map(|(i, value)| {
                let i = i.to_string();
                let sub_path = &[path, &[&i]].concat();
                self.validate(value, sub_path).and_then(|v| {
                    v.merge_into(flat_config)
                        .with_context(|| sub_path.join("."))
                })
            })
            .collect()
    }

    fn validate_object(
        &self,
        value: toml::value::Table,
        path: &[&str],
    ) -> anyhow::Result<ValidateResult> {
        use PropertyKind::*;

        let Schema::Object { typ, members } = self else {
            panic!("{}: expected object schema", path.join("."));
        };
        let mut flat_config = IndexMap::new();
        let mut values = IndexMap::new();

        for (key, value) in value {
            let sub_path = &[path, &[&key]].concat();
            let key = key.clone();
            let sub_ctx = || sub_path.join(".");
            match members.get(&key) {
                Some(Property {
                    kind: Singleton(schema),
                    ..
                }) if schema.is_scalar() => {
                    values.insert(
                        key,
                        schema
                            .validate(value, sub_path)?
                            .take_result()
                            .with_context(sub_ctx)?,
                    );
                }
                Some(Property {
                    kind: Singleton(schema),
                    ..
                }) => schema
                    .validate(value, sub_path)?
                    .merge_object(&mut flat_config)
                    .with_context(sub_ctx)?,
                Some(Property {
                    kind: Array(schema),
                    ..
                }) => {
                    values.insert(key, Value::Array(schema.validate_array(value, sub_path)?));
                }
                Some(Property {
                    kind: Multiset(schema),
                    ..
                }) if schema.is_scalar() => {
                    values.insert(key, Value::Set(schema.validate_array(value, sub_path)?));
                }
                Some(Property {
                    kind: Multiset(schema),
                    ..
                }) => {
                    let TomlValue::Array(array) = value else {
                        return Err(anyhow::anyhow!("expected array for multiset"))
                            .with_context(sub_ctx);
                    };
                    let objs = schema.validate_object_array(array, sub_path, &mut flat_config)?;
                    merge_flat_objects(&mut flat_config, sub_path, objs)?;
                }
                None if path.is_empty() => match (self.find_object_schema(&key), value) {
                    (Some((schema, true)), TomlValue::Array(array)) => {
                        let objs =
                            schema.validate_object_array(array, sub_path, &mut flat_config)?;
                        merge_flat_objects(&mut flat_config, sub_path, objs)?;
                    }
                    (Some((schema, false)), TomlValue::Table(table)) => schema
                        .validate_object(table, sub_path)?
                        .merge_object(&mut flat_config)
                        .with_context(sub_ctx)?,
                    (Some((_, multi)), _) => {
                        let expect = if multi { "array" } else { "object" };
                        return Err(anyhow::anyhow!("expected {expect}")).with_context(sub_ctx);
                    }
                    (None, value) => {
                        if let Some(value) =
                            merge_schemaless_value(value, &mut flat_config, sub_path)
                                .with_context(sub_ctx)?
                        {
                            values.insert(key, value);
                        }
                    }
                },
                None => {
                    if let Some(value) = merge_schemaless_value(value, &mut flat_config, sub_path)
                        .with_context(sub_ctx)?
                    {
                        values.insert(key, value);
                    }
                }
            }
        }
        let typ = typ.into();
        Ok(ValidateResult::new(
            Value::Nested { typ, values },
            flat_config,
        ))
    }
}

pub struct ValidateResult {
    result: Value,
    flat_config: Option<IndexMap<String, Value>>,
}

impl From<Value> for ValidateResult {
    fn from(value: Value) -> Self {
        ValidateResult {
            result: value,
            flat_config: None,
        }
    }
}

impl ValidateResult {
    fn new(result: Value, flat_config: IndexMap<String, Value>) -> Self {
        ValidateResult {
            result,
            flat_config: Some(flat_config),
        }
    }

    pub fn take_result(self) -> anyhow::Result<Value> {
        if self.flat_config.is_some() {
            anyhow::bail!("not a value-only result");
        }
        Ok(self.result)
    }

    pub fn merge_into(self, flat_config: &mut IndexMap<String, Value>) -> anyhow::Result<Value> {
        if let Some(new_flat_config) = self.flat_config {
            merge_flat_config(flat_config, new_flat_config)?;
        }
        Ok(self.result)
    }

    pub fn merge_object(self, flat_config: &mut IndexMap<String, Value>) -> anyhow::Result<()> {
        let value = self.merge_into(flat_config)?;
        if let Value::Nested { typ, .. } = &value {
            merge_flat_config(flat_config, [(typ.clone(), value)])
        } else {
            anyhow::bail!("expected object");
        }
    }
}

fn merge_flat_config(
    flat_config: &mut IndexMap<String, Value>,
    new_flat_config: impl IntoIterator<Item = (String, Value)>,
) -> anyhow::Result<()> {
    for (key, value) in new_flat_config {
        match flat_config.entry(key) {
            Entry::Occupied(mut entry) => match (entry.get_mut(), value) {
                (
                    Value::Nested {
                        typ: existing_typ,
                        values: existing_map,
                    },
                    Value::Nested {
                        typ: new_typ,
                        values: new_map,
                    },
                ) => {
                    if new_typ.ne(existing_typ) {
                        anyhow::bail!(
                            "cannot merge nested values of different types: {} and {}",
                            existing_typ,
                            new_typ
                        );
                    }
                    for (new_key, new_value) in new_map {
                        match existing_map.entry(new_key) {
                            Entry::Occupied(entry) => {
                                anyhow::bail!("duplicate key: {}", entry.key());
                            }
                            Entry::Vacant(entry) => {
                                entry.insert(new_value);
                            }
                        }
                    }
                }
                (Value::Set(existing_set), Value::Set(new_set)) => {
                    existing_set.extend(new_set);
                }
                (existing, new) => {
                    panic!("expected {existing:?} but got {new:?}");
                }
            },
            Entry::Vacant(entry) => {
                entry.insert(value);
            }
        }
    }
    Ok(())
}

fn merge_flat_objects<I>(
    flat_config: &mut IndexMap<String, Value>,
    path: &[&str],
    objects: I,
) -> anyhow::Result<()>
where
    I: IntoIterator<Item = Value>,
{
    let mut values = IndexMap::new();
    for (i, value) in objects.into_iter().enumerate() {
        let i = i.to_string();
        let sub_path = &[path, &[&i]].concat();
        if let Value::Nested { typ, .. } = &value {
            values
                .entry(typ.to_string())
                .or_insert_with(|| Vec::new())
                .push(value);
        } else {
            panic!("{}: expected object", sub_path.join("."));
        };
    }
    merge_flat_config(
        flat_config,
        values.into_iter().map(|(k, v)| (k, Value::Set(v))),
    )
    .with_context(|| path.join("."))
}

fn merge_schemaless_value(
    value: TomlValue,
    flat_config: &mut IndexMap<String, Value>,
    path: &[&str],
) -> anyhow::Result<Option<Value>> {
    let value = Value::try_from(value)?;
    match value {
        Value::Nested { ref typ, .. } => {
            merge_flat_config(flat_config, [(typ.clone(), Value::try_from(value)?)])
                .with_context(|| path.join("."))?;
            Ok(None)
        }
        Value::Set(items) if items.iter().any(Value::is_object) => {
            merge_flat_objects(flat_config, path, items)?;
            Ok(None)
        }
        _ => Ok(Some(value)),
    }
}

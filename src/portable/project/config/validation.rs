use std::borrow::Cow;

use edgeql_parser::helpers::quote_string as ql;
use indexmap::IndexMap;

use toml::Value as TomlValue;

use crate::hint::HintExt;

use super::Value;
use super::schema::{ObjectType, Schema, Typ};

pub fn validate(value: TomlValue, schema: &Schema) -> anyhow::Result<Commands> {
    let mut validator = Validator {
        commands: Commands::default(),
        schema,
        path: Vec::new(),
    };

    validator.validate_top_level(value)?;

    Ok(validator.commands)
}

#[derive(Debug, Default)]
pub struct Commands {
    pub set: Vec<(String, String, Value)>,
    pub insert: IndexMap<String, Vec<IndexMap<String, Value>>>,
}

impl Commands {
    pub fn set(&mut self, config_object: &str, values: IndexMap<String, Value>) {
        for (property, val) in values {
            self.set.push((config_object.to_string(), property, val));
        }
    }
    pub fn insert(&mut self, config_object: String, values: IndexMap<String, Value>) {
        let inserts = self.insert.entry(config_object).or_default();
        inserts.push(values);
    }
    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.insert.is_empty()
    }
}

struct Validator<'s> {
    commands: Commands,
    schema: &'s Schema,
    path: Vec<String>,
}

impl Validator<'_> {
    /// Entry point
    fn validate_top_level(&mut self, value: TomlValue) -> anyhow::Result<()> {
        let TomlValue::Table(entries) = value else {
            return Err(anyhow::anyhow!("expected a table for [local.config]"));
        };

        // validate entries like `cfg::Config` and `ext::auth::AuthConfig```
        let mut not_found = toml::map::Map::new();
        for (cfg_object, value) in entries {
            let Some(object_type) = self.schema.find_object(&cfg_object) else {
                not_found.insert(cfg_object, value);
                continue;
            };

            let toml_values = if object_type.is_multi {
                let TomlValue::Array(values) = value else {
                    return Err(self.err_expected("an array", &value));
                };
                values
            } else {
                vec![value]
            };

            self.path.push(cfg_object.clone());
            for v in toml_values {
                let values = self.validate_object_type(v, object_type)?;
                if object_type.is_top_level {
                    self.commands.set(&cfg_object, values);
                } else {
                    self.commands.insert(cfg_object.clone(), values);
                }
            }

            self.path.pop();
        }

        // validate entries like `allow_bare_ddl`, which we implicitly assume are on cfg::Config
        let cfg_config = self.schema.find_object("cfg::Config").unwrap();
        let values = self.validate_object_type(TomlValue::Table(not_found), cfg_config)?;
        self.commands.set("cfg::Config", values);

        Ok(())
    }

    fn validate_object_type(
        &mut self,
        value: TomlValue,
        obj: &ObjectType,
    ) -> anyhow::Result<IndexMap<String, Value>> {
        let TomlValue::Table(entries) = value else {
            return Err(self.err_expected("a table", &value));
        };

        let mut properties = IndexMap::new();

        for (key, value) in entries {
            log::debug!("{key}");
            self.path.push(key.clone());

            let Some(ptr) = obj.pointers.get(&key) else {
                return Err(anyhow::anyhow!(
                    "unknown configuration option: {}",
                    self.path.join(".")
                ));
            };

            if ptr.target.is_scalar() {
                // properties

                if !ptr.is_multi {
                    let value = self.validate_property(value, &ptr.target)?;
                    properties.insert(key, value);
                } else {
                    let TomlValue::Array(array) = value else {
                        return Err(self.err_expected("an array", &value));
                    };
                    let values = array
                        .into_iter()
                        .map(|v| self.validate_property(v, &ptr.target))
                        .collect::<Result<Vec<_>, _>>()?;
                    properties.insert(key, Value::Set(values));
                }
            } else {
                // links

                if !ptr.is_multi {
                    let value = self.validate_link(value, &ptr.target)?;
                    if let Some(value) = value {
                        properties.insert(key, value);
                    }
                } else {
                    let TomlValue::Array(array) = value else {
                        return Err(self.err_expected("an array", &value));
                    };
                    let mut set = Vec::new();
                    for v in array {
                        if let Some(v) = self.validate_link(v, &ptr.target)? {
                            set.push(v);
                        }
                    }
                    if !set.is_empty() {
                        properties.insert(key, Value::Set(set));
                    }
                }
            }

            self.path.pop();
        }

        Ok(properties)
    }

    fn validate_property(&mut self, value: TomlValue, typ: &Typ) -> anyhow::Result<Value> {
        use TomlValue as Toml;
        use Typ::*;

        let as_injected = value
            .as_str()
            .and_then(|v| v.strip_prefix("{{"))
            .and_then(|v| v.strip_suffix("}}"));
        if let Some(injected) = as_injected {
            return Ok(Value::Injected(injected.to_string()));
        }

        Ok(match (typ, value) {
            (Primitive(name) | Enum { name, .. }, Toml::Array(v)) => {
                return Err(self.err_expected(name, &Toml::Array(v)));
            }
            (Primitive(name) | Enum { name, .. }, Toml::Table(v)) => {
                return Err(self.err_expected(name, &Toml::Table(v)));
            }
            (Primitive(prim), value) => match (prim.as_str(), value) {
                ("str", Toml::String(value)) => Value::Injected(ql(&value)),
                ("int64", Toml::Integer(value)) => Value::Injected(value.to_string()),
                ("int32", Toml::Integer(value)) => Value::Injected(value.to_string()),
                ("int16", Toml::Integer(value)) => Value::Injected(value.to_string()),
                ("float64", Toml::Float(value)) => Value::Injected(value.to_string()),
                ("float32", Toml::Float(value)) => Value::Injected(value.to_string()),
                ("bool", Toml::Boolean(value)) => Value::Injected(value.to_string()),
                ("duration", Toml::String(value)) => {
                    Value::Injected(format!("<duration>{}", ql(&value)))
                }
                (_, value) => {
                    return Err(self.err_expected(prim, &value));
                }
            },
            (Enum { name, choices }, Toml::String(value)) => {
                if !choices.contains(&value) {
                    return Err(
                        self.err_expected(format!("one of {choices:?}"), &Toml::String(value))
                    );
                }
                Value::Injected(format!("<{}>{}", name, ql(&value)))
            }
            (typ, value) => {
                return Err(self.err_expected(typ, &value));
            }
        })
    }

    fn validate_link(&mut self, value: TomlValue, typ: &Typ) -> anyhow::Result<Option<Value>> {
        use TomlValue as Toml;
        use Typ::*;

        match (typ, value) {
            (ObjectRef(target_ref), Toml::Table(value)) => {
                let Some(target) = self.schema.find_object(target_ref) else {
                    return Err(anyhow::anyhow!(
                        "{}: unknown config object: {target_ref}",
                        self.path.join(".")
                    ));
                };

                let values = self.validate_object_type(Toml::Table(value), target)?;
                Ok(if target.is_non_locatable {
                    Some(Value::Insert {
                        typ: target_ref.clone(),
                        values,
                    })
                } else {
                    self.commands.insert(target_ref.clone(), values);
                    None
                })
            }
            (Union(obj_type_refs), Toml::Table(mut value)) => {
                let Some(Toml::String(t_name)) = value.remove("_tname") else {
                    return Err(
                        anyhow::anyhow!("{} is missing _tname field", self.path.join("."))
                            .with_hint(|| format!(
                                "{} can be any of the following types: {}. Use _tname to differenciate between them.",
                                self.path.last().unwrap(),
                                obj_type_refs.join(", ")
                            ))
                            .into(),
                    );
                };
                let Some(obj_type_ref) = obj_type_refs.iter().find(|r| r == &&t_name) else {
                    return Err(anyhow::anyhow!(
                        "{}: unknown type {t_name}",
                        self.path.join(".")
                    ));
                };

                let Some(target) = self.schema.find_object(obj_type_ref) else {
                    return Err(anyhow::anyhow!(
                        "{}: unknown config object: {obj_type_ref}",
                        self.path.join(".")
                    ));
                };
                let values = self.validate_object_type(Toml::Table(value), target)?;
                Ok(if target.is_non_locatable {
                    Some(Value::Insert {
                        typ: obj_type_ref.clone(),
                        values,
                    })
                } else {
                    self.commands.insert(obj_type_ref.clone(), values);
                    None
                })
            }
            (typ, value) => {
                Err(self.err_expected(typ, &value))
            }
        }
    }

    fn err_expected(&self, expected: impl std::fmt::Display, got: &TomlValue) -> anyhow::Error {
        let got = match got {
            TomlValue::String(s) => Cow::Owned(format!("\"{s}\"")),
            TomlValue::Integer(_) => "an integer".into(),
            TomlValue::Float(_) => "a float".into(),
            TomlValue::Boolean(_) => "a boolean".into(),
            TomlValue::Datetime(_) => "a datetime".into(),
            TomlValue::Array(_) => "an array".into(),
            TomlValue::Table(_) => "a table".into(),
        };
        anyhow::anyhow!("{} expected {expected}, got {got}", self.path.join("."))
    }
}

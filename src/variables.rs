use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;

use crate::prompt;
use crate::prompt::variable::{self, VariableInput};
use crate::repl;
use gel_protocol::codec;
use gel_protocol::descriptors::{Descriptor, Typedesc};
use gel_protocol::value::Value;

#[derive(Debug)]
pub struct Canceled;

pub async fn input_variables(
    desc: &Typedesc,
    prompt: &mut repl::PromptRpc,
    input_language: repl::InputLanguage,
) -> Result<Value, anyhow::Error> {
    // only for protocol < 0.12
    if desc.is_empty_tuple() {
        return Ok(Value::Tuple(Vec::new()));
    }
    match desc.root() {
        Some(Descriptor::ObjectShape(obj)) => {
            let mut fields = Vec::with_capacity(obj.elements.len());
            let shape = obj.elements[..].into();
            for el in obj.elements.iter() {
                let optional = el.cardinality.map(|c| c.is_optional()).unwrap_or(false);
                let name = match input_language {
                    // SQL params are 1-based, so adjust the base
                    repl::InputLanguage::Sql => (el
                        .name
                        .parse::<i32>()
                        .expect("SQL argument names to be numeric")
                        + 1)
                    .to_string(),
                    _ => el.name.to_owned(),
                };
                fields
                    .push(input_item(&name, desc.get(el.type_pos)?, desc, prompt, optional).await?);
            }
            Ok(Value::Object { shape, fields })
        }
        Some(root) => Err(anyhow::anyhow!("Unknown input type descriptor: {:?}", root)),
        // Since protocol 0.12
        None => Ok(Value::Nothing),
    }
}

fn get_descriptor_type<'a>(
    desc: &'a Descriptor,
    all: &'a Typedesc,
) -> Result<Arc<dyn VariableInput>, anyhow::Error> {
    let base = desc.normalize_to_base(&all.as_query_arg_context())?;

    match base {
        Descriptor::BaseScalar(s) => {
            let var_type: Arc<dyn VariableInput> = match *s.id {
                codec::STD_STR => Arc::new(variable::Str),
                codec::STD_UUID => Arc::new(variable::Uuid),
                codec::STD_INT16 => Arc::new(variable::Int16),
                codec::STD_INT32 => Arc::new(variable::Int32),
                codec::STD_INT64 => Arc::new(variable::Int64),
                codec::STD_FLOAT32 => Arc::new(variable::Float32),
                codec::STD_FLOAT64 => Arc::new(variable::Float64),
                codec::STD_DECIMAL => Arc::new(variable::Decimal),
                codec::STD_BOOL => Arc::new(variable::Bool),
                codec::STD_JSON => Arc::new(variable::Json),
                codec::STD_BIGINT => Arc::new(variable::BigInt),
                _ => return Err(anyhow::anyhow!("Unimplemented input type {}", *s.id)),
            };

            Ok(var_type)
        }
        Descriptor::Array(arr) => {
            let element_type = get_descriptor_type(all.get(arr.type_pos)?, all)?;
            Ok(Arc::new(variable::Array { element_type }))
        }
        Descriptor::Tuple(tuple) => {
            let elements: Result<Vec<Arc<dyn VariableInput>>, _> = tuple
                .element_types
                .iter()
                .map(|v| get_descriptor_type(all.get(*v)?, all))
                .collect();

            match elements {
                Ok(element_types) => Ok(Arc::new(variable::Tuple { element_types })),
                Err(e) => Err(e),
            }
        }
        Descriptor::NamedTuple(named_tuple) => {
            let mut elements = HashMap::new();

            for element in &named_tuple.elements {
                elements.insert(
                    element.name.clone(),
                    get_descriptor_type(all.get(element.type_pos)?, all)?,
                );
            }

            Ok(Arc::new(variable::NamedTuple {
                element_types: elements,
                shape: named_tuple.elements[..].into(),
            }))
        }
        _ => Err(anyhow::anyhow!(
            "Unimplemented input type descriptor: {:?}",
            desc
        )),
    }
}

async fn input_item(
    name: &str,
    item: &Descriptor,
    all: &Typedesc,
    state: &mut repl::PromptRpc,
    optional: bool,
) -> Result<Option<Value>, anyhow::Error> {
    let var_type = get_descriptor_type(item, all)?;

    let val = match state.variable_input(name, var_type, optional, "").await? {
        prompt::VarInput::Value(val) => Some(val),
        prompt::VarInput::Interrupt => Err(Canceled)?,
        prompt::VarInput::Eof => None,
    };

    Ok(val)
}

impl Error for Canceled {}

impl fmt::Display for Canceled {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        "Operation canceled".fmt(f)
    }
}

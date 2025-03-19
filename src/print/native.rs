use std::cmp::min;

use bigdecimal::BigDecimal;
use num_bigint::BigInt;

use crate::print::buffer::Result;
use crate::print::formatter::Formatter;
use crate::repl::VectorLimit;
use gel_protocol::value::Value;
use geozero::{ToWkt, wkb::Ewkb};

pub trait FormatExt {
    fn format<F: Formatter>(&self, prn: &mut F) -> Result<F::Error>;
}

pub fn format_string(s: &str, expanded: bool) -> String {
    let mut buf = String::with_capacity(s.len() + 2);
    buf.push('\'');
    for c in s.chars() {
        match c {
            '\x00'..='\x08' | '\x0B' | '\x0C' | '\x0E'..='\x1F' | '\x7F' => {
                buf.push_str(&format!("\\x{:02x}", c as u32))
            }
            '\u{0080}'..='\u{009F}' => buf.push_str(&format!("\\u{:04x}", c as u32)),
            '\'' => buf.push_str("\\'"),
            '\\' => buf.push_str("\\\\"),
            '\r' if !expanded => buf.push_str("\\r"),
            '\n' if !expanded => buf.push_str("\\n"),
            '\t' if !expanded => buf.push_str("\\t"),
            _ => buf.push(c),
        }
    }
    buf.push('\'');
    buf
}

fn format_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut buf = String::with_capacity(bytes.len() + 3);
    buf.push('b');
    buf.push('\'');
    for b in bytes {
        match b {
            0..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F | 0x7F..=0xFF => {
                write!(&mut buf, "\\x{b:02x}").unwrap()
            }
            b'\'' => buf.push_str("\\'"),
            b'\r' => buf.push_str("\\r"),
            b'\n' => buf.push_str("\\n"),
            b'\t' => buf.push_str("\\t"),
            b'\\' => buf.push_str("\\\\"),
            _ => buf.push(*b as char),
        }
    }
    buf.push('\'');
    buf
}

fn format_bigint(bint: BigInt) -> String {
    let txt = bint.to_string();
    let no_zeros = txt.trim_end_matches('0');
    let zeros = txt.len() - no_zeros.len();
    if zeros > 5 {
        format!("{no_zeros}e{zeros}n")
    } else {
        format!("{txt}n")
    }
}

fn format_decimal(value: BigDecimal) -> String {
    let txt = value.to_string();
    if txt.contains('.') {
        if txt.starts_with("0.00000") {
            let no_zeros = txt[2..].trim_start_matches('0');
            let zeros = txt.len() - 2 - no_zeros.len();
            format!("0.{no_zeros}e-{zeros}")
        } else {
            format!("{txt}n")
        }
    } else {
        let no_zeros = txt.trim_end_matches('0');
        let zeros = txt.len() - no_zeros.len();
        if zeros > 5 {
            format!("{no_zeros}.0e{zeros}n")
        } else {
            format!("{txt}.0n")
        }
    }
}

impl FormatExt for Value {
    fn format<F: Formatter>(&self, prn: &mut F) -> Result<F::Error> {
        use Value as V;
        match self {
            V::Nothing => prn.const_uuid("Nothing"),
            V::Uuid(u) => prn.const_uuid(u),
            V::Str(s) => prn.const_string(format_string(s, prn.expand_strings())),
            V::Bytes(b) => prn.const_string(format_bytes(b)),
            V::Int16(v) => prn.const_number(v),
            V::Int32(v) => prn.const_number(v),
            V::Int64(v) => prn.const_number(v),
            V::Float32(v) => prn.const_number(v),
            V::Float64(v) => prn.const_number(v),
            V::BigInt(v) => prn.const_number(format_bigint(v.into())),
            V::Decimal(v) => prn.const_number(format_decimal(v.into())),
            V::Bool(v) => prn.const_bool(v),
            V::ConfigMemory(t) => prn.typed("cfg::memory", t.to_string()),
            V::Datetime(t) => prn.typed("datetime", format!("{t:?}")),
            V::LocalDatetime(t) => prn.typed("cal::local_datetime", format!("{t:?}")),
            V::LocalDate(d) => prn.typed("cal::local_date", format!("{d:?}")),
            V::LocalTime(t) => prn.typed("cal::local_time", format!("{t:?}")),
            V::Duration(d) => prn.typed("duration", d.to_string()),
            V::RelativeDuration(d) => prn.typed("cal::relative_duration", d.to_string()),
            V::DateDuration(d) => prn.typed("cal::date_duration", d.to_string()),
            V::Json(d) => prn.const_string(format!("{d:?}")),
            V::Set(items) => prn.set(|prn| {
                if let Some(limit) = prn.max_items() {
                    for item in &items[..min(limit, items.len())] {
                        item.format(prn)?;
                        prn.comma()?;
                    }
                    if items.len() > limit {
                        prn.ellipsis()?;
                    }
                } else {
                    for item in items {
                        item.format(prn)?;
                        prn.comma()?;
                    }
                }
                Ok(())
            }),
            V::Object { shape, fields } => {
                // TODO(tailhook) optimize it on no-implicit-types
                //                or just cache typeid index on shape
                let type_name = shape
                    .elements
                    .iter()
                    .zip(fields)
                    .find(|(f, _)| f.name == "__tname__")
                    .and_then(|(_, v)| {
                        if let Some(Value::Str(type_name)) = v {
                            Some(type_name.as_str())
                        } else {
                            None
                        }
                    });
                prn.object(type_name, |prn| {
                    let mut n = 0;
                    for (fld, value) in shape.elements.iter().zip(fields) {
                        if !fld.flag_implicit || prn.implicit_properties() {
                            if fld.flag_link_property {
                                prn.object_field(&("@".to_owned() + &fld.name), true)?;
                            } else {
                                prn.object_field(&fld.name, false)?;
                            };
                            value.format(prn)?;
                            prn.comma()?;
                            n += 1;
                        }
                    }
                    if n == 0 {
                        if let Some((fld, value)) = shape
                            .elements
                            .iter()
                            .zip(fields)
                            .find(|(f, _)| f.name == "id")
                        {
                            prn.object_field(&fld.name, false)?;
                            value.format(prn)?;
                            prn.comma()?;
                        }
                    }
                    Ok(())
                })
            }
            V::SparseObject(s) => prn.object(Some("SparseObject"), |prn| {
                for (fld, value) in s.pairs() {
                    prn.object_field(fld, false)?;
                    if let Some(value) = value {
                        value.format(prn)?;
                    } else {
                        prn.nil()?;
                    }
                    prn.comma()?;
                }
                Ok(())
            }),
            V::Tuple(items) => prn.tuple(|prn| {
                for item in items {
                    item.format(prn)?;
                    prn.comma()?;
                }
                Ok(())
            }),
            V::NamedTuple { shape, fields } => prn.named_tuple(|prn| {
                for (fld, value) in shape.elements.iter().zip(fields) {
                    prn.tuple_field(&fld.name)?;
                    value.format(prn)?;
                    prn.comma()?;
                }
                Ok(())
            }),
            V::SQLRow { shape, fields } => prn.named_tuple(|prn| {
                for (fld, value) in shape.elements.iter().zip(fields) {
                    prn.tuple_field(&fld.name)?;
                    value.format(prn)?;
                    prn.comma()?;
                }
                Ok(())
            }),
            V::Array(items) => prn.array(None, |prn| {
                if let Some(limit) = prn.max_items() {
                    for item in &items[..min(limit, items.len())] {
                        item.format(prn)?;
                        prn.comma()?;
                    }
                    if items.len() > limit {
                        prn.ellipsis()?;
                    }
                } else {
                    for item in items {
                        item.format(prn)?;
                        prn.comma()?;
                    }
                }
                Ok(())
            }),
            V::Vector(items) => match prn.max_vector_length() {
                VectorLimit::Fixed(limit) => prn.array(Some("ext::pgvector::vector"), |prn| {
                    for item in &items[..min(limit, items.len())] {
                        prn.const_number(item)?;
                        prn.comma()?;
                    }
                    if items.len() > limit {
                        prn.ellipsis()?;
                    }
                    Ok(())
                }),
                VectorLimit::Unlimited => prn.array(Some("ext::pgvector::vector"), |prn| {
                    for item in items {
                        prn.const_number(item)?;
                        prn.comma()?;
                    }
                    Ok(())
                }),
                VectorLimit::Auto => prn.auto_sized_vector(items),
            },
            V::Enum(v) => prn.const_enum(&**v),
            V::Range(rng) => {
                prn.call("range", |prn| {
                    rng.lower().map(|x| &**x).format(prn)?;
                    prn.comma()?;

                    if !rng.is_empty() {
                        rng.upper().map(|x| &**x).format(prn)?;
                        prn.comma()?;

                        // These fields are all optional, so we omit them
                        // when they have the default values.
                        if !rng.inc_lower() {
                            prn.tuple_field("inc_lower")?;
                            prn.const_bool(rng.inc_lower())?;
                            prn.comma()?;
                        }

                        if rng.inc_upper() {
                            prn.tuple_field("inc_upper")?;
                            prn.const_bool(rng.inc_upper())?;
                            prn.comma()?;
                        }
                    } else {
                        prn.tuple_field("empty")?;
                        prn.const_bool(rng.is_empty())?;
                        prn.comma()?;
                    }

                    Ok(())
                })
            }
            V::PostGisGeometry(v) => {
                let wkb = Ewkb(v);
                prn.const_string(wkb.to_ewkt(None).unwrap())
            }
            V::PostGisGeography(v) => {
                let wkb = Ewkb(v);
                prn.const_string(wkb.to_ewkt(None).unwrap())
            }
            V::PostGisBox2d(v) => {
                let wkb = Ewkb(v);
                prn.const_string(wkb.to_ewkt(None).unwrap())
            }
            V::PostGisBox3d(v) => {
                let wkb = Ewkb(v);
                prn.const_string(wkb.to_ewkt(None).unwrap())
            }
        }
    }
}

impl FormatExt for Option<Value> {
    fn format<F: Formatter>(&self, prn: &mut F) -> Result<F::Error> {
        match self {
            Some(v) => v.format(prn),
            None => prn.nil(),
        }
    }
}

impl FormatExt for Option<&'_ Value> {
    fn format<F: Formatter>(&self, prn: &mut F) -> Result<F::Error> {
        match self {
            Some(v) => v.format(prn),
            None => prn.nil(),
        }
    }
}

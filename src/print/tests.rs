use std::convert::Infallible;
use std::convert::TryFrom;
use std::pin::Pin;
use std::str::FromStr;
use std::task;

use bigdecimal::BigDecimal;
use bytes::Bytes;
use tokio_stream::Stream;

use crate::print::native::FormatExt;
use crate::print::style::Styler;
use crate::print::{self, _native_format, Config};
use crate::repl::VectorLimit;
use gel_protocol::codec::{ObjectShape, ShapeElement};
use gel_protocol::model::Datetime;
use gel_protocol::value::Value;

struct UnfusedStream<'a, I>(Option<&'a [I]>);

impl<'a, I> UnfusedStream<'a, I> {
    fn new(els: &'a [I]) -> UnfusedStream<'a, I> {
        UnfusedStream(Some(els))
    }
}

impl<I: Clone> Stream for UnfusedStream<'_, I> {
    type Item = Result<I, Infallible>;
    fn poll_next(
        mut self: Pin<&mut Self>,
        _cx: &mut task::Context<'_>,
    ) -> task::Poll<Option<Self::Item>> {
        let val = self.0.as_mut().expect("no poll after EOS");
        if val.is_empty() {
            self.0.take().unwrap();
            return task::Poll::Ready(None);
        }
        let item = val[0].clone();
        *val = &val[1..];
        task::Poll::Ready(Some(Ok(item)))
    }
}

fn test_format_cfg<I: FormatExt + Clone + Send + Sync>(
    items: &[I],
    config: &Config,
) -> Result<String, Infallible> {
    let mut out = String::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime
        .block_on(_native_format(
            UnfusedStream::new(items),
            config,
            config.max_width.unwrap_or(80),
            false,
            &mut out,
        ))
        .unwrap();
    Ok(out)
}

fn test_format<I: FormatExt + Clone + Send + Sync>(items: &[I]) -> Result<String, Infallible> {
    test_format_cfg(
        items,
        &Config {
            colors: Some(false),
            indent: 2,
            expand_strings: false,
            max_width: Some(80),
            implicit_properties: false,
            max_items: None,
            max_vector_length: VectorLimit::Unlimited,
            styler: Styler::new(),
        },
    )
}

fn json_fmt(j: &str) -> String {
    print::json_to_string(
        serde_json::from_str::<serde_json::Value>(j)
            .unwrap()
            .as_array()
            .unwrap(),
        &Config::new(),
    )
    .unwrap()
}

fn json_fmt_width(w: usize, j: &str) -> String {
    print::json_to_string(
        serde_json::from_str::<serde_json::Value>(j)
            .unwrap()
            .as_array()
            .unwrap(),
        Config::new().max_width(w),
    )
    .unwrap()
}

#[test]
fn int() {
    assert_eq!(test_format(&[Value::Int64(10)]).unwrap(), "{10}");
    assert_eq!(
        test_format(&[Value::Int64(10), Value::Int64(20),]).unwrap(),
        "{10, 20}"
    );
}

#[test]
fn bigdecimal() {
    assert_eq!(
        test_format(&[Value::Decimal(
            TryFrom::try_from(BigDecimal::from_str("10.1").unwrap()).unwrap()
        ),])
        .unwrap(),
        "{10.1n}"
    );
}

#[test]
fn bigint() {
    assert_eq!(
        test_format(&[
            Value::BigInt(10.into()),
            Value::BigInt(10000.into()),
            Value::BigInt(100000000000i64.into()),
        ])
        .unwrap(),
        "{10n, 10000n, 1e11n}"
    );
}

#[test]
fn datetime() {
    assert_eq!(
        test_format(&[
            Value::Datetime(Datetime::from_unix_micros(-1000000000000000)),
            Value::Datetime(Datetime::from_unix_micros(1604506938347258)),
        ])
        .unwrap(),
        "{<datetime>\'1938-04-24T22:13:20Z\', \
      <datetime>\'2020-11-04T16:22:18.347258Z\'}"
    );
}

#[test]
fn decimal() {
    assert_eq!(
        test_format(&[
            Value::Decimal(TryFrom::try_from(BigDecimal::from_str("10e3").unwrap()).unwrap()),
            Value::Decimal(TryFrom::try_from(BigDecimal::from_str("10e10").unwrap()).unwrap()),
            Value::Decimal(
                TryFrom::try_from(BigDecimal::from_str("100000000000.1").unwrap()).unwrap()
            ),
            Value::Decimal(
                TryFrom::try_from(BigDecimal::from_str("0.000000000000508").unwrap()).unwrap()
            ),
        ])
        .unwrap(),
        "{10000.0n, 1.0e11n, 100000000000.1n, 5.08E-13n}"
    );
}

#[test]
fn array_ellipsis() {
    assert_eq!(
        test_format(&[Value::Array(vec![
            Value::Int64(10),
            Value::Int64(20),
            Value::Int64(30),
        ]),])
        .unwrap(),
        "{[10, 20, 30]}"
    );
    assert_eq!(
        test_format_cfg(
            &[Value::Array(vec![
                Value::Int64(10),
                Value::Int64(20),
                Value::Int64(30),
            ]),],
            Config::new().max_items(Some(2))
        )
        .unwrap(),
        "{[10, 20, ...]}"
    );
    assert_eq!(
        test_format_cfg(
            &[Value::Array(vec![
                Value::Int64(10),
                Value::Int64(20),
                Value::Int64(30),
            ]),],
            Config::new().max_items(Some(2)).max_width(10)
        )
        .unwrap(),
        r###"{
  [
    10,
    20,
    ... (further results hidden `\set limit 2`)
  ],
}"###
    );
    assert_eq!(
        test_format_cfg(
            &[Value::Array(vec![Value::Int64(10),]),],
            Config::new().max_items(Some(2))
        )
        .unwrap(),
        "{[10]}"
    );
}

#[test]
fn set_ellipsis() {
    assert_eq!(
        test_format(&[Value::Set(vec![
            Value::Int64(10),
            Value::Int64(20),
            Value::Int64(30),
        ]),])
        .unwrap(),
        "{{10, 20, 30}}"
    );
    assert_eq!(
        test_format_cfg(
            &[Value::Set(vec![
                Value::Int64(10),
                Value::Int64(20),
                Value::Int64(30),
            ]),],
            Config::new().max_items(Some(2))
        )
        .unwrap(),
        "{{10, 20, ...}}"
    );
    assert_eq!(
        test_format_cfg(
            &[Value::Set(vec![Value::Int64(10),]),],
            Config::new().max_items(Some(2))
        )
        .unwrap(),
        "{{10}}"
    );
}

#[test]
fn vector() {
    use crate::repl::VectorLimit::*;
    assert_eq!(
        test_format(&[Value::Vector((0..10).map(|v| v as _).collect()),]).unwrap(),
        "{<ext::pgvector::vector>[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]}"
    );
    assert_eq!(
        test_format_cfg(
            &[Value::Vector((0..10).map(|v| v as _).collect()),],
            Config::new().max_vector_length(Fixed(2))
        )
        .unwrap(),
        "{<ext::pgvector::vector>[0, 1, ...]}",
    );
    assert_eq!(
        test_format_cfg(
            &[Value::Vector((0..10).map(|v| v as _).collect()),],
            Config::new().max_vector_length(Unlimited)
        )
        .unwrap(),
        "{<ext::pgvector::vector>[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]}",
    );
    assert_eq!(
        test_format_cfg(
            &[Value::Vector((0..10).map(|v| v as _).collect()),],
            Config::new().max_width(20).max_vector_length(Auto)
        )
        .unwrap(),
        "{\n  \
           <ext::pgvector::vector>[\n    0,\n    1,\n    2,\n    ...\n  ],\n}",
    );
    assert_eq!(
        test_format_cfg(
            &[Value::Vector((0..10).map(|v| v as _).collect()),],
            Config::new().max_width(50).max_vector_length(Auto)
        )
        .unwrap(),
        "{\n  <ext::pgvector::vector>[0, 1, 2, 3, 4, 5, ...],\n}",
    );
}

#[test]
fn wrap() {
    assert_eq!(
        test_format_cfg(
            &[Value::Int64(10), Value::Int64(20),],
            Config::new().max_width(10)
        )
        .unwrap(),
        "{10, 20}"
    );
    assert_eq!(
        test_format_cfg(
            &[Value::Int64(10), Value::Int64(20), Value::Int64(30),],
            Config::new().max_width(10)
        )
        .unwrap(),
        "{\n  10,\n  20,\n  30,\n}"
    );
}

#[test]
fn object() {
    let shape = ObjectShape::new(vec![
        ShapeElement {
            flag_implicit: false,
            flag_link_property: false,
            flag_link: false,
            cardinality: None,
            name: "field1".into(),
        },
        ShapeElement {
            flag_implicit: false,
            flag_link_property: false,
            flag_link: false,
            cardinality: None,
            name: "field2".into(),
        },
    ]);
    assert_eq!(
        test_format_cfg(
            &[
                Value::Object {
                    shape: shape.clone(),
                    fields: vec![Some(Value::Int32(10)), Some(Value::Int32(20)),]
                },
                Value::Object {
                    shape: shape.clone(),
                    fields: vec![Some(Value::Int32(30)), Some(Value::Int32(40)),]
                },
            ],
            Config::new().max_width(60)
        )
        .unwrap(),
        r###"{
  Object {field1: 10, field2: 20},
  Object {field1: 30, field2: 40},
}"###
    );
    assert_eq!(
        test_format_cfg(
            &[
                Value::Object {
                    shape: shape.clone(),
                    fields: vec![Some(Value::Int32(10)), Some(Value::Int32(20)),]
                },
                Value::Object {
                    shape: shape.clone(),
                    fields: vec![Some(Value::Int32(30)), None,]
                },
            ],
            Config::new().max_width(20)
        )
        .unwrap(),
        r###"{
  Object {
    field1: 10,
    field2: 20,
  },
  Object {
    field1: 30,
    field2: {},
  },
}"###
    );
}

#[test]
fn link_property() {
    let shape = ObjectShape::new(vec![
        ShapeElement {
            flag_implicit: false,
            flag_link_property: false,
            flag_link: false,
            cardinality: None,
            name: "field1".into(),
        },
        ShapeElement {
            flag_implicit: false,
            flag_link_property: true,
            flag_link: false,
            cardinality: None,
            name: "field2".into(),
        },
    ]);
    assert_eq!(
        test_format_cfg(
            &[
                Value::Object {
                    shape: shape.clone(),
                    fields: vec![Some(Value::Int32(10)), Some(Value::Int32(20)),]
                },
                Value::Object {
                    shape: shape.clone(),
                    fields: vec![Some(Value::Int32(30)), Some(Value::Int32(40)),]
                },
            ],
            Config::new().max_width(60)
        )
        .unwrap(),
        r###"{
  Object {field1: 10, @field2: 20},
  Object {field1: 30, @field2: 40},
}"###
    );
}

#[test]
fn str() {
    assert_eq!(
        test_format(&[Value::Str("hello".into())]).unwrap(),
        "{'hello'}"
    );
    assert_eq!(
        test_format(&[Value::Str("a\nb".into())]).unwrap(),
        "{'a\\nb'}"
    );
    assert_eq!(
        test_format(&[Value::Str("a'b".into())]).unwrap(),
        r"{'a\'b'}"
    );
    assert_eq!(
        test_format_cfg(
            &[Value::Str("a\nb".into())],
            Config::new().expand_strings(true)
        )
        .unwrap(),
        "{\n  'a\nb',\n}"
    );
    assert_eq!(
        test_format_cfg(
            &[Value::Str("a'b".into())],
            Config::new().expand_strings(true)
        )
        .unwrap(),
        r"{'a\'b'}"
    );
}

#[test]
fn bytes() {
    assert_eq!(
        test_format(&[Value::Bytes(Bytes::from_static(b"hello"))]).unwrap(),
        "{b'hello'}"
    );
    assert_eq!(
        test_format(&[Value::Bytes(Bytes::from_static(b"a\nb"))]).unwrap(),
        "{b'a\\nb'}"
    );
    assert_eq!(
        test_format(&[Value::Bytes(Bytes::from_static(b"a'b"))]).unwrap(),
        r"{b'a\'b'}"
    );
}

#[test]
fn all_widths() {
    let shape = ObjectShape::new(vec![ShapeElement {
        flag_implicit: false,
        flag_link_property: false,
        flag_link: false,
        cardinality: None,
        name: "field1".into(),
    }]);
    for width in 0..100 {
        test_format_cfg(
            &[Value::Object {
                shape: shape.clone(),
                fields: vec![Some(Value::Str(
                    "Sint tempor. Qui occaecat eu consectetur elit.".into(),
                ))],
            }],
            Config::new().max_width(width),
        )
        .unwrap();
    }
}

#[test]
fn all_widths_vec_obj() {
    use crate::repl::VectorLimit::*;
    let shape = ObjectShape::new(vec![ShapeElement {
        flag_implicit: false,
        flag_link_property: false,
        flag_link: false,
        cardinality: None,
        name: "field1".into(),
    }]);
    for mvec in [Auto, Unlimited, Fixed(8), Fixed(35)] {
        for width in 0..100 {
            test_format_cfg(
                &[Value::Object {
                    shape: shape.clone(),
                    fields: vec![Some(Value::Vector((0..200).map(|v| v as _).collect()))],
                }],
                Config::new().max_width(width).max_vector_length(mvec),
            )
            .unwrap();
        }
    }
}

#[test]
fn all_widths_vec() {
    use crate::repl::VectorLimit::*;
    for mvec in [Auto, Unlimited, Fixed(8), Fixed(35)] {
        for width in 0..100 {
            test_format_cfg(
                &[Value::Vector((0..200).map(|v| v as _).collect())],
                Config::new().max_width(width).max_vector_length(mvec),
            )
            .unwrap();
        }
    }
}

#[test]
fn all_widths_json() {
    for width in 0..100 {
        json_fmt_width(
            width,
            r###"[
            {"field1": "Sint tempor. Qui occaecat eu consectetur elit."},
            {"field2": "Lorem ipsum dolor sit amet."}
        ]"###,
        );
    }
}

#[test]
fn all_widths_json_item() {
    for width in 0..100 {
        json_fmt_width(
            width,
            r###"[
            {"field1": "Sint tempor. Qui occaecat eu consectetur elit."},
            {"field2": "Lorem ipsum dolor sit amet."}
        ]"###,
        );
    }
}

#[test]
fn json() {
    assert_eq!(json_fmt("[10]"), "[10]");
    assert_eq!(
        json_fmt_width(
            20,
            r###"[
        {"field1": [],
         "field2": {}},
        {"field1": ["x"],
         "field2": {"a": 1}}
    ]
    "###
        ),
        r###"[
  {
    "field1": [],
    "field2": {}
  },
  {
    "field1": ["x"],
    "field2": {
      "a": 1
    }
  }
]"###
    );
}

#[test]
fn postgis_geometry() {
    assert_eq!(
        test_format(&[Value::PostGisGeometry(
            /*
             * Point
             * 01 - byteOrder, Little Endian
             * 01000000 - wkbType, WKBPoint
             * 0000000000000040 - x, 2.0
             * 000000000000F03F - y, 1.0
             */
            b"\
            \x01\
            \x01\x00\x00\x00\
            \x00\x00\x00\x00\x00\x00\x00\x40\
            \x00\x00\x00\x00\x00\x00\xF0\x3F\
            "[..]
                .into()
        ),],)
        .unwrap(),
        r###"{POINT(2 1)}"###
    );
}

#[test]
fn postgis_geography() {
    assert_eq!(
        test_format(&[Value::PostGisGeography(
            /*
             * Point
             * 01 - byteOrder, Little Endian
             * 01000000 - wkbType, WKBPoint
             * 0000000000000040 - x, 2.0
             * 000000000000F03F - y, 1.0
             */
            b"\
            \x01\
            \x01\x00\x00\x00\
            \x00\x00\x00\x00\x00\x00\x00\x40\
            \x00\x00\x00\x00\x00\x00\xF0\x3F\
            "[..]
                .into(),
        ),],)
        .unwrap(),
        r###"{POINT(2 1)}"###
    );
}

#[test]
fn postgis_box_2d() {
    assert_eq!(
        test_format(&[Value::PostGisBox2d(
            /*
             * Polygon
             * 01 - byteOrder, Little Endian
             * 03000000 - wkbType, wkbPolygon
             * 01000000 - numRings, 1
             * 05000000 - numPoints, 5
             * 000000000000F03F - x, 1.0
             * 000000000000F03F - y, 1.0
             * 0000000000000040 - x, 2.0
             * 000000000000F03F - y, 1.0
             * 0000000000000040 - x, 2.0
             * 0000000000000040 - y, 2.0
             * 000000000000F03F - x, 1.0
             * 0000000000000040 - y, 2.0
             * 000000000000F03F - x, 1.0
             * 000000000000F03F - y, 1.0
             */
            b"\
            \x01\
            \x03\x00\x00\x00\
            \x01\x00\x00\x00\
            \x05\x00\x00\x00\
            \x00\x00\x00\x00\x00\x00\xF0\x3F\
            \x00\x00\x00\x00\x00\x00\xF0\x3F\
            \x00\x00\x00\x00\x00\x00\x00\x40\
            \x00\x00\x00\x00\x00\x00\xF0\x3F\
            \x00\x00\x00\x00\x00\x00\x00\x40\
            \x00\x00\x00\x00\x00\x00\x00\x40\
            \x00\x00\x00\x00\x00\x00\xF0\x3F\
            \x00\x00\x00\x00\x00\x00\x00\x40\
            \x00\x00\x00\x00\x00\x00\xF0\x3F\
            \x00\x00\x00\x00\x00\x00\xF0\x3F\
            "[..]
                .into(),
        ),],)
        .unwrap(),
        r###"{POLYGON((1 1,2 1,2 2,1 2,1 1))}"###
    );
}

#[test]
fn postgis_box_3d() {
    assert_eq!(
        test_format(&[Value::PostGisBox2d(
            /*
             * Polygon
             * 01 - byteOrder, Little Endian
             * 03000080 - wkbType, wkbPolygonZ
             * 01000000 - numRings, 1
             * 05000000 - numPoints, 5
             * 000000000000F03F - x, 1.0
             * 000000000000F03F - y, 1.0
             * 0000000000000000 - z, 0.0
             * 0000000000000040 - x, 2.0
             * 000000000000F03F - y, 1.0
             * 0000000000000000 - z, 0.0
             * 0000000000000040 - x, 2.0
             * 0000000000000040 - y, 2.0
             * 0000000000000000 - z, 0.0
             * 000000000000F03F - x, 1.0
             * 0000000000000040 - y, 2.0
             * 0000000000000000 - z, 0.0
             * 000000000000F03F - x, 1.0
             * 000000000000F03F - y, 1.0
             * 0000000000000000 - z, 0.0
             */
            b"\
            \x01\
            \x03\x00\x00\x80\
            \x01\x00\x00\x00\
            \x05\x00\x00\x00\
            \x00\x00\x00\x00\x00\x00\xF0\x3F\
            \x00\x00\x00\x00\x00\x00\xF0\x3F\
            \x00\x00\x00\x00\x00\x00\x08\x40\
            \x00\x00\x00\x00\x00\x00\x00\x40\
            \x00\x00\x00\x00\x00\x00\xF0\x3F\
            \x00\x00\x00\x00\x00\x00\x08\x40\
            \x00\x00\x00\x00\x00\x00\x00\x40\
            \x00\x00\x00\x00\x00\x00\x00\x40\
            \x00\x00\x00\x00\x00\x00\x08\x40\
            \x00\x00\x00\x00\x00\x00\xF0\x3F\
            \x00\x00\x00\x00\x00\x00\x00\x40\
            \x00\x00\x00\x00\x00\x00\x08\x40\
            \x00\x00\x00\x00\x00\x00\xF0\x3F\
            \x00\x00\x00\x00\x00\x00\xF0\x3F\
            \x00\x00\x00\x00\x00\x00\x08\x40\
            "[..]
                .into(),
        ),],)
        .unwrap(),
        r###"{POLYGON((1 1 3,2 1 3,2 2 3,1 2 3,1 1 3))}"###
    );
}

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::convert::TryInto;
use std::str::FromStr;
use anyhow::Context as _;

use colorful::Colorful;
use bigdecimal::BigDecimal;
use edgedb_protocol::codec::{NamedTupleShape};
use edgedb_protocol::value::Value;
use edgedb_protocol::model;
use edgeql_parser::helpers::unquote_string;
use nom::combinator::{recognize, value, map, map_res, opt, cut, flat_map, success, cond};
use nom::bytes::complete::{tag, tag_no_case, take_while, take_while_m_n};
use nom::character::complete::{alphanumeric1, char, digit1, i16, i32, i64, multispace0};
use nom::{IResult, Needed, Parser, InputLength};
use nom::branch::alt;
use nom::Err::{Error, Failure, Incomplete};
use nom::error::{context, ContextError, ErrorKind, FromExternalError, ParseError};
use nom::multi::{many0_count, separated_list0};
use nom::number::complete::{double, float, recognize_float_parts};
use nom::sequence::{delimited, preceded, terminated, tuple};
use num_bigint::ToBigInt;
use rustyline::completion::Completer;
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::{Validator, ValidationResult, ValidationContext};
use rustyline::{Helper, Context};

type ParseResult<'a, I = &'a str, R = Value> = IResult<I, R, ParsingError>;

#[derive(Debug, thiserror::Error)]
pub enum ParsingError {
    #[error("{}", description)]
    Mistake { kind: Option<ErrorKind>, description: String },
    #[error("External error occurred: {}", error)]
    External { kind: Option<ErrorKind>, description: String, error: anyhow::Error },
    #[error("{}", hint.clone().unwrap_or("value is incomplete".to_string()))]
    Incomplete { hint: Option<String> },
}

impl ParseError<&str> for ParsingError {
    // on one line, we show the error code and the input that caused it
    fn from_error_kind(input: &str, kind: ErrorKind) -> Self {
        let message = format!("{:?} failed on the following input: \'{:?}\'", kind.description(), input);
        ParsingError::Mistake { kind: Some(kind), description: message }
    }

    // if combining multiple errors, we show them one after the other
    fn append(input: &str, kind: ErrorKind, other: Self) -> Self {
        let message = format!("{}, then: {}", other, ParsingError::from_error_kind(input, kind));
        ParsingError::Mistake { kind: Some(kind), description: message }
    }

    fn from_char(input: &str, c: char) -> Self {
        let message = if input != "" {
            format!("Expected '{}' in {:?}", c, input)
        } else {
            format!("Expected '{}'", c)
        };
        ParsingError::Mistake { kind: None, description: message }
    }

    fn or(self, other: Self) -> Self {
        let message = format!("{}, or: {}", self, other);
        ParsingError::Mistake { kind: None, description: message }
    }
}

impl ContextError<&str> for ParsingError {
    fn add_context(_input: &str, ctx: &'static str, other: Self) -> Self {
        let message = format!("{} -> {}", ctx, other);
        ParsingError::Mistake { kind: None, description: message }
    }
}

impl FromExternalError<&str, String> for ParsingError {
    fn from_external_error(input: &str, kind: ErrorKind, e: String) -> Self {
        ParsingError::Mistake {
            kind: Some(kind),
            description: format!("{} at {}", e, input),
        }
    }
}

impl FromExternalError<&str, ParsingError> for ParsingError {
    fn from_external_error(_: &str, _: ErrorKind, e: ParsingError) -> Self {
        return e;
    }
}

impl FromExternalError<&str, anyhow::Error> for ParsingError {
    fn from_external_error(input: &str, kind: ErrorKind, e: anyhow::Error) -> Self {
        ParsingError::External {
            error: e,
            kind: Some(kind),
            description: format!("Failed at '{}'", input),
        }
    }
}

bitflags::bitflags! {
    pub struct InputFlags: u8 {
        const NONE = 0;
        const FORCE_QUOTED_STRINGS = 1 << 0;
    }
}

pub trait VariableInput: fmt::Debug + Send + Sync + 'static {
    fn type_name(&self) -> &str;
    fn parse<'a>(&self, input: &'a str, flags: InputFlags) -> ParseResult<'a>;
}

pub fn trailing_separated_list0<I, O, O2, E, F, G>(
  mut sep: G,
  mut f: F,
) -> impl FnMut(I) -> IResult<I, Vec<O>, E>
where
  I: Clone + InputLength,
  F: Parser<I, O, E>,
  G: Parser<I, O2, E>,
  E: ParseError<I>,
{
    move |i: I| {
        let (i1, v) = separated_list0(
            |i| sep.parse(i), |i| f.parse(i)).parse(i)?;
        let (i2, _) = cond(v.len() > 0, opt(|i| sep.parse(i))).parse(i1)?;
        Ok((i2, v))
    }
}


fn white_space<'a, O, E: ParseError<&'a str>, F: Parser<&'a str, O, E>>(
    f: F,
) -> impl Parser<&'a str, O, E> {
    delimited(multispace0, f, multispace0)
}

fn space(i: &str) -> IResult<&str, &str, ParsingError> {
    let chars = " \t\r\n";
    take_while(move |c| chars.contains(c))(i)
}

fn quoted_str(input: &str) -> IResult<&str, String, ParsingError> {
    context(
        "any_quote_str",
        alt((
            single_quoted_str,
            double_quoted_str
        )),
    )(input)
}

fn single_quoted_str(input: &str) -> IResult<&str, String, ParsingError> {
    context(
        "single_quote_str",
        |s| quoted_str_parser(s, '\''),
    )(input)
}

fn double_quoted_str(input: &str) -> IResult<&str, String, ParsingError> {
    context(
        "double_quote_str",
        |s| quoted_str_parser(s, '\"'),
    )(input)
}

fn quoted_str_parser<'a>(input: &'a str, quote: char) -> IResult<&'a str, String, ParsingError> {
    context(
        "quoted_string",
        map_res(
            recognize(
                tuple((
                    char(quote),
                    move |str: &'a str| {
                        let mut pos = 0;
                        let mut prev = None;
                        let mut complete = false;

                        for c in str.chars() {
                            // check for a quote, if prev is none then its the quote is the first char of the string
                            // meaning it isn't escaped; but if we have a previous char then check whether it's the
                            // escape char '\'
                            if c == quote && (prev == None || prev != Some('\\')) {
                                complete = true;
                                break;
                            }

                            pos += 1;
                            prev = Some(c);
                        }

                        if !complete {
                            return Err(Failure(ParsingError::Mistake {
                                kind: None,
                                description: format!("Missing end quote in '{}'", str)
                            }))
                        }

                        // we only need to return the remainder since we're calling 'recognize' above.
                        Ok((&str[pos..], ()))
                    },
                    char(quote),
                )),
            ),
            |s| unquote_string(s).map(|v| v.into()).context("Invalid quoted string"),
        ),
    )(input)
}

#[derive(Debug)]
pub struct Str;

impl VariableInput for Str {
    fn type_name(&self) -> &str { "str" }
    fn parse<'a>(&self, input: &'a str, flags: InputFlags) -> ParseResult<'a> {
        if flags.contains(InputFlags::FORCE_QUOTED_STRINGS) {
            context(
                "quoted_str",
                map(
                    quoted_str,
                    Value::Str,
                ),
            )(input)
        } else {
            context(
                "str",
                |s: &str| Ok(("", Value::Str(s.to_string()))),
            )(input)
        }
    }
}

#[derive(Debug)]
pub struct Uuid;

impl VariableInput for Uuid {
    fn type_name(&self) -> &str { "uuid" }
    fn parse<'a>(&self, input: &'a str, _flags: InputFlags) -> ParseResult<'a> {
        context(
            "uuid",
            map(
                map_res(
                    take_while_m_n(
                        32usize,
                        36usize,
                        |c: char| c.is_alphanumeric() || c == '-',
                    ),
                    |s| uuid::Uuid::from_str(s).context("Cannot parse to UUID"),
                ),
                |v| Value::Uuid(v),
            ),
        )(input)
    }
}

#[derive(Debug)]
pub struct Int16;

impl VariableInput for Int16 {
    fn type_name(&self) -> &str { "int16" }
    fn parse<'a>(&self, input: &'a str, _flags: InputFlags) -> ParseResult<'a> {
        context("int16", map(i16, Value::Int16))(input)
    }
}

#[derive(Debug)]
pub struct Int32;

impl VariableInput for Int32 {
    fn type_name(&self) -> &str { "int32" }
    fn parse<'a>(&self, input: &'a str, _flags: InputFlags) -> ParseResult<'a> {
        context(
            "int32",
            map(i32, Value::Int32),
        )(input)
    }
}

#[derive(Debug)]
pub struct Int64;

impl VariableInput for Int64 {
    fn type_name(&self) -> &str { "int64" }
    fn parse<'a>(&self, input: &'a str, _flags: InputFlags) -> ParseResult<'a> {
        context(
            "int64",
            map(i64, Value::Int64),
        )(input)
    }
}

#[derive(Debug)]
pub struct Float32;

impl VariableInput for Float32 {
    fn type_name(&self) -> &str { "float32" }
    fn parse<'a>(&self, input: &'a str, _flags: InputFlags) -> ParseResult<'a> {
        context("float32", map(float, Value::Float32))(input)
    }
}

#[derive(Debug)]
pub struct Float64;

impl VariableInput for Float64 {
    fn type_name(&self) -> &str { "float64" }
    fn parse<'a>(&self, input: &'a str, _flags: InputFlags) -> ParseResult<'a> {
        context("float64", map(double, Value::Float64))(input)
    }
}

#[derive(Debug)]
pub struct Bool;

impl VariableInput for Bool {
    fn type_name(&self) -> &str { "bool" }
    fn parse<'a>(&self, input: &'a str, _flags: InputFlags) -> ParseResult<'a> {
        context(
            "bool",
            alt((
                value(Value::Bool(true), tag_no_case("true")),
                value(Value::Bool(false), tag_no_case("false"))
            )),
        )(input)
    }
}

#[derive(Debug)]
pub struct BigInt;

impl VariableInput for BigInt {
    fn type_name(&self) -> &str { "bigint" }
    fn parse<'a>(&self, input: &'a str, _flags: InputFlags) -> ParseResult<'a> {
        context(
            "bigint",
            map_res(
                recognize(
                    tuple((
                        opt(alt((char('+'), char('-')))),
                        digit1,
                        opt(tuple((
                            alt((char('e'), char('E'))),
                            opt(alt((char('+'), char('-')))),
                            cut(digit1)
                        )))
                    ))
                ),
                |v: &str| -> Result<Value, anyhow::Error> {
                    let dec: BigDecimal = v.parse()?;
                    let int = dec.to_bigint()
                        .context("number is not an integer")?;
                    let int = int.try_into()?;
                    Ok(Value::BigInt(int))
                },
            ),
        )(input)
    }
}

#[derive(Debug)]
pub struct Decimal;

impl VariableInput for Decimal {
    fn type_name(&self) -> &str { "decimal" }
    fn parse<'a>(&self, input: &'a str, _flags: InputFlags) -> ParseResult<'a> {
        context(
            "decimal",
            map(
                map_res(
                    map_res(
                        recognize(recognize_float_parts),
                        |v| BigDecimal::from_str(v).context("format doesn't represent a big decimal"),
                    ),
                    |v| v.try_into().context("BigDecimal cannot be interpolated"),
                ),
                |v| Value::Decimal(v),
            ),
        )(input)
    }
}

#[derive(Debug)]
pub struct Json;

impl VariableInput for Json {
    fn type_name(&self) -> &str { "json" }
    fn parse<'a>(&self, input: &'a str, _flags: InputFlags) -> ParseResult<'a> {
        context(
            "json",
            |s: &'a str| {
                let de = serde_json::Deserializer::from_str(s);
                let mut stream = de.into_iter::<serde_json::Value>();

                // consume a single json value
                match stream.next() {
                    Some(r) => match r {
                        Ok(_) => {}
                        Err(e) => return Err(Error(ParsingError::External {
                            error: e.into(),
                            kind: None,
                            description: "Failed to parse json token".to_string(),
                        }))
                    }
                    None => return Err(Error(ParsingError::Incomplete { hint: None }))
                }

                // we grab the substring that was successfully parsed from the stream as well as return the slice that
                // wasn't parsed by serde
                Ok((
                    &s[stream.byte_offset()..],
                    Value::Json(
                        model::Json::new_unchecked(s[0..stream.byte_offset()].into())
                    )
                ))
            },
        )(input)
    }
}

#[derive(Debug)]
pub struct Array {
    pub element_type: Arc<dyn VariableInput>,
}

impl VariableInput for Array {
    fn type_name(&self) -> &str { "array" }
    fn parse<'a>(&self, input: &'a str, _flags: InputFlags) -> ParseResult<'a> {
        context(
            "array",
            map(
                preceded(
                    white_space(char('[')),
                    terminated(
                        trailing_separated_list0(
                            white_space(char(',')),
                            |s| self.element_type.parse(
                                s,
                                InputFlags::FORCE_QUOTED_STRINGS,
                            ),
                        ),
                        preceded(
                            space,
                            char(']'),
                        ),
                    ),
                ),
                |v| Value::Array(v),
            ),
        )(input)
    }
}

#[derive(Debug)]
pub struct Tuple {
    pub element_types: Vec<Arc<dyn VariableInput>>,
}

pub struct TupleParser<'a> {
    tuple: &'a Tuple,
}

impl Parser<&str, Value, ParsingError> for TupleParser<'_> {
    fn parse<'a>(&mut self, mut input: &'a str) -> IResult<&'a str, Value, ParsingError> {
        let mut res = Vec::new();
        let mut position = 0;

        loop {
            if position >= self.tuple.element_types.len() {
                // we've read all the elements in the tuple, return the remainder
                return Ok((input, Value::Tuple(res)));
            }

            // match an the element
            match self.tuple.element_types[position].parse(input, InputFlags::FORCE_QUOTED_STRINGS) {
                Err(e) => return Err(e),
                Ok((remainder, result)) => {
                    res.push(result);
                    input = remainder;
                    position += 1;
                }
            }

            // don't try to match a separator if we've match all elements in the tuple
            if position >= self.tuple.element_types.len() {
                return Ok((input, Value::Tuple(res)));
            }

            match white_space(char(',')).parse(input) {
                Err(e) => {
                    if position >= self.tuple.element_types.len() {
                        // end of tuple
                        return Ok((input, Value::Tuple(res)));
                    }

                    return Err(e);
                }
                Ok((remainder, _)) => {
                    input = remainder;
                }
            }
        }
    }
}

impl VariableInput for Tuple {
    fn type_name(&self) -> &str {
        "tuple"
    }

    fn parse<'a>(&self, input: &'a str, _flags: InputFlags) -> ParseResult<'a> {
        let element_parser = TupleParser {
            tuple: self,
        };

        context(
            "tuple",
            preceded(
                white_space(char('(')),
                terminated(
                    element_parser,
                    preceded(
                        space,
                        char(')'),
                    ),
                ),
            ),
        )(input)
    }
}

#[derive(Debug)]
pub struct NamedTuple {
    pub element_types: HashMap<String, Arc<dyn VariableInput>>,
    pub shape: NamedTupleShape
}

impl VariableInput for NamedTuple {
    fn type_name(&self) -> &str {
        "named_tuple"
    }

    fn parse<'a>(&self, input: &'a str, _flags: InputFlags) -> ParseResult<'a> {
        context(
            "named_tuple",
            preceded(
                white_space(char('(')),
                map_res(
                    terminated(
                        trailing_separated_list0(
                            white_space(char(',')),
                            flat_map(
                                terminated(
                                    context(
                                        "identifier",
                                        white_space(
                                            map_res(
                                                recognize(
                                                    many0_count(
                                                        alt((
                                                            alphanumeric1,
                                                            tag("_"),
                                                            tag("-")
                                                        ))
                                                    )
                                                ),
                                                |ident| {
                                                    match self.element_types.get(ident) {
                                                        Some(element_type) => Ok((ident, element_type)),
                                                        None => Err(ParsingError::Mistake {
                                                            kind: None,
                                                            description: format!(
                                                                "Expecting one of the following identifier(s): {}",
                                                                self.shape.elements.iter()
                                                                    .map(|v| v.name.clone())
                                                                    .collect::<Vec<String>>()
                                                                    .join(", ")
                                                            )
                                                        })
                                                    }
                                                }
                                            )
                                        )
                                    ),
                                    context(
                                        "assignment_op",
                                        white_space(tag(":="))
                                    )
                                ),
                                |element| tuple((
                                    success(element.0),
                                    context(
                                        "named_tuple_element",
                                        move |v| element.1.parse(v, InputFlags::FORCE_QUOTED_STRINGS)
                                    )
                                ))
                            )
                        ),
                        preceded(
                            space,
                            char(')'),
                        ),
                    ),
                    |mut result: Vec<(&str, Value)>| {
                        if result.len() < self.shape.elements.len() {
                            return Err(ParsingError::Incomplete {
                                hint: Some(
                                    format!(
                                        "Expecting one of the following element name(s): {}",
                                        self.shape.elements.iter()
                                            .filter(|v| !result.iter().any(|t| t.0 == v.name.as_str()))
                                            .map(|v| v.name.clone())
                                            .collect::<Vec<String>>()
                                            .join(", ")
                                    )
                                )
                            })
                        }

                        // sort the values by the order in the shape
                        result.sort_by(|a, b| -> Ordering {
                            let apos = self.shape.elements.iter().position(|e| e.name == a.0).unwrap();
                            let bpos = self.shape.elements.iter().position(|e| e.name == b.0).unwrap();

                            apos.cmp(&bpos)
                        });

                        Ok(Value::NamedTuple {
                            shape: self.shape.clone(),
                            fields: result.into_iter()
                                .map(|v| v.1)
                                .collect()
                        })
                    }
                ),
            )
        )(input)
    }
}

fn format_parsing_error(e: nom::Err<ParsingError>) -> String {
    format!(" -- {}", match e {
        Error(p) | Failure(p) => match p {
            ParsingError::Mistake {
                kind: _kind,
                description
            } => format!("{}", description),
            ParsingError::External {
                description,
                error,
                kind: _
            } => format!("External error occurred: {} {}", description, error),
            ParsingError::Incomplete { hint: description } => description.unwrap_or("Incomplete input".to_string()),
        },
        Incomplete(Needed::Size(sz)) => format!("Incomplete input, needing {} more chars", sz),
        Incomplete(_n) => "Incomplete input".to_string(),
    })
}

pub struct VarHelper {
    var_type: Arc<dyn VariableInput>,
}

pub struct ErrorHint(String);

impl rustyline::hint::Hint for ErrorHint {
    fn display(&self) -> &str { self.0.as_ref() }
    fn completion(&self) -> Option<&str> { None }
}

impl VarHelper {
    pub fn new(var_type: Arc<dyn VariableInput>) -> VarHelper {
        VarHelper {
            var_type,
        }
    }
}

impl Helper for VarHelper {}

impl Hinter for VarHelper {
    type Hint = ErrorHint;
    fn hint(&self, line: &str, _pos: usize, _ctx: &Context)
            -> Option<Self::Hint>
    {
        if line == "" {  // be friendly from the start
            return None;
        }
        match self.var_type.parse(line, InputFlags::NONE) {
            Ok(r) => {
                if r.0.len() == 0 {
                    return None;
                }

                return Some(ErrorHint(" -- excess unparsed content".to_string()));
            }
            Err(e) => {
                Some(ErrorHint(format_parsing_error(e)))
            }
        }
    }
}

impl Highlighter for VarHelper {
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
        match self.var_type.parse(line, InputFlags::NONE) {
            Ok(r) => {
                if r.0.len() == 0 {
                    return line.into();
                }

                // remove the remaining unparsed content from the original str
                let mut str = line[..(line.len() - r.0.len())].to_string();

                // add it back, but with it highlighted red
                str.push_str(&r.0.light_red().to_string());
                str.into()
            }
            Err(_) => line.light_red().to_string().into(),
        }
    }
    fn has_continuation_prompt(&self) -> bool {
        true
    }
    fn highlight_hint<'h>(&self, hint: &'h str) -> std::borrow::Cow<'h, str> {
        return hint.rgb(0x56, 0x56, 0x56).to_string().into();
    }
    fn highlight_char<'l>(&self, _line: &'l str, _pos: usize) -> bool {
        // needed to highlight hint
        true
    }
}

impl Validator for VarHelper {
    fn validate(&self, ctx: &mut ValidationContext)
                -> Result<ValidationResult, ReadlineError>
    {
        match self.var_type.parse(ctx.input(), InputFlags::NONE) {
            Ok(_) => Ok(ValidationResult::Valid(None)),
            Err(e) => {
                Ok(ValidationResult::Invalid(Some(format_parsing_error(e))))
            }
        }
    }
}

impl Completer for VarHelper {
    type Candidate = String;
    fn complete(&self, _line: &str, pos: usize, _ctx: &Context)
                -> Result<(usize, Vec<Self::Candidate>), ReadlineError>
    {
        Ok((pos, Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::convert::TryFrom;
    use std::str::FromStr;
    use std::sync::Arc;
    use bigdecimal::BigDecimal;
    use edgedb_protocol::codec::NamedTupleShape;
    use edgedb_protocol::descriptors::{TupleElement, TypePos};
    use edgedb_protocol::model;
    use edgedb_protocol::value::Value;
    use crate::prompt::variable::{Array, BigInt, Bool, Decimal, Float32, Float64, InputFlags, Int16, Int32, Int64, Json, NamedTuple, ParseResult, Str, Tuple, Uuid, VariableInput};

    fn assert_value(result: ParseResult, expected: Value) {
        assert!(result.is_ok());

        let parsed = result.unwrap();

        assert!(parsed.0.is_empty());
        assert_eq!(parsed.1, expected);
    }

    fn assert_excess(result: ParseResult, expected: Value) {
        assert!(result.is_ok());

        let parsed = result.unwrap();

        assert!(!parsed.0.is_empty());
        assert_eq!(parsed.1, expected);
    }


    fn assert_error(result: ParseResult) {
        assert!(result.is_err());
    }

    #[test]
    fn test_str() {
        assert_value(Str.parse("ABC123", InputFlags::NONE), Value::Str("ABC123".to_string()));
        assert_value(Str.parse("\"AA\'\\BC", InputFlags::NONE), Value::Str("\"AA\'\\BC".to_string()));
    }


    #[test]
    fn test_quoted_str() {
        assert_value(
            Str.parse("\"ABC\"", InputFlags::FORCE_QUOTED_STRINGS),
            Value::Str("ABC".to_string()),
        );

        assert_value(
            Str.parse(
                "\"DEF \\\" \' \\x23\"", InputFlags::FORCE_QUOTED_STRINGS,
            ),
            Value::Str("DEF \" \' \x23".to_string()),
        );

        assert_value(
            Str.parse(
                "\'\\u263A\'", InputFlags::FORCE_QUOTED_STRINGS,
            ),
            Value::Str("\u{263A}".to_string()),
        );

        assert_value(
            Str.parse(
                "\'\"\\\'\'", InputFlags::FORCE_QUOTED_STRINGS,
            ),
            Value::Str("\"\'".to_string()),
        );

        assert_excess(
            Str.parse("\"\"\"", InputFlags::FORCE_QUOTED_STRINGS),
            Value::Str("".to_string()),
        );

        assert_error(
            Str.parse("\"\\\"", InputFlags::FORCE_QUOTED_STRINGS),
        )
    }

    #[test]
    fn test_uuid() {
        assert_value(
            Uuid.parse("dad2752f-9224-4a1e-93fa-a25ffdfd44ea", InputFlags::NONE),
            Value::Uuid(uuid::Uuid::parse_str("dad2752f-9224-4a1e-93fa-a25ffdfd44ea").unwrap()),
        );

        assert_value(
            Uuid.parse("dad2752f92244a1e93faa25ffdfd44ea", InputFlags::NONE),
            Value::Uuid(uuid::Uuid::parse_str("dad2752f-9224-4a1e-93fa-a25ffdfd44ea").unwrap()),
        );

        assert_error(
            Uuid.parse("dad2752f9224-4a1e-93fa-a25ffdfd44ea", InputFlags::NONE)
        );

        assert_error(
            Uuid.parse("dad2752f92244a1e93faa25ffdfd44eaa", InputFlags::NONE),
        );

        assert_excess(
            Uuid.parse("dad2752f-9224-4a1e-93fa-a25ffdfd44eaa", InputFlags::NONE),
            Value::Uuid(uuid::Uuid::parse_str("dad2752f-9224-4a1e-93fa-a25ffdfd44ea").unwrap()),
        );
    }

    #[test]
    fn test_int16() {
        assert_value(
            Int16.parse("10", InputFlags::NONE),
            Value::Int16(10),
        );

        assert_error(
            Int16.parse("abc", InputFlags::NONE),
        );

        assert_excess(
            Int16.parse("10abc", InputFlags::NONE),
            Value::Int16(10),
        );

        assert_error(
            Int16.parse("32768", InputFlags::NONE),
        );

        assert_error(
            Int16.parse("-32769", InputFlags::NONE),
        );

        assert_value(
            Int16.parse("32767", InputFlags::NONE),
            Value::Int16(32767),
        );
    }

    #[test]
    fn test_int32() {
        assert_value(
            Int32.parse("10", InputFlags::NONE),
            Value::Int32(10),
        );

        assert_error(
            Int32.parse("abc", InputFlags::NONE),
        );

        assert_excess(
            Int32.parse("10abc", InputFlags::NONE),
            Value::Int32(10),
        );

        assert_error(
            Int32.parse("2147483648", InputFlags::NONE),
        );

        assert_error(
            Int32.parse("-2147483649", InputFlags::NONE),
        );

        assert_value(
            Int32.parse("2147483647", InputFlags::NONE),
            Value::Int32(2147483647),
        );
    }

    #[test]
    fn test_int64() {
        assert_value(
            Int64.parse("10", InputFlags::NONE),
            Value::Int64(10),
        );

        assert_error(
            Int64.parse("abc", InputFlags::NONE),
        );

        assert_excess(
            Int64.parse("10abc", InputFlags::NONE),
            Value::Int64(10),
        );

        assert_error(
            Int64.parse("9223372036854775808", InputFlags::NONE),
        );

        assert_error(
            Int64.parse("-9223372036854775809", InputFlags::NONE),
        );

        assert_value(
            Int64.parse("9223372036854775807", InputFlags::NONE),
            Value::Int64(9223372036854775807),
        );
    }

    #[test]
    fn test_f32() {
        assert_value(
            Float32.parse("1.23", InputFlags::NONE),
            Value::Float32(1.23f32),
        );

        assert_value(
            Float32.parse("-1.23", InputFlags::NONE),
            Value::Float32(-1.23f32),
        );

        assert_value(
            Float32.parse("3.40282347e+32", InputFlags::NONE),
            Value::Float32(3.40282347e+32f32),
        );

        assert_excess(
            Float32.parse("-24.a", InputFlags::NONE),
            Value::Float32(-24f32),
        );
    }

    #[test]
    fn test_f64() {
        assert_value(
            Float64.parse("1.23", InputFlags::NONE),
            Value::Float64(1.23f64),
        );

        assert_value(
            Float64.parse("-1.23", InputFlags::NONE),
            Value::Float64(-1.23f64),
        );

        assert_value(
            Float64.parse("3.40282347e+32", InputFlags::NONE),
            Value::Float64(3.40282347e+32f64),
        );

        assert_excess(
            Float64.parse("-24.a", InputFlags::NONE),
            Value::Float64(-24f64),
        );
    }

    #[test]
    fn test_bool() {
        assert_value(
            Bool.parse("true", InputFlags::NONE),
            Value::Bool(true),
        );

        assert_value(
            Bool.parse("false", InputFlags::NONE),
            Value::Bool(false),
        );

        assert_value(
            Bool.parse("TRUE", InputFlags::NONE),
            Value::Bool(true),
        );

        assert_value(
            Bool.parse("FALSE", InputFlags::NONE),
            Value::Bool(false),
        );

        assert_error(
            Bool.parse("ASDF", InputFlags::NONE)
        );

        assert_excess(
            Bool.parse("falsee", InputFlags::NONE),
            Value::Bool(false),
        )
    }

    #[test]
    fn test_bigint() {
        assert_value(
            BigInt.parse("2e2", InputFlags::NONE),
            Value::BigInt(model::BigInt::from(200)),
        );

        assert_value(
            BigInt.parse("-520912125", InputFlags::NONE),
            Value::BigInt(model::BigInt::from(-520912125)),
        );

        assert_excess(
            BigInt.parse("1.23", InputFlags::NONE),
            Value::BigInt(model::BigInt::from(1)),
        );

        assert_error(
            BigInt.parse("-abc", InputFlags::NONE),
        );
    }

    #[test]
    fn test_decimal() {
        assert_value(
            Decimal.parse("2e2", InputFlags::NONE),
            Value::Decimal(model::Decimal::try_from(BigDecimal::from_str("2e2").unwrap()).unwrap()),
        );

        assert_value(
            Decimal.parse("-22.54e229", InputFlags::NONE),
            Value::Decimal(model::Decimal::try_from(BigDecimal::from_str("-22.54e229").unwrap()).unwrap()),
        );

        assert_error(
            Decimal.parse("-abc", InputFlags::NONE),
        );
    }

    #[test]
    fn test_json() {
        assert_value(
            Json.parse("{\"ABC\":123}", InputFlags::NONE),
            Value::Json(model::Json::new_unchecked("{\"ABC\":123}".to_string())),
        );

        assert_value(
            Json.parse("123", InputFlags::NONE),
            Value::Json(model::Json::new_unchecked("123".to_string())),
        );

        assert_value(
            Json.parse("{\"ABC\":[1,2,3]}", InputFlags::NONE),
            Value::Json(model::Json::new_unchecked("{\"ABC\":[1,2,3]}".to_string())),
        );

        assert_error(
            Json.parse("123a", InputFlags::NONE),
        );
    }

    #[test]
    fn test_array() {
        assert_value(
            Array {
                element_type: Arc::new(Str)
            }.parse("['', 'ABC', 'a\"b\\\'c']", InputFlags::NONE),
            Value::Array(vec![
                Value::Str("".to_string()),
                Value::Str("ABC".to_string()),
                Value::Str("a\"b\'c".to_string()),
            ]),
        );

        assert_value(
            Array { element_type: Arc::new(Str) }.parse("[]", InputFlags::NONE),
            Value::Array(vec![]),
        );

        assert_value(
            Array {
                element_type: Arc::new(Json)
            }.parse("[{\"ABC\": [1,2,3]}, [1,2,3,4], 4, \"ABC\"]", InputFlags::NONE),
            Value::Array(vec![
                Value::Json(model::Json::new_unchecked("{\"ABC\": [1,2,3]}".to_string())),
                Value::Json(model::Json::new_unchecked("[1,2,3,4]".to_string())),
                Value::Json(model::Json::new_unchecked("4".to_string())),
                Value::Json(model::Json::new_unchecked("\"ABC\"".to_string())),
            ]),
        );

        assert_value(
            Array {
                element_type: Arc::new(Str)
            }.parse("[\"]\"]", InputFlags::NONE),
            Value::Array(vec![
                Value::Str("]".to_string())
            ]),
        );

        assert_excess(
            Array {
                element_type: Arc::new(Int64)
            }.parse("[1,2,3]4", InputFlags::NONE),
            Value::Array(vec![
                Value::Int64(1),
                Value::Int64(2),
                Value::Int64(3),
            ]),
        );

        assert_value(
            Array {
                element_type: Arc::new(Str)
            }.parse("['ABC',]", InputFlags::NONE),
            Value::Array(vec![
                Value::Str("ABC".to_string()),
            ]),
        );

        assert_error(
            Array {
                element_type: Arc::new(Str)
            }.parse("[,]", InputFlags::NONE)
        );
    }

    #[test]
    fn test_tuple() {
        assert_value(
            Tuple {
                element_types: vec![
                    Arc::new(Int64),
                    Arc::new(Str),
                    Arc::new(Float32),
                ]
            }.parse("(12345, \"ABC123\", 12.34)", InputFlags::NONE),
            Value::Tuple(vec![
                Value::Int64(12345),
                Value::Str("ABC123".to_string()),
                Value::Float32(12.34f32),
            ]),
        );

        assert_value(
            Tuple {
                element_types: vec![
                    Arc::new(Array {
                        element_type: Arc::new(Int64)
                    }),
                    Arc::new(Array {
                        element_type: Arc::new(Str)
                    }),
                    Arc::new(Str),
                ]
            }.parse("([1,5,7], ['ABC', 'de\\\'f\"g', '\\x23ABC'], \"ABC123\")", InputFlags::NONE),
            Value::Tuple(vec![
                Value::Array(vec![
                    Value::Int64(1),
                    Value::Int64(5),
                    Value::Int64(7),
                ]),
                Value::Array(vec![
                    Value::Str("ABC".to_string()),
                    Value::Str("de\'f\"g".to_string()),
                    Value::Str("\x23ABC".to_string()),
                ]),
                Value::Str("ABC123".to_string()),
            ]),
        );

        assert_error(
            Tuple {
                element_types: vec![
                    Arc::new(Str),
                    Arc::new(Int64),
                ]
            }.parse("()", InputFlags::NONE)
        )
    }

    fn create_named_tuple_parser(shape: Vec<(&str, Arc<dyn VariableInput>)>) -> (NamedTupleShape, NamedTuple) {
        let tuple_shape: NamedTupleShape = shape.iter()
            .map(|v| TupleElement {
                name: v.0.to_string(),
                type_pos: TypePos(0)
            })
            .collect::<Vec<TupleElement>>()[..].into();

        let parser = NamedTuple {
            shape: tuple_shape.clone(),
            element_types: shape.into_iter()
                .map(|(n, t)| (n.to_string(), t))
                .collect::<HashMap<String, Arc<dyn VariableInput>>>()
        };

        (tuple_shape.clone(), parser)
    }

    fn assert_named_tuple_value(shape: Vec<(&str, Arc<dyn VariableInput>)>, expected: Vec<Value>, to_parse: &str) {
        let (tuple_shape, parser) = create_named_tuple_parser(shape);

        assert_value(parser.parse(to_parse, InputFlags::NONE), Value::NamedTuple {
            shape: tuple_shape,
            fields: expected
        })
    }

    fn assert_named_tuple_err(shape: Vec<(&str, Arc<dyn VariableInput>)>, to_parse: &str) {
        let (_, parser) = create_named_tuple_parser(shape);

        assert_error(parser.parse(to_parse, InputFlags::NONE))
    }

    fn assert_named_tuple_excess(shape: Vec<(&str, Arc<dyn VariableInput>)>, expected: Vec<Value>, to_parse: &str) {
        let (tuple_shape, parser) = create_named_tuple_parser(shape);

        assert_excess(parser.parse(to_parse, InputFlags::NONE), Value::NamedTuple {
            shape: tuple_shape,
            fields: expected
        })
    }

    #[test]
    fn test_named_tuple() {
        assert_named_tuple_value(
            vec![
                ("abc", Arc::new(Str) as Arc<dyn VariableInput>),
                ("def", Arc::new(Str) as Arc<dyn VariableInput>)
            ],
            vec![
                Value::Str("123".to_string()),
                Value::Str("456".to_string())
            ],
            "(abc := '123', def:='456')"
        );

        assert_named_tuple_value(
            vec![
                ("abc", Arc::new(Str) as Arc<dyn VariableInput>),
                ("def", Arc::new(Str) as Arc<dyn VariableInput>)
            ],
            vec![
                Value::Str("123".to_string()),
                Value::Str("456".to_string())
            ],
            "(abc := '123', def:='456',)"
        );

        assert_named_tuple_value(
            vec![
                ("abc", Arc::new(Str) as Arc<dyn VariableInput>),
                ("def", Arc::new(Str) as Arc<dyn VariableInput>)
            ],
            vec![
                Value::Str("123".to_string()),
                Value::Str("456".to_string())
            ],
            "(def := '456', abc := '123')"
        );

        assert_named_tuple_value(
            vec![
                ("abc", Arc::new(Str)),
                ("def", Arc::new(Array {
                    element_type: Arc::new(Int64)
                })),
                ("ghi", Arc::new(Int32))
            ],
            vec![
                Value::Str("123".to_string()),
                Value::Array(vec![
                    Value::Int64(123),
                    Value::Int64(456),
                    Value::Int64(789),
                ]),
                Value::Int32(-123)
            ],
            "(def := [123,456, 789], abc := '123', ghi := -123)"
        );

        assert_named_tuple_err(
            vec![
                ("abc", Arc::new(Str) as Arc<dyn VariableInput>),
                ("def", Arc::new(Str) as Arc<dyn VariableInput>)
            ],
            "(def := 123, abc := 456)"
        );

        assert_named_tuple_err(
            vec![
                ("abc", Arc::new(Str) as Arc<dyn VariableInput>),
                ("def", Arc::new(Str) as Arc<dyn VariableInput>)
            ],
            "(aaa := 123, abc := 456)"
        );

        assert_named_tuple_excess(
            vec![
                ("abc", Arc::new(Str) as Arc<dyn VariableInput>),
                ("def", Arc::new(Str) as Arc<dyn VariableInput>)
            ],
            vec![
                Value::Str("123".to_string()),
                Value::Str("456".to_string())
            ],
            "(abc := '123', def := '456')abc"
        )
    }
}

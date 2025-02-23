use std::convert::TryFrom;

use proc_macro2::Span;
use proc_macro_error::emit_error;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::token::Paren;

use crate::kw;

#[derive(Debug)]
pub enum FieldAttr {
    Default(syn::Ident),
    DefaultValue(syn::Expr),
    Parse(CliParse),
    Name(syn::LitStr),
    Flatten,
    Value { name: syn::Ident, value: syn::Expr },
}

#[derive(Debug)]
pub enum ContainerAttr {
    Default(syn::Ident),
    Value { name: syn::Ident, value: syn::Expr },
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum SubcommandAttr {
    Name(syn::LitStr),
    Default(syn::Ident),
    Value { name: syn::Ident, value: syn::Expr },
}

pub enum Case {
    Camel,
    Snake,
    Kebab,
    ShoutySnake,
    Mixed,
    Title,
    ShoutyKebab,
}

pub struct ContainerAttrs {
    pub main: bool,
    pub rename_all: Case,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParserKind {
    FromStr,
    TryFromStr,
    FromOsStr,
    TryFromOsStr,
    FromOccurrences,
    FromFlag,
    ValueEnum,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CliParse {
    pub kind: ParserKind,
    pub parser: Option<syn::Expr>,
    pub span: Span,
}

pub struct FieldAttrs {
    pub name: Option<syn::LitStr>,
    pub long: Option<Option<syn::LitStr>>,
    pub short: Option<syn::LitChar>,
    pub subcommand: bool,
    pub flatten: bool,
    pub from_global: bool,
    pub parse: Option<CliParse>,
    pub default_value: Option<syn::Expr>,
}

pub struct SubcommandAttrs {
    pub name: Option<String>,
    pub flatten: bool,
}

struct ContainerAttrList(pub Punctuated<ContainerAttr, syn::Token![,]>);
struct FieldAttrList(pub Punctuated<FieldAttr, syn::Token![,]>);
struct SubcommandAttrList(pub Punctuated<SubcommandAttr, syn::Token![,]>);

fn try_set<T, I>(dest: &mut T, value: I)
where
    T: TryFrom<I, Error = syn::Error>,
{
    T::try_from(value)
        .map(|val| *dest = val)
        .map_err(|e| emit_error!(syn_error_to_proc_macro_diagnostic_to_error(e)))
        .ok();
}

impl Parse for ContainerAttr {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        use ContainerAttr::*;
        let name: syn::Ident = input.parse()?;
        let lookahead = input.lookahead1();
        if lookahead.peek(Paren) {
            let content;
            syn::parenthesized!(content in input);
            let value = content.parse()?;
            Ok(Value { name, value })
        } else if lookahead.peek(syn::Token![=]) {
            let _eq: syn::Token![=] = input.parse()?;
            let value: syn::Expr = input.parse()?;
            Ok(Value { name, value })
        } else if lookahead.peek(syn::Token![,]) || input.cursor().eof() {
            Ok(Default(name))
        } else {
            Err(lookahead.error())
        }
    }
}

impl Parse for FieldAttr {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        use FieldAttr::*;

        let lookahead = input.lookahead1();
        if lookahead.peek(kw::parse) {
            let _parse: kw::parse = input.parse()?;
            let content;
            syn::parenthesized!(content in input);
            let kind = content.parse()?;
            let lookahead = content.lookahead1();
            if content.cursor().eof() {
                Ok(Parse(CliParse {
                    kind,
                    parser: None,
                    span: input.span(),
                }))
            } else if lookahead.peek(syn::Token![=]) {
                let _eq: syn::Token![=] = content.parse()?;
                let parser = content.parse()?;
                Ok(Parse(CliParse {
                    kind,
                    parser: Some(parser),
                    span: content.span(),
                }))
            } else {
                Err(lookahead.error())
            }
        } else if lookahead.peek(kw::name) {
            let _kw: kw::name = input.parse()?;
            let _eq: syn::Token![=] = input.parse()?;
            let name = input.parse()?;
            Ok(Name(name))
        } else if lookahead.peek(kw::default_value) {
            let _kw: kw::default_value = input.parse()?;
            let _eq: syn::Token![=] = input.parse()?;
            let value = input.parse()?;
            Ok(DefaultValue(value))
        } else if lookahead.peek(kw::flatten) {
            let _kw: kw::flatten = input.parse()?;
            Ok(Flatten)
        } else if lookahead.peek(kw::value_enum) {
            let _kw: kw::value_enum = input.parse()?;
            Ok(Parse(CliParse {
                kind: ParserKind::ValueEnum,
                parser: None,
                span: input.span(),
            }))
        } else if lookahead.peek(syn::Ident) {
            let name: syn::Ident = input.parse()?;
            let lookahead = input.lookahead1();
            if lookahead.peek(Paren) {
                let content;
                syn::parenthesized!(content in input);
                let value = content.parse()?;
                Ok(Value { name, value })
            } else if lookahead.peek(syn::Token![=]) {
                let _eq: syn::Token![=] = input.parse()?;
                let value: syn::Expr = input.parse()?;
                Ok(Value { name, value })
            } else if lookahead.peek(syn::Token![,]) || input.cursor().eof() {
                Ok(Default(name))
            } else {
                Err(lookahead.error())
            }
        } else {
            Err(lookahead.error())
        }
    }
}

impl Parse for SubcommandAttr {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        use SubcommandAttr::*;

        let lookahead = input.lookahead1();
        if lookahead.peek(kw::name) {
            let _kw: kw::name = input.parse()?;
            let _eq: syn::Token![=] = input.parse()?;
            let val = input.parse()?;
            Ok(Name(val))
        } else {
            let name: syn::Ident = input.parse()?;
            let lookahead = input.lookahead1();
            if lookahead.peek(Paren) {
                let content;
                syn::parenthesized!(content in input);
                let value = content.parse()?;
                Ok(Value { name, value })
            } else if lookahead.peek(syn::Token![=]) {
                let _eq: syn::Token![=] = input.parse()?;
                let value: syn::Expr = input.parse()?;
                Ok(Value { name, value })
            } else if lookahead.peek(syn::Token![,]) || input.cursor().eof() {
                Ok(Default(name))
            } else {
                Err(lookahead.error())
            }
        }
    }
}

impl Parse for ContainerAttrList {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        Punctuated::parse_terminated(input).map(ContainerAttrList)
    }
}

impl Parse for FieldAttrList {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        Punctuated::parse_terminated(input).map(FieldAttrList)
    }
}

impl Parse for SubcommandAttrList {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        Punctuated::parse_terminated(input).map(SubcommandAttrList)
    }
}

impl ContainerAttrs {
    pub fn from_syn(attrs: &[syn::Attribute]) -> ContainerAttrs {
        use ContainerAttr::*;

        let mut res = ContainerAttrs {
            main: false,
            rename_all: Case::Kebab,
        };
        for attr in attrs {
            if matches!(attr.style, syn::AttrStyle::Outer)
                && (attr.path().is_ident("command") || attr.path().is_ident("arg"))
            {
                let chunk: ContainerAttrList = match attr.parse_args() {
                    Ok(attr) => attr,
                    Err(e) => {
                        emit_error!(syn_error_to_proc_macro_diagnostic_to_error(e));
                        continue;
                    }
                };
                for item in chunk.0 {
                    match item {
                        Value { name, value } if name == "rename_all" => {
                            try_set(&mut res.rename_all, value);
                        }
                        Value { name: _, value: _ } => {}
                        Default(name) if name == "main" => {
                            res.main = true;
                        }
                        Default(name) => {
                            emit_error!(&name, "expected `{}=value`", name);
                        }
                    }
                }
            }
        }
        res
    }
}

impl FieldAttrs {
    pub fn from_syn(attrs: &[syn::Attribute]) -> FieldAttrs {
        use FieldAttr::*;

        let mut res = FieldAttrs {
            name: None,
            short: None,
            long: None,
            subcommand: false,
            flatten: false,
            from_global: false,
            parse: None,
            default_value: None,
        };
        for attr in attrs {
            if matches!(attr.style, syn::AttrStyle::Outer)
                && (attr.path().is_ident("command") || attr.path().is_ident("arg"))
            {
                let chunk: FieldAttrList = match attr.parse_args() {
                    Ok(attr) => attr,
                    Err(e) => {
                        emit_error!(syn_error_to_proc_macro_diagnostic_to_error(e));
                        continue;
                    }
                };
                for item in chunk.0 {
                    match item {
                        Value { name, value } if name == "long" => {
                            match value {
                                syn::Expr::Lit(syn::ExprLit {
                                    lit: syn::Lit::Str(s),
                                    ..
                                }) => res.long = Some(Some(s)),
                                _ => emit_error!(value, "expected string"),
                            };
                        }
                        Value { name, value } if name == "short" => {
                            match value {
                                syn::Expr::Lit(syn::ExprLit {
                                    lit: syn::Lit::Char(s),
                                    ..
                                }) => res.short = Some(s),
                                _ => emit_error!(value, "expected character"),
                            };
                        }
                        Value { name: _, value: _ } => {}
                        Default(name) if name == "long" => {
                            res.long = Some(None);
                        }
                        Flatten => {
                            res.flatten = true;
                        }
                        Default(name) if name == "subcommand" => {
                            res.subcommand = true;
                        }
                        Default(name) if name == "from_global" => {
                            res.from_global = true;
                        }
                        Default(name) => {
                            emit_error!(&name, "expected `{}=value`", name);
                        }
                        DefaultValue(val) => {
                            res.default_value = Some(val);
                        }
                        Parse(parse) => {
                            res.parse = Some(parse);
                        }
                        Name(name) => {
                            res.name = Some(name);
                        }
                    }
                }
            }
        }
        res
    }
}

impl SubcommandAttrs {
    pub fn from_syn(attrs: &[syn::Attribute]) -> SubcommandAttrs {
        use SubcommandAttr::*;

        let mut res = SubcommandAttrs {
            name: None,
            flatten: false,
        };
        for attr in attrs {
            if matches!(attr.style, syn::AttrStyle::Outer)
                && (attr.path().is_ident("arg") || attr.path().is_ident("command"))
            {
                let chunk: SubcommandAttrList = match attr.parse_args() {
                    Ok(attr) => attr,
                    Err(e) => {
                        emit_error!(syn_error_to_proc_macro_diagnostic_to_error(e));
                        continue;
                    }
                };
                for item in chunk.0 {
                    match item {
                        Name(name) => res.name = Some(name.value()),
                        Value { name: _, value: _ } => {}
                        Default(name) if name == "flatten" => {
                            res.flatten = true;
                        }
                        Default(name) => {
                            emit_error!(&name, "expected `{}=value`", name);
                        }
                    }
                }
            }
        }
        res
    }
}

impl TryFrom<syn::Expr> for Case {
    type Error = syn::Error;
    fn try_from(val: syn::Expr) -> syn::Result<Case> {
        match val {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) => {
                let case = match &s.value()[..] {
                    "CamelCase" => Case::Camel,
                    "snake_case" => Case::Snake,
                    "kebab-case" => Case::Kebab,
                    "SHOUTY_SNAKE_CASE" => Case::ShoutySnake,
                    "mixedCase" => Case::Mixed,
                    "Title Case" => Case::Title,
                    "SHOUTY-KEBAB-CASE" => Case::ShoutyKebab,
                    _ => {
                        return Err(syn::Error::new_spanned(
                            s,
                            "undefined case conversion".to_string(),
                        ));
                    }
                };
                Ok(case)
            }
            _ => Err(syn::Error::new_spanned(val, "literal expected")),
        }
    }
}

impl Case {
    pub fn convert(&self, s: &str) -> String {
        match self {
            Case::Camel => heck::ToUpperCamelCase::to_upper_camel_case(s),
            Case::Snake => heck::ToSnakeCase::to_snake_case(s),
            Case::Kebab => heck::ToKebabCase::to_kebab_case(s),
            Case::ShoutySnake => heck::ToShoutySnakeCase::to_shouty_snake_case(s),
            Case::Mixed => heck::ToLowerCamelCase::to_lower_camel_case(s),
            Case::Title => heck::ToTitleCase::to_title_case(s),
            Case::ShoutyKebab => heck::ToShoutyKebabCase::to_shouty_kebab_case(s),
        }
    }
}

impl Parse for ParserKind {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let lookahead = input.lookahead1();
        if lookahead.peek(kw::from_str) {
            let _kw: kw::from_str = input.parse()?;
            Ok(ParserKind::FromStr)
        } else if lookahead.peek(kw::try_from_str) {
            let _kw: kw::try_from_str = input.parse()?;
            Ok(ParserKind::TryFromStr)
        } else if lookahead.peek(kw::from_os_str) {
            let _kw: kw::from_os_str = input.parse()?;
            Ok(ParserKind::FromOsStr)
        } else if lookahead.peek(kw::try_from_os_str) {
            let _kw: kw::try_from_os_str = input.parse()?;
            Ok(ParserKind::TryFromOsStr)
        } else if lookahead.peek(kw::from_occurrences) {
            let _kw: kw::from_occurrences = input.parse()?;
            Ok(ParserKind::FromOccurrences)
        } else if lookahead.peek(kw::from_flag) {
            let _kw: kw::from_flag = input.parse()?;
            Ok(ParserKind::FromFlag)
        } else {
            Err(lookahead.error())
        }
    }
}

/// Converts syn::Error to proc_macro_error::Diagnostic.
///
/// This function is provided by proc_macro_error via From train, but only syn 1, but not syn 2.
/// When proc_macro_error is provides it for syn 2, this function can be removed.
fn syn_error_to_proc_macro_diagnostic_to_error(err: syn::Error) -> proc_macro_error::Diagnostic {
    use proc_macro2::{Delimiter, TokenTree};
    use proc_macro_error::{Diagnostic, DiagnosticExt, Level, SpanRange};

    fn gut_error(ts: &mut impl Iterator<Item = TokenTree>) -> Option<(SpanRange, String)> {
        let first = match ts.next() {
            // compile_error
            None => return None,
            Some(tt) => tt.span(),
        };
        ts.next().unwrap(); // !

        let lit = match ts.next().unwrap() {
            TokenTree::Group(group) => {
                // Currently `syn` builds `compile_error!` invocations
                // exclusively in `ident{"..."}` (braced) form which is not
                // followed by `;` (semicolon).
                //
                // But if it changes to `ident("...");` (parenthesized)
                // or `ident["..."];` (bracketed) form,
                // we will need to skip the `;` as well.
                // Highly unlikely, but better safe than sorry.

                if group.delimiter() == Delimiter::Parenthesis
                    || group.delimiter() == Delimiter::Bracket
                {
                    ts.next().unwrap(); // ;
                }

                match group.stream().into_iter().next().unwrap() {
                    TokenTree::Literal(lit) => lit,
                    _ => unreachable!(),
                }
            }
            _ => unreachable!(),
        };

        let last = lit.span();
        let mut msg = lit.to_string();

        // "abc" => abc
        msg.pop();
        msg.remove(0);

        Some((SpanRange { first, last }, msg))
    }

    let mut ts = err.to_compile_error().into_iter();

    let (span_range, msg) = gut_error(&mut ts).unwrap();
    let mut res = Diagnostic::spanned_range(span_range, Level::Error, msg);

    while let Some((span_range, msg)) = gut_error(&mut ts) {
        res = res.span_range_error(span_range, msg);
    }

    res
}

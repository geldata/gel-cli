use std::collections::HashSet;

use edgeql_parser::keywords::{self, Keyword};
use edgeql_parser::tokenizer::{Kind, Tokenizer};
use std::sync::LazyLock;

use crate::completion::{BackslashFsm, ValidationResult};
use crate::print::style::{Style, Styler};

static UNRESERVED_KEYWORDS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| keywords::UNRESERVED_KEYWORDS.iter().copied().collect());

pub fn edgeql(outbuf: &mut String, text: &str, styler: &Styler) {
    let mut pos = 0;
    let mut token_stream = Tokenizer::new(text);
    for res in &mut token_stream {
        let tok = match res {
            Ok(tok) => tok,
            Err(_) => {
                outbuf.push_str(&text[pos..]);
                return;
            }
        };
        if tok.span.start as usize > pos {
            emit_insignificant(outbuf, styler, &text[pos..tok.span.start as usize]);
        }
        if let Some(st) = token_style(tok.kind, &tok.text) {
            styler.write(st, &tok.text, outbuf);
        } else {
            outbuf.push_str(&tok.text);
        }
        pos = tok.span.end as usize;
    }
    emit_insignificant(outbuf, styler, &text[pos..]);
}

pub fn backslash(outbuf: &mut String, text: &str, styler: &Styler) {
    use crate::commands::backslash;

    let mut pos = 0;
    let mut tokens = backslash::Parser::new(text);
    let mut fsm = BackslashFsm::Command;
    for token in &mut tokens {
        if token.span.0 > pos {
            emit_insignificant(outbuf, styler, &text[pos..token.span.0]);
        }
        let style = match fsm.validate(&token) {
            ValidationResult::Valid => Some(Style::BackslashCommand),
            ValidationResult::Invalid => Some(Style::Error),
            ValidationResult::Unknown => None,
        };
        let value = &text[token.span.0..token.span.1];
        if let Some(st) = style {
            styler.write(st, value, outbuf);
        } else {
            outbuf.push_str(value);
        }
        pos = token.span.1;
        fsm = fsm.advance(token);
    }
    emit_insignificant(outbuf, styler, &text[pos..]);
}

fn emit_insignificant(buf: &mut String, styler: &Styler, mut chunk: &str) {
    while let Some(pos) = chunk.find('#') {
        if let Some(end) = chunk[pos..].find('\n') {
            buf.push_str(&chunk[..pos]);
            styler.write(Style::Comment, &chunk[pos..pos + end], buf);

            // must be unstyled to work well at the end of input
            buf.push('\n');

            chunk = &chunk[pos + end + 1..];
        } else {
            break;
        }
    }
    buf.push_str(chunk);
}

fn token_style(kind: Kind, value: &str) -> Option<Style> {
    use crate::print::style::Style as S;
    use edgeql_parser::tokenizer::Kind as T;

    match kind {
        T::Keyword(Keyword("true" | "false")) => Some(S::Boolean),
        T::Keyword(_) => Some(S::Keyword),
        T::Ident => {
            let lc = value.to_lowercase();
            if UNRESERVED_KEYWORDS.contains(&lc[..]) {
                Some(S::Keyword)
            } else {
                None
            }
        }

        T::At => Some(S::Operator),
        T::Dot => Some(S::Punctuation),
        T::BackwardLink => Some(S::Operator),

        T::Assign => Some(S::Operator),
        T::SubAssign => Some(S::Operator),
        T::AddAssign => Some(S::Operator),
        T::Arrow => Some(S::Operator),
        T::Coalesce => Some(S::Operator),
        T::Namespace => None,
        T::FloorDiv => Some(S::Operator),
        T::Concat => Some(S::Operator),
        T::GreaterEq => Some(S::Operator),
        T::LessEq => Some(S::Operator),
        T::NotEq => Some(S::Operator),
        T::NotDistinctFrom => None,
        T::DistinctFrom => None,
        T::Comma => Some(S::Punctuation),
        T::OpenParen => None,
        T::CloseParen => None,
        T::OpenBracket => None,
        T::CloseBracket => None,
        T::OpenBrace => None,
        T::CloseBrace => None,
        T::Semicolon => Some(S::Punctuation),
        T::Colon => Some(S::Operator),
        T::Add => Some(S::Operator),
        T::Sub => Some(S::Operator),
        T::DoubleSplat => Some(S::Operator),
        T::Mul => Some(S::Operator),
        T::Div => Some(S::Operator),
        T::Modulo => Some(S::Operator),
        T::Pow => Some(S::Operator),
        T::Less => Some(S::Operator),
        T::Greater => Some(S::Operator),
        T::Eq => Some(S::Operator),
        T::Ampersand => Some(S::Operator),
        T::Pipe => Some(S::Operator),
        T::Parameter => None, // TODO (tailhook)
        T::ParameterAndType => None,
        T::DecimalConst => Some(S::Number),
        T::FloatConst => Some(S::Number),
        T::IntConst => Some(S::Number),
        T::BigIntConst => Some(S::Number),
        T::BinStr => Some(S::String),
        T::Str => Some(S::String),
        T::BacktickName => None,
        T::Substitution => Some(S::Decorator),

        T::StrInterpStart => Some(S::String),
        T::StrInterpCont => Some(S::String),
        T::StrInterpEnd => Some(S::String),

        _ => None,
    }
}

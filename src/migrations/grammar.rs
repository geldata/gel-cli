use std::marker::PhantomData;

use combine::easy::{self, Errors, Info};
use combine::error::{StreamError, Tracked};
use combine::parser::combinator::no_partial;
use combine::stream::ResetStream;
use combine::{ParseResult, Parser, Positioned, StreamOnce, position};
use combine::{between, choice, eof, many, opaque, satisfy, skip_many};
use edgeql_parser::helpers::{UnquoteError, unquote_string};
use edgeql_parser::keywords::Keyword;
use edgeql_parser::position::Pos;
use edgeql_parser::tokenizer::{Checkpoint, Kind, Token, Tokenizer};

use crate::migrations::migration::Migration;

type Error<'a> = easy::Error<Token<'a>, Token<'a>>;

#[derive(Debug, Clone)]
pub struct Value<'a> {
    kind: Kind,
    value: &'static str,
    phantom: PhantomData<&'a u8>,
}

#[derive(Debug, Clone)]
pub struct TokenMatch<'a> {
    kind: Kind,
    phantom: PhantomData<&'a u8>,
}

// TODO: remove this in favour of a chumsky parser
pub struct TokenStream<'a>(pub Tokenizer<'a>);

pub fn kw<'s>(value: &'static str) -> impl Parser<TokenStream<'s>, Output = ()> {
    Value {
        kind: Kind::Keyword(Keyword(value)),
        value,
        phantom: PhantomData,
    }
    .map(|_| ())
}

pub fn ident<'s>(value: &'static str) -> impl Parser<TokenStream<'s>, Output = ()> {
    Value {
        kind: Kind::Ident,
        value,
        phantom: PhantomData,
    }
    .map(|_| ())
}

pub fn kind<'x>(kind: Kind) -> TokenMatch<'x> {
    TokenMatch {
        kind,
        phantom: PhantomData,
    }
}

impl<'a> StreamOnce for TokenStream<'a> {
    type Token = Token<'a>;
    type Range = Token<'a>;
    type Position = Pos;
    type Error = Errors<Token<'a>, Token<'a>, Pos>;

    fn uncons(&mut self) -> Result<Self::Token, Error<'a>> {
        match self.0.next() {
            Some(Ok(t)) => Ok(t),
            Some(Err(t)) => Err(Error::message_format(t.message)),
            None => Err(Error::end_of_input()),
        }
    }
}

impl Positioned for TokenStream<'_> {
    fn position(&self) -> Self::Position {
        self.0.current_pos()
    }
}

impl ResetStream for TokenStream<'_> {
    type Checkpoint = Checkpoint;
    fn checkpoint(&self) -> Self::Checkpoint {
        self.0.checkpoint()
    }
    fn reset(&mut self, checkpoint: Checkpoint) -> Result<(), Self::Error> {
        self.0.reset(checkpoint);
        Ok(())
    }
}

impl<'a> Parser<TokenStream<'a>> for Value<'a> {
    type Output = Token<'a>;
    type PartialState = ();

    #[inline]
    fn parse_lazy(
        &mut self,
        input: &mut TokenStream<'a>,
    ) -> ParseResult<Self::Output, Errors<Token<'a>, Token<'a>, Pos>> {
        satisfy(|c: Token<'a>| c.kind == self.kind && c.text.eq_ignore_ascii_case(self.value))
            .parse_lazy(input)
    }

    fn add_error(&mut self, error: &mut Tracked<<TokenStream<'a> as StreamOnce>::Error>) {
        error
            .error
            .add_error(Error::Expected(Info::Static(self.value)));
    }
}

impl<'a> Parser<TokenStream<'a>> for TokenMatch<'a> {
    type Output = Token<'a>;
    type PartialState = ();

    #[inline]
    fn parse_lazy(
        &mut self,
        input: &mut TokenStream<'a>,
    ) -> ParseResult<Self::Output, Errors<Token<'a>, Token<'a>, Pos>> {
        satisfy(|c: Token<'a>| c.kind == self.kind).parse_lazy(input)
    }

    fn add_error(&mut self, error: &mut Tracked<Errors<Token<'a>, Token<'a>, Pos>>) {
        error
            .error
            .add_error(Error::Expected(Info::Owned(format!("{:?}", self.kind))));
    }
}

enum Statement {
    SetMessage(String),
    Ignored,
}

fn chosen_statements<'a>() -> impl Parser<TokenStream<'a>, Output = Statement> {
    use Statement::*;
    kw("set").with(choice((
        ident("message")
            .skip(kind(Kind::Assign))
            .with(kind(Kind::Str))
            .skip(kind(Kind::Semicolon))
            .and_then(|token| -> Result<_, UnquoteError> {
                Ok(SetMessage(unquote_string(&token.text)?.into()))
            }),
        any_statement(),
    )))
}

fn braces<'a>() -> impl Parser<TokenStream<'a>, Output = Statement> {
    use Statement::*;
    opaque!(no_partial(
        between(
            kind(Kind::OpenBrace),
            kind(Kind::CloseBrace),
            skip_many(
                satisfy(|t: Token<'a>| { !matches!(t.kind, Kind::OpenBrace | Kind::CloseBrace) })
                    .map(|_| Ignored)
                    .or(braces())
            )
        )
        .map(|_| Ignored)
    ))
}

fn any_statement<'a>() -> impl Parser<TokenStream<'a>, Output = Statement> {
    use Statement::*;

    skip_many(
        satisfy(|t: Token<'a>| {
            !matches!(t.kind, Kind::Semicolon | Kind::CloseBrace | Kind::OpenBrace)
        })
        .map(|_| Ignored)
        .or(braces()),
    )
    .skip(kind(Kind::Semicolon))
    .map(|_| Ignored)
}

fn statement<'a>() -> impl Parser<TokenStream<'a>, Output = Statement> {
    chosen_statements().or(any_statement())
}

fn migration<'a>() -> impl Parser<TokenStream<'a>, Output = Migration> {
    use Statement::*;

    kw("create")
        .and(kw("migration"))
        .with((position(), kind(Kind::Ident)))
        .skip(kw("onto"))
        .and((position(), kind(Kind::Ident)))
        .and(between(
            kind(Kind::OpenBrace),
            kind(Kind::CloseBrace),
            (position(), many::<Vec<_>, _, _>(statement()), position()),
        ))
        .skip(kind(Kind::Semicolon))
        .skip(eof())
        .and_then(|((id, parent_id), brace_block)| -> Result<_, Error<'_>> {
            let (id_start, id) = id;
            let id_end = id_start.offset as usize + id.text.len();
            let (parent_id_start, parent_id) = parent_id;
            let parent_id_end = parent_id_start.offset as usize + parent_id.text.len();
            let (start, statements, end) = brace_block;
            let mut m = Migration {
                message: None,
                id: id.text.into(),
                id_range: (id_start.offset as usize, id_end),
                parent_id: parent_id.text.into(),
                parent_id_range: (parent_id_start.offset as usize, parent_id_end),
                text_range: (start.offset as usize, end.offset as usize),
            };

            for item in statements {
                match item {
                    SetMessage(text) => {
                        if m.message.is_some() {
                            Err(Error::Message("duplicate `SET message` statement".into()))?;
                        }
                        m.message = Some(text);
                    }
                    Ignored => {}
                }
            }
            Ok(m)
        })
}

pub fn parse_migration(data: &str) -> anyhow::Result<Migration> {
    let mut tokens = TokenStream(Tokenizer::new(data));
    match migration().parse_stream(&mut tokens) {
        ParseResult::CommitOk(res) => Ok(res),
        ParseResult::PeekOk(_) => unreachable!(),
        ParseResult::CommitErr(e) => anyhow::bail!("parse error: {}", e),
        ParseResult::PeekErr(e) => anyhow::bail!("parse error: {:?}", e),
    }
}

#[cfg(test)]
mod test {
    use super::parse_migration;

    #[test]
    fn empty() {
        let m = parse_migration("CREATE MIGRATION u123 ONTO u234 {};").unwrap();
        assert_eq!(m.id, "u123");
        assert_eq!(m.parent_id, "u234");
        assert_eq!(m.message, None);
    }

    #[test]
    fn set_all() {
        let m = parse_migration(
            r###"
            CREATE MIGRATION u234 ONTO u123 {
                    set message := $$ hello world! $$;
            };
        "###,
        )
        .unwrap();
        assert_eq!(m.id, "u234");
        assert_eq!(m.parent_id, "u123");
        assert_eq!(m.message, Some(" hello world! ".into()));
    }

    #[test]
    fn nested_braces() {
        let m = parse_migration(
            r###"
            CREATE MIGRATION u234 ONTO u123 {
              {{};};
              CREATE { };
            };
        "###,
        )
        .unwrap();
        assert_eq!(m.id, "u234");
        assert_eq!(m.parent_id, "u123");
    }

    #[test]
    fn set_duplicate() {
        let m = parse_migration(
            r###"
            CREATE MIGRATION u123 ONTO u234 {
                    set message := 'xxxx';
                    set message := 'yyy';
            };
        "###,
        )
        .unwrap_err();
        assert_eq!(
            m.to_string(),
            "parse error: Parse error at 2:13\n\
             duplicate `SET message` statement\n"
        );
    }

    #[test]
    fn mix() {
        let m = parse_migration(
            r###"
            CREATE MIGRATION m234 ONTO m123 {
                    select 1;
                    set message := 'hello';
                    set some_thing := 123;
                    insert x;
                    set other_thing := 'test';
                    set thing3 := call(235);
            };
        "###,
        )
        .unwrap();
        assert_eq!(m.id, "m234");
        assert_eq!(m.parent_id, "m123");
        assert_eq!(m.message, Some("hello".into()));
    }

    #[test]
    fn mix_braces() {
        let m = parse_migration(
            r###"
            CREATE MIGRATION m567 ONTO m234 {
                    SELECT Obj1 { field1 };
                    set message := 'test test';
            };
        "###,
        )
        .unwrap();
        assert_eq!(m.id, "m567");
        assert_eq!(m.parent_id, "m234");
        assert_eq!(m.message, Some("test test".into()));
    }

    #[test]
    fn err_set1() {
        parse_migration(
            r###"
            CREATE MIGRATION {
                set message := 234;
            };
        "###,
        )
        .unwrap_err();
    }

    #[test]
    fn err_set2() {
        parse_migration(
            r###"
            CREATE MIGRATION {
                set message := 'hello' test;
            };
        "###,
        )
        .unwrap_err();
    }

    #[test]
    fn err_trailing() {
        parse_migration(
            r###"
            CREATE MIGRATION {
                set message := 'hello';
            };
            something
        "###,
        )
        .unwrap_err();
    }
}

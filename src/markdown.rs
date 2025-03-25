use std::sync::LazyLock;

use crate::print;

fn prepare_markdown(text: &str) -> String {
    let mut min_indent = text.len();
    for line in text.lines() {
        let stripped = line.trim_start();
        if stripped.is_empty() {
            continue;
        }
        let indent = line.len() - stripped.len();
        if indent < min_indent {
            min_indent = indent;
        }
    }
    if min_indent == 0 {
        return text.to_string();
    }
    let mut buf = String::with_capacity(text.len());
    for line in text.lines() {
        if line.len() > min_indent {
            buf.push_str(&line[min_indent..]);
        }
        buf.push('\n');
    }
    buf
}

static MADSKIN: LazyLock<termimad::MadSkin> = LazyLock::new(|| {
    use termimad::crossterm::style::{Attribute, Color};

    if !print::use_color() {
        return termimad::MadSkin::no_style();
    }

    let mut skin = termimad::MadSkin::default();
    skin.bold.set_fg(Color::Reset);
    skin.inline_code.set_fg(Color::Reset);
    skin.inline_code.set_bg(Color::Reset);
    skin.inline_code.add_attr(Attribute::Bold);
    skin.code_block.set_fg(Color::Reset);
    skin.code_block.set_bg(Color::Reset);
    skin.code_block.add_attr(Attribute::Bold);
    skin
});

fn parse_markdown(text: &str) -> minimad::Text {
    use minimad::CompositeStyle::{self, *};
    use minimad::Line::*;
    use minimad::{Composite, Text};

    let lines = Text::from(text).lines;
    let mut text = Text {
        lines: Vec::with_capacity(lines.len()),
    };
    for line in lines.into_iter() {
        if let Normal(Composite {
            style,
            compounds: cmps,
        }) = line
        {
            if cmps.is_empty() {
                text.lines.push(Normal(Composite {
                    style,
                    compounds: cmps,
                }));
                continue;
            }
            match (style, text.lines.last_mut()) {
                (_, Some(&mut Normal(Composite { ref compounds, .. }))) if compounds.is_empty() => {
                    text.lines.push(Normal(Composite {
                        style,
                        compounds: cmps,
                    }));
                }
                (
                    Paragraph,
                    Some(&mut Normal(Composite {
                        style: Paragraph,
                        ref mut compounds,
                    })),
                )
                | (
                    Paragraph,
                    Some(&mut Normal(Composite {
                        style: CompositeStyle::ListItem(_),
                        ref mut compounds,
                    })),
                )
                | (
                    Quote,
                    Some(&mut Normal(Composite {
                        style: Quote,
                        ref mut compounds,
                    })),
                ) => {
                    compounds.push(minimad::Compound::raw_str(" "));
                    compounds.extend(cmps);
                }
                _ => {
                    text.lines.push(Normal(Composite {
                        style,
                        compounds: cmps,
                    }));
                }
            }
        }
    }
    text
}

pub fn format_title(text: &str) -> String {
    let text = prepare_markdown(text);
    let mut text = parse_markdown(&text);
    if !text.lines.is_empty() {
        text.lines.drain(1..);
    }
    let fmt = termimad::FmtText::from_text(&MADSKIN, text, None);
    fmt.to_string().trim().to_string()
}

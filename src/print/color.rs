use std::sync::LazyLock;

use colorful::core::color_string::CString;
use colorful::{Color, Colorful};

use super::style::Style;

pub trait Highlight: colorful::Colorful + colorful::core::StrMarker + Sized {
    fn unstyled(self) -> CString {
        CString::new(self.to_str())
    }

    fn muted(self) -> CString {
        if let Some(t) = THEME.as_ref() {
            self.color(t.muted)
        } else {
            CString::new(self)
        }
    }

    fn danger(self) -> CString {
        if let Some(t) = THEME.as_ref() {
            self.color(t.danger)
        } else {
            CString::new(self)
        }
    }

    fn success(self) -> CString {
        if let Some(t) = THEME.as_ref() {
            self.color(t.success)
        } else {
            CString::new(self)
        }
    }

    fn warning(self) -> CString {
        if let Some(t) = THEME.as_ref() {
            self.color(t.warning)
        } else {
            CString::new(self)
        }
    }

    fn emphasized(self) -> CString {
        if THEME.is_some() {
            self.bold()
        } else {
            CString::new(self)
        }
    }
}

impl<T: colorful::Colorful + colorful::core::StrMarker + Sized> Highlight for T {}

pub static TERMINAL_LUMA: LazyLock<Option<f32>> = LazyLock::new(|| {
    if !concolor::get(concolor::Stream::Stdout).color() {
        return None;
    }

    terminal_light::luma_with_timeout(std::time::Duration::from_millis(5000)).ok()
});

static THEME: std::sync::LazyLock<Option<Theme>> = std::sync::LazyLock::new(|| {
    if !concolor::get(concolor::Stream::Stdout).color() {
        return None;
    }

    let is_term_light = TERMINAL_LUMA.map_or(false, |x| x > 0.6);

    Some(if is_term_light {
        Theme {
            muted: Color::Grey63,
            danger: Color::DarkRed1,
            success: Color::DarkGreen,
            warning: Color::Yellow,

            syntax_string: Color::DarkOliveGreen3a,
            syntax_set: Color::SteelBlue,
            syntax_object: Color::Grey37,
            syntax_link_property: Color::IndianRed1b,
            syntax_number: Color::CadetBlue1,
            syntax_boolean: Color::LightSalmon3b,
            syntax_enum: Color::DarkGoldenrod,
            syntax_uuid: Color::LightGoldenrod3,
            syntax_keyword: Color::DarkRed2,
            syntax_operator: Color::DarkRed2,
            syntax_comment: Color::Grey35,
            syntax_cast: Color::DarkRed2,
            syntax_backslash: Color::DarkRed1,
        }
    } else {
        Theme {
            muted: Color::Grey37,
            danger: Color::LightRed,
            success: Color::Green,
            warning: Color::LightYellow,

            syntax_string: Color::DarkOliveGreen3a,
            syntax_set: Color::SteelBlue,
            syntax_object: Color::Grey63,
            syntax_link_property: Color::IndianRed1b,
            syntax_number: Color::CadetBlue1,
            syntax_boolean: Color::LightSalmon3b,
            syntax_enum: Color::DarkGoldenrod,
            syntax_uuid: Color::LightGoldenrod3,
            syntax_keyword: Color::IndianRed1b,
            syntax_operator: Color::IndianRed1b,
            syntax_comment: Color::Grey66,
            syntax_cast: Color::IndianRed1b,
            syntax_backslash: Color::IndianRed1c,
        }
    })
});

struct Theme {
    muted: Color,
    danger: Color,
    success: Color,
    warning: Color,

    syntax_string: Color,
    syntax_set: Color,
    syntax_object: Color,
    syntax_link_property: Color,
    syntax_number: Color,
    syntax_boolean: Color,
    syntax_enum: Color,
    syntax_uuid: Color,
    syntax_keyword: Color,
    syntax_operator: Color,
    syntax_comment: Color,
    syntax_cast: Color,
    syntax_backslash: Color,
}

pub(super) fn apply_syntax_style(style: Style, data: &str) -> CString {
    if let Some(theme) = super::color::THEME.as_ref() {
        match style {
            Style::Comment => data.color(theme.syntax_comment),
            Style::String => data.color(theme.syntax_string),
            Style::Set => data.color(theme.syntax_set),
            Style::Object => data.color(theme.syntax_object),
            Style::Number => data.color(theme.syntax_number),
            Style::Boolean => data.color(theme.syntax_boolean),
            Style::UUID => data.color(theme.syntax_uuid),
            Style::Enum => data.color(theme.syntax_enum),
            Style::Cast => data.color(theme.syntax_cast),
            Style::LinkProperty => data.color(theme.syntax_link_property),

            Style::Keyword => data.color(theme.syntax_keyword),
            Style::Operator => data.color(theme.syntax_operator),
            Style::BackslashCommand => data.color(theme.syntax_backslash).bold(),
            Style::Error => data.color(theme.danger),

            Style::Decorator
            | Style::Array
            | Style::Tuple
            | Style::TupleField
            | Style::Pointer
            | Style::Punctuation => CString::new(data),
        }
    } else {
        CString::new(data)
    }
}

#[macro_export]
macro_rules! msg {
    ($($tt:tt)*) => {
        eprintln!($($tt)*);
    }
}

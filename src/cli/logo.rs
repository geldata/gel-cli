use std::io::IsTerminal;
use std::io::{stdout, Write};

use crate::print;
use anes::*;

macro_rules! write_ansi {
    ($($args:tt)*) => {
        _ = write!(stdout(), "{}",$($args)*);
    }
}

pub fn print_logo(allow_animation: bool, small: bool) {
    if !cfg!(feature = "gel") {
        return;
    }

    if !print::use_utf8() {
        return;
    }

    let logo = if small {
        include_str!("logo_blocks.txt")
    } else {
        include_str!("logo.txt")
    };

    let lines = logo.lines().collect::<Vec<_>>();
    let line_count = lines.len() as u16;
    let line_width = lines
        .iter()
        .map(|line: &&str| line.chars().count())
        .max()
        .unwrap();

    let is_light = print::TERMINAL_LUMA.map_or(false, |x| x > 0.6);
    let primary = if is_light {
        Color::DarkMagenta
    } else {
        Color::Magenta
    };
    let secondary = if is_light {
        Color::DarkYellow
    } else {
        Color::Yellow
    };

    let normal = |c| {
        write_ansi!(ResetAttributes);
        write_ansi!(SetAttribute(Attribute::Bold));
        if c == '$' || c == '█' || c == '▄' || c == '▀' {
            write_ansi!(SetForegroundColor(primary));
        } else {
            write_ansi!(SetForegroundColor(secondary));
        }
        write_ansi!(SetAttribute(Attribute::Bold));
    };

    let highlight = || {
        write_ansi!(SetForegroundColor(Color::White));
    };

    if !cfg!(windows) && allow_animation && stdout().is_terminal() && print::use_color() {
        const TRAILING_HIGHLIGHT_COLS: usize = 5;
        const FRAME_DELAY: u64 = 20;

        for _ in 0..line_count {
            eprintln!();
        }

        write_ansi!(MoveCursorUp(line_count + 1));

        for line in &lines {
            for char in line.chars() {
                normal(char);
                write_ansi!(char);
            }
            write_ansi!("\n");
            std::thread::sleep(std::time::Duration::from_millis(FRAME_DELAY));
        }
        write_ansi!("\n");

        write_ansi!(SaveCursorPosition);
        write_ansi!(HideCursor);

        for col in 0..line_width + TRAILING_HIGHLIGHT_COLS {
            _ = stdout().flush();
            std::thread::sleep(std::time::Duration::from_millis(FRAME_DELAY));

            write_ansi!(MoveCursorUp(line_count + 1));
            for line in &lines {
                // Unhighlight the previous trailing column
                if col >= TRAILING_HIGHLIGHT_COLS {
                    write_ansi!(MoveCursorLeft(TRAILING_HIGHLIGHT_COLS as u16));
                    let char = line
                        .chars()
                        .nth(col - TRAILING_HIGHLIGHT_COLS)
                        .unwrap_or(' ');
                    normal(char);
                    write_ansi!(char);
                    if TRAILING_HIGHLIGHT_COLS > 1 {
                        write_ansi!(MoveCursorRight((TRAILING_HIGHLIGHT_COLS - 1) as u16));
                    }
                }
                if let Some(c) = line.chars().nth(col) {
                    highlight();
                    write_ansi!(c);
                } else {
                    normal(' ');
                    write_ansi!(' ');
                }
                write_ansi!(MoveCursorLeft(1_u16));
                write_ansi!(MoveCursorDown(1_u16));
            }
            write_ansi!(MoveCursorDown(1_u16));
            write_ansi!(MoveCursorRight(1_u16));
        }
        write_ansi!(ShowCursor);
        write_ansi!(RestoreCursorPosition);
        write_ansi!(ResetAttributes);
    } else if print::use_color() {
        for line in &lines {
            for char in line.chars() {
                normal(char);
                write_ansi!(char);
            }
            write_ansi!(ResetAttributes);
            write_ansi!("\n");
        }
    } else {
        println!("{}", logo);
    }
}

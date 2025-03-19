use std::cmp::{max, min};
use std::fmt::{self, Write};

use terminal_size::{Width, terminal_size};
use unicode_width::UnicodeWidthChar;

use crate::print::Highlight;

pub trait Contents {
    fn width_bounds(&self) -> (usize, usize);
    fn height(&self, width: usize) -> usize;
    fn render(&self, width: usize, height: usize, f: &mut fmt::Formatter) -> fmt::Result;
}

struct BufRender<'a> {
    cell: &'a dyn Contents,
    width: usize,
    height: usize,
}

#[derive(Debug, PartialEq, Clone)]
enum TextState {
    Normal,
    Escape,
    Bracket,
}

pub struct Counter {
    pub width: usize,
    pub offset: usize,
    state: TextState,
}

pub struct HardWrap<T: fmt::Write> {
    inner: T,
    counter: Counter,
    width: usize,
}

pub struct Float(pub f64);
pub struct Right<T: fmt::Display>(pub T);
pub struct WordList<T: Words>(pub T);

pub trait Words {
    fn print<T: Printer>(&self, p: &mut T) -> fmt::Result;
}

pub trait Printer {
    fn word(&mut self, word: impl fmt::Display) -> fmt::Result;
    fn words(&mut self, iter: impl Iterator<Item = impl fmt::Display>) -> fmt::Result {
        for word in iter {
            self.word(word)?;
        }
        Ok(())
    }
}

struct WidthPrinter {
    min_width: usize,
    width: usize,
    first: bool,
}

struct HeightPrinter {
    width: usize,
    height: usize,
    column: usize,
}

struct TextPrinter<'a, 'b: 'a> {
    fmt: &'a mut fmt::Formatter<'b>,
    width: usize,
    column: usize,
}

pub fn render(title: Option<impl fmt::Display>, table: &[Vec<Box<dyn Contents + '_>>]) {
    let width = terminal_size()
        .map(|(Width(w), _h)| w.into())
        .unwrap_or(200);
    let cols = table.iter().map(|r| r.len()).max().unwrap_or(1);
    let width_bounds = (0..cols)
        .map(|c| {
            table
                .iter()
                .map(|r| r.get(c).map(|c| c.width_bounds()).unwrap_or((0, 0)))
                .fold((0, 0, 0), |(fmin, fmax, sum), (cmin, cmax)| {
                    (max(fmin, cmin), max(fmax, cmax), sum + cmax)
                })
        })
        .collect::<Vec<(usize, usize, usize)>>();
    let borders = cols - 1; // TODO (tailhook)
    let min_width = width_bounds.iter().map(|&(m, _, _)| m).sum::<usize>() + borders;
    let max_width = width_bounds.iter().map(|&(_, m, _)| m).sum::<usize>() + borders;
    let widths = if max_width <= width {
        width_bounds.iter().map(|(_min, max, _sum)| *max).collect()
    } else if min_width >= width {
        let (max_i, max_col) = width_bounds
            .iter()
            .map(|(m, _, _)| *m)
            .enumerate()
            .max_by_key(|(_n, v)| *v)
            .expect("at least one column must exist");
        if min_width - max_col < width {
            // The heuristic is that if the widest column's min width is too
            // big, we can just cut that one.
            let mut widths: Vec<_> = width_bounds.iter().map(|(min, _max, _sum)| *min).collect();
            widths[max_i] = width - (min_width - max_col);
            widths
        } else {
            // something is too wrong, let's bail out
            width_bounds.iter().map(|(min, _max, _sum)| *min).collect()
        }
    } else {
        let sum: usize = width_bounds.iter().map(|(_, _, s)| s).sum();
        let to_divide: usize = width - min_width;
        let mut widths = Vec::with_capacity(cols);
        let mut total_width = 0;
        for &(cmin, _, csum) in &width_bounds {
            let cwidth = cmin + (to_divide as f64 * csum as f64 / sum as f64) as usize;
            widths.push(cwidth);
            total_width += cwidth;
        }
        let mut rem_width = width - total_width - borders;
        for idx in (0..cols).cycle() {
            if rem_width == 0 {
                break;
            }
            if widths[idx] < width_bounds[idx].1 {
                widths[idx] += 1;
                rem_width -= 1;
            }
        }
        widths
    };
    let heights = table
        .iter()
        .map(|row| {
            row.iter()
                .zip(&widths)
                .map(|(cell, &width)| cell.height(width))
                .max()
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();

    if let Some(title) = title {
        print_title(title, min(width, max_width));
    }
    let mut buffers = widths
        .iter()
        .map(|w| String::with_capacity(*w))
        .collect::<Vec<_>>();
    let mut line_buf = String::with_capacity(width + 1);
    for (row, height) in table.iter().zip(heights) {
        for buf in &mut buffers {
            buf.truncate(0);
        }
        for (idx, (cell, &width)) in row.iter().zip(&widths).enumerate() {
            write!(
                &mut buffers[idx],
                "{}",
                BufRender {
                    cell: &**cell,
                    width,
                    height
                }
            )
            .ok();
        }
        let mut lines = buffers
            .iter()
            .map(|text| text.lines().peekable())
            .collect::<Vec<_>>();
        while lines.iter_mut().any(|l| l.peek().is_some()) {
            let mut next_col = 0;
            let mut col = 0;
            for (iter, width) in lines.iter_mut().zip(&widths) {
                if col < next_col {
                    for _ in 0..next_col - col {
                        line_buf.push(' ');
                    }
                    col = next_col;
                }
                next_col = col + width + 1;
                if let Some(line) = iter.next() {
                    col += str_width(line);
                    line_buf.push_str(line);
                }
            }
            println!("{line_buf}");
            line_buf.truncate(0);
        }
    }
}

pub fn print_title(title: impl fmt::Display, width: usize) {
    let width = min(
        terminal_size()
            .map(|(Width(w), _h)| w.into())
            .unwrap_or(width),
        width,
    );
    let twidth = display_width(&title) + 2;
    let filler = max(width.saturating_sub(twidth) / 2, 5);
    println!(
        "{} {} {}",
        format_args!("{0:─^filler$}", "").to_string().muted(),
        title.to_string().emphasized(),
        format_args!("{0:─^filler$}", "").to_string().muted(),
    );
}

impl fmt::Display for BufRender<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.cell.render(self.width, self.height, f)
    }
}

impl<T: fmt::Display> Contents for T {
    fn width_bounds(&self) -> (usize, usize) {
        let mut cnt = Counter::new();
        write!(&mut cnt, "{self}").expect("can write into counter");
        (cnt.width, cnt.width)
    }
    fn height(&self, _width: usize) -> usize {
        1
    }
    fn render(&self, _width: usize, _height: usize, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl<T: fmt::Display> Contents for Right<T> {
    fn width_bounds(&self) -> (usize, usize) {
        let mut cnt = Counter::new();
        write!(&mut cnt, "{}", self.0).expect("can write into counter");
        (cnt.width, cnt.width)
    }
    fn height(&self, _width: usize) -> usize {
        1
    }
    fn render(&self, width: usize, _height: usize, f: &mut fmt::Formatter) -> fmt::Result {
        let inner_width = self.width_bounds().0;
        write!(f, "{0:>1$}{2}", "", width - inner_width, self.0)
    }
}

impl Contents for Float {
    fn width_bounds(&self) -> (usize, usize) {
        let mut cnt = Counter::new();
        write!(&mut cnt, "{:.1}", self.0).expect("can write into counter");
        (cnt.width, cnt.width)
    }
    fn height(&self, _width: usize) -> usize {
        1
    }
    fn render(&self, width: usize, _height: usize, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{1:>0$.1}", width, self.0)
    }
}

impl Counter {
    pub fn new() -> Counter {
        Counter {
            width: 0,
            offset: 0,
            state: TextState::Normal,
        }
    }
    pub fn add_char(&mut self, c: char) {
        use TextState::*;
        match self.state {
            Escape => {
                if c == '[' {
                    self.state = Bracket;
                } else {
                    self.state = Normal;
                }
            }
            Bracket => match c {
                ';' | '?' | '0'..='9' => {}
                _ => self.state = Normal,
            },
            Normal if c == '\x1b' => {
                self.state = Escape;
            }
            Normal => {
                self.width += c.width().unwrap_or(0);
                self.offset += c.len_utf8();
            }
        }
    }
}

impl fmt::Write for Counter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            self.add_char(c);
        }
        Ok(())
    }
}

pub fn str_width(s: &str) -> usize {
    let mut cnt = Counter::new();
    for c in s.chars() {
        cnt.add_char(c);
    }
    cnt.width
}

pub fn display_width(v: impl fmt::Display) -> usize {
    let mut cnt = Counter::new();
    write!(&mut cnt, "{v}").expect("can write into counter");
    cnt.width
}

impl<T: Words> Contents for WordList<T> {
    fn width_bounds(&self) -> (usize, usize) {
        let mut prn = WidthPrinter {
            min_width: 0,
            width: 0,
            first: true,
        };
        self.0
            .print(&mut prn)
            .expect("width printing always succeeds");
        (prn.min_width, prn.width)
    }
    fn height(&self, width: usize) -> usize {
        let mut prn = HeightPrinter {
            width,
            height: 1,
            column: 0,
        };
        self.0
            .print(&mut prn)
            .expect("height printing always succeeds");
        prn.height
    }
    fn render(&self, width: usize, _height: usize, f: &mut fmt::Formatter) -> fmt::Result {
        let mut prn = TextPrinter {
            fmt: f,
            width,
            column: 0,
        };
        self.0.print(&mut prn)
    }
}

impl Printer for WidthPrinter {
    fn word(&mut self, word: impl fmt::Display) -> fmt::Result {
        let item_width = display_width(word);
        self.min_width = max(self.min_width, item_width);
        self.width += item_width;
        if self.first {
            self.first = false;
        } else {
            self.width += " ".len();
        }
        Ok(())
    }
}

impl Printer for HeightPrinter {
    fn word(&mut self, word: impl fmt::Display) -> fmt::Result {
        let item_width = display_width(word);
        let mut space_width = if self.column == 0 { 0 } else { " ".len() };
        if self.column > 0 && self.column + space_width + item_width > self.width {
            self.height += 1;
            self.column = 0;
            space_width = 0;
        }
        self.column += space_width + item_width;
        // min_width is not enough, hard wrap things
        if self.column > self.width {
            self.height += (self.column + self.width - 1) / self.width - 1;
            self.column = self.width; // wrap on next word
        }
        Ok(())
    }
}

impl Printer for TextPrinter<'_, '_> {
    fn word(&mut self, word: impl fmt::Display) -> fmt::Result {
        let item_width = display_width(&word);
        let mut space_width = if self.column == 0 { 0 } else { " ".len() };
        if self.column > 0 && self.column + space_width + item_width > self.width {
            self.fmt.write_char('\n')?;
            self.column = 0;
            space_width = 0;
        }
        if space_width > 0 {
            self.fmt.write_char(' ')?;
        }
        self.column += space_width + item_width;
        if self.column > self.width {
            let mut wrapper = HardWrap {
                inner: &mut self.fmt,
                counter: Counter::new(),
                width: self.width,
            };
            write!(&mut wrapper, "{word}")?;
            if wrapper.counter.width > 0 {
                self.fmt.write_char('\n')?;
            }
            self.column = 0;
        } else {
            word.fmt(self.fmt)?;
        }
        Ok(())
    }
}

impl<T: fmt::Write> fmt::Write for HardWrap<T> {
    fn write_char(&mut self, c: char) -> fmt::Result {
        self.counter.add_char(c);
        self.inner.write_char(c)?;
        if self.counter.width >= self.width {
            self.inner.write_char('\n')?;
            self.counter.width = 0;
        }
        Ok(())
    }
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            self.write_char(c)?;
        }
        Ok(())
    }
}

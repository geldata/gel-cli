use prettytable::format::{Alignment, TableFormat};
use prettytable::format::{FormatBuilder, LinePosition, LineSeparator};
pub use prettytable::{Attr, Cell, Row, Table};
use std::sync::LazyLock;

pub static FORMAT: LazyLock<TableFormat> = LazyLock::new(|| {
    FormatBuilder::new()
        .column_separator('│')
        .borders('│')
        .separators(&[LinePosition::Top], LineSeparator::new('─', '┬', '┌', '┐'))
        .separators(
            &[LinePosition::Title],
            LineSeparator::new('─', '┼', '├', '┤'),
        )
        .separators(
            &[LinePosition::Bottom],
            LineSeparator::new('─', '┴', '└', '┘'),
        )
        .padding(1, 1)
        .build()
});

pub fn header_cell(title: &str) -> Cell {
    Cell::new_align(title, Alignment::LEFT).with_style(Attr::Dim)
}

pub fn settings(rows: &[(&str, String)]) {
    let mut table = Table::new();
    for (title, value) in rows {
        table.add_row(Row::new(vec![Cell::new(title), Cell::new(value)]));
    }
    table.set_format(*FORMAT);
    table.printstd();
}

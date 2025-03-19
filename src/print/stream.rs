use std::convert::Infallible;
use std::io::{self, Write};

use super::Stdout;

pub(in crate::print) trait Output {
    type Error;
    fn write(&mut self, data: &str) -> Result<(), Self::Error>;
}

impl Output for &mut String {
    type Error = Infallible;
    fn write(&mut self, data: &str) -> Result<(), Infallible> {
        self.push_str(data);
        Ok(())
    }
}

impl Output for Stdout {
    type Error = io::Error;
    fn write(&mut self, data: &str) -> Result<(), io::Error> {
        io::stdout().lock().write_all(data.as_bytes())?;
        Ok(())
    }
}

use std::io::{Write, stderr};
use std::time::{Duration, Instant};

pub struct Time {
    start: Instant,
}

impl Time {
    pub fn measure() -> Time {
        Time {
            start: Instant::now(),
        }
    }
}

impl Drop for Time {
    fn drop(&mut self) {
        let dur = self.start.elapsed();
        let rounded = Duration::new(dur.as_secs(), 0);
        write!(stderr(), "(took {}) ", humantime::format_duration(rounded)).ok();
    }
}

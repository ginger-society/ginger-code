pub const GREEN:  &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const RED:    &str = "\x1b[31m";
pub const CYAN:   &str = "\x1b[36m";
pub const BOLD:   &str = "\x1b[1m";
pub const RESET:  &str = "\x1b[0m";

pub fn is_tty() -> bool {
    unsafe { libc::isatty(1) == 1 }
}

pub struct Colour {
    on: bool,
}

impl Colour {
    pub fn new() -> Self {
        Self { on: is_tty() }
    }

    pub fn paint(&self, code: &'static str, s: &str) -> String {
        if self.on {
            format!("{code}{s}{RESET}")
        } else {
            s.to_string()
        }
    }
}
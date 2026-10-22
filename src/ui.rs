//! Human-facing output. Tests swap stdout for a [`SharedBuf`] and assert on the text.

use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;

use console::Style;

pub struct Ui {
    out: RefCell<Box<dyn Write>>,
    color: bool,
}

impl Ui {
    pub fn stdout() -> Ui {
        Ui {
            out: RefCell::new(Box::new(io::stdout())),
            color: console::colors_enabled(),
        }
    }

    pub fn capture() -> (Ui, SharedBuf) {
        let buf = SharedBuf::default();
        (
            Ui {
                out: RefCell::new(Box::new(buf.clone())),
                color: false,
            },
            buf,
        )
    }

    pub fn line(&self, s: impl AsRef<str>) {
        let mut out = self.out.borrow_mut();
        // A closed stdout (e.g. `adopt list | head`) is not worth an error.
        let _ = writeln!(out, "{}", s.as_ref());
        let _ = out.flush();
    }

    pub fn ok(&self, s: impl AsRef<str>) {
        self.line(format!(
            "{} {}",
            self.paint(Style::new().green(), "✔"),
            s.as_ref()
        ));
    }

    pub fn warn(&self, s: impl AsRef<str>) {
        self.line(format!(
            "{} {}",
            self.paint(Style::new().yellow(), "!"),
            s.as_ref()
        ));
    }

    pub fn fail(&self, s: impl AsRef<str>) {
        self.line(format!(
            "{} {}",
            self.paint(Style::new().red(), "✘"),
            s.as_ref()
        ));
    }

    /// Something `--dry-run` skipped.
    pub fn would(&self, s: impl AsRef<str>) {
        self.line(self.dim(format!("[dry-run] {}", s.as_ref())));
    }

    pub fn dim(&self, s: impl AsRef<str>) -> String {
        self.paint(Style::new().dim(), s.as_ref())
    }

    pub fn bold(&self, s: impl AsRef<str>) -> String {
        self.paint(Style::new().bold(), s.as_ref())
    }

    pub fn paint(&self, style: Style, s: &str) -> String {
        style.force_styling(self.color).apply_to(s).to_string()
    }
}

#[derive(Clone, Default)]
pub struct SharedBuf(Rc<RefCell<Vec<u8>>>);

impl SharedBuf {
    pub fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.borrow()).into_owned()
    }
}

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

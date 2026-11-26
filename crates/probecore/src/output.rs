//! Scoped CLI output for a thread that executes commands inside a terminal UI.
//! Ordinary CLI callers keep the standard print macros and byte streams.

use std::cell::RefCell;
use std::fmt;

pub enum Line<'a> {
    Text(&'a str),
    Error(&'a str),
    Progress(&'a str),
    Rtt(&'a [u8]),
}

type Sink = Box<dyn Fn(Line<'_>)>;

thread_local! {
    static SINK: RefCell<Option<Sink>> = RefCell::new(None);
}

/// Restore the previous sink even if the command unwinds.
pub fn with_sink<T>(sink: impl Fn(Line<'_>) + 'static, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<Sink>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SINK.replace(self.0.take());
        }
    }

    let _restore = Restore(SINK.replace(Some(Box::new(sink))));
    run()
}

pub fn captured() -> bool {
    SINK.with(|sink| sink.borrow().is_some())
}

/// Returns whether the active sink consumed the output.
pub fn send(line: Line<'_>) -> bool {
    SINK.with(|sink| {
        if let Some(sink) = sink.borrow().as_ref() {
            sink(line);
            true
        } else {
            false
        }
    })
}

#[doc(hidden)]
pub fn print(stderr: bool, args: fmt::Arguments<'_>) {
    if captured() {
        let text = args.to_string();
        send(if stderr {
            Line::Error(&text)
        } else {
            Line::Text(&text)
        });
    } else if stderr {
        std::eprintln!("{args}");
    } else {
        std::println!("{args}");
    }
}

#[macro_export]
macro_rules! output_line {
    () => {
        $crate::output::print(false, format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::output::print(false, format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! output_error {
    ($($arg:tt)*) => {
        $crate::output::print(true, format_args!($($arg)*))
    };
}

pub use crate::{output_error as eprintln, output_line as println};

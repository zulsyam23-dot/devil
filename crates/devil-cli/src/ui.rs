//! Small helpers for terminal output.

use owo_colors::OwoColorize;
use std::io::IsTerminal;

pub fn color_enabled() -> bool {
    std::io::stdout().is_terminal() && std::env::var("NO_COLOR").is_err()
}

pub fn bold(text: &str) -> String {
    if color_enabled() {
        text.bold().to_string()
    } else {
        text.to_string()
    }
}

pub fn green(text: &str) -> String {
    if color_enabled() {
        text.green().to_string()
    } else {
        text.to_string()
    }
}

pub fn yellow(text: &str) -> String {
    if color_enabled() {
        text.yellow().to_string()
    } else {
        text.to_string()
    }
}

pub fn red(text: &str) -> String {
    if color_enabled() {
        text.red().to_string()
    } else {
        text.to_string()
    }
}

pub fn dim(text: &str) -> String {
    if color_enabled() {
        text.dimmed().to_string()
    } else {
        text.to_string()
    }
}

/// A `status  message` line, cargo-style.
pub fn status(verb: impl std::fmt::Display, message: impl std::fmt::Display) {
    println!("{verb} {message}");
}

pub fn ok(message: impl std::fmt::Display) {
    status(&green("done"), message);
}

pub fn warn(message: impl std::fmt::Display) {
    println!("{} {message}", yellow("warning:"));
}

pub fn error(message: impl std::fmt::Display) {
    eprintln!("{} {message}", red("error:"));
}

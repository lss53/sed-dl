// src/symbols.rs

use colored::{ColoredString, Colorize};
use std::sync::LazyLock;

pub static OK: LazyLock<ColoredString> = LazyLock::new(|| "[OK]".green());
pub static ERROR: LazyLock<ColoredString> = LazyLock::new(|| "[X]".red());
pub static INFO: LazyLock<ColoredString> = LazyLock::new(|| "[i]".cyan());
pub static WARN: LazyLock<ColoredString> = LazyLock::new(|| "[!]".yellow());
pub static CTRL_C: LazyLock<ColoredString> = LazyLock::new(|| "Ctrl+C".yellow());

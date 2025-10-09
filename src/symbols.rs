// src/symbols.rs

use std::sync::LazyLock;
use colored::{Colorize, ColoredString};

pub static OK: LazyLock<ColoredString> = LazyLock::new(|| "[OK]".green());
pub static ERROR: LazyLock<ColoredString> = LazyLock::new(|| "[X]".red());
pub static INFO: LazyLock<ColoredString> = LazyLock::new(|| "[i]".cyan());
pub static WARN: LazyLock<ColoredString> = LazyLock::new(|| "[!]".yellow());
pub static CTRL_C: LazyLock<ColoredString> = LazyLock::new(|| "Ctrl+C".yellow());
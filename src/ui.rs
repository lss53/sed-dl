// src/ui.rs

use crate::{constants, symbols};
use colored::*;
use std::io::{self, Write};

pub fn print_header(title: &str) {
    println!("\n{}", "═".repeat(constants::UI_WIDTH));
    println!(" {}", title.cyan().bold());
    println!("{}", "═".repeat(constants::UI_WIDTH));
}

pub fn print_sub_header(title: &str) {
    println!("\n--- {} ---", title.bold());
}

pub fn box_message(title: &str, content: &[&str], color_func: fn(ColoredString) -> ColoredString) {
    println!("\n┌{}┐", "─".repeat(constants::UI_WIDTH - 2));
    println!("  {}", color_func(title.bold()));
    println!("├{}┤", "─".repeat(constants::UI_WIDTH - 2));
    for line in content {
        println!("  {}", line);
    }
    println!("└{}┘", "─".repeat(constants::UI_WIDTH - 2));
}

pub fn prompt(message: &str, default: Option<&str>) -> io::Result<String> {
    let default_str = default.map_or("".to_string(), |d| format!(" (默认: {})", d));
    print!("\n>>> {}{}: ", message, default_str);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim().to_string();
    if input.is_empty() {
        Ok(default.unwrap_or("").to_string())
    } else {
        Ok(input)
    }
}

pub fn confirm(question: &str, default_yes: bool) -> bool {
    let options = if default_yes { "(Y/n)" } else { "(y/N)" };
    loop {
        match prompt(
            &format!("{} {} (按 {} 取消)", question, options, *symbols::CTRL_C),
            None,
        ) {
            Ok(choice) => {
                let choice = choice.to_lowercase();
                if choice == "y" {
                    return true;
                }
                if choice == "n" {
                    return false;
                }
                if choice.is_empty() {
                    return default_yes;
                }
                println!("{}", "无效输入，请输入 'y' 或 'n'。".red());
            }
            Err(_) => return false,
        }
    }
}

pub fn selection_menu(
    options: &[String],
    title: &str,
    instructions: &str,
    default_choice: &str,
) -> String {
    println!("\n┌{}┐", "─".repeat(constants::UI_WIDTH - 2));
    println!("  {}", title.cyan().bold());
    println!("├{}┤", "─".repeat(constants::UI_WIDTH - 2));

    let pad = options.len().to_string().len();
    for (i, option) in options.iter().enumerate() {
        println!(
            "  [{}] {}",
            format!("{:<pad$}", i + 1, pad = pad).yellow(),
            option
        );
    }

    println!("├{}┤", "─".repeat(constants::UI_WIDTH - 2));
    println!("  {} (按 {} 可取消)", instructions, *symbols::CTRL_C);
    println!("└{}┘", "─".repeat(constants::UI_WIDTH - 2));

    prompt("请输入你的选择", Some(default_choice)).unwrap_or_default()
}

pub fn prompt_hidden(message: &str) -> io::Result<String> {
    print!("\n>>> {}: ", message);
    io::stdout().flush()?;
    rpassword::read_password()
}

pub fn get_user_choices_from_menu(
    options: &[String],
    title: &str,
    default_choice: &str,
) -> Vec<String> {
    if options.is_empty() {
        return vec![];
    }
    let user_input = selection_menu(options, title, "支持格式: 1, 3, 2-4, all", default_choice);
    crate::utils::parse_selection_indices(&user_input, options.len())
        .into_iter()
        .map(|i| options[i].clone())
        .collect()
}

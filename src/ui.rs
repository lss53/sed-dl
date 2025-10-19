// src/ui.rs

use crate::{constants, error::AppResult, symbols};
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

/// 打印一条普通信息，带 [i] 符号
pub fn info(message: &str) {
    println!("{} {}", *symbols::INFO, message);
}

/// 打印一条成功信息，带 [OK] 符号
pub fn success(message: &str) {
    println!("{} {}", *symbols::OK, message);
}

/// 打印一条警告信息，带 [!] 符号，内容为黄色
pub fn warn(message: &str) {
    println!("{} {}", *symbols::WARN, message.yellow());
}

/// 打印一条错误信息到 stderr，带 [X] 符号，内容为红色
pub fn error(message: &str) {
    eprintln!("{} {}", *symbols::ERROR, message.red());
}

/// 打印不带任何符号的普通文本
pub fn plain(message: &str) {
    println!("{}", message);
}

pub fn prompt(message: &str, default: Option<&str>) -> io::Result<String> {
    print!("\n>>> {}{}: ", message, default.map_or("".to_string(), |d| format!(" (默认: {})", d)));
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
                error("无效输入，请输入 'y' 或 'n'。");
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
) -> AppResult<String> {
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

    prompt("请输入你的选择", Some(default_choice)).map_err(|_| crate::error::AppError::UserInterrupt)
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
) -> AppResult<Vec<String>> {
    if options.is_empty() {
        return Ok(vec![]);
    }
    let user_input = selection_menu(
        options,
        title,
        "支持格式: 1, 3, 2-4, all",
        default_choice,
    )?; // <--- 在这里使用 '?'
    
    let selected_items = crate::utils::parse_selection_indices(&user_input, options.len()) // 现在 user_input 是 String 类型
        .into_iter()
        .map(|i| options[i].clone())
        .collect();
        
    Ok(selected_items) // <--- 将最终结果包装在 Ok() 中
}

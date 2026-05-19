use clap::Parser;
use colored::*;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    path: Option<String>,
}

fn main() {
    let args = Args::parse();
    
    let path_str = if let Some(p) = args.path {
        p
    } else {
        print!("{}", "Введите путь для поиска дубликатов: ".yellow());
        io::stdout().flush().unwrap();
        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();
        
        let trimmed = input.trim();
        if trimmed.starts_with('"') && trimmed.ends_with('"') {
            trimmed[1..trimmed.len()-1].to_string()
        } else {
            trimmed.to_string()
        }
    };

    let root_path = Path::new(&path_str);

    if !root_path.exists() || !root_path.is_dir() {
        eprintln!("{}", "Ошибка: Указанный путь не существует или не является директорией!".red());
        wait_for_exit();
        return;
    }

    println!("{}", "Поиск файлов...".cyan());

    let mut size_map: HashMap<u64, Vec<PathBuf>> = HashMap::new();

    for entry in WalkDir::new(root_path).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() {
            if let Ok(metadata) = fs::metadata(path) {
                let size = metadata.len();
                if size > 0 {
                    size_map.entry(size).or_default().push(path.to_path_buf());
                }
            }
        }
    }

    size_map.retain(|_, paths| paths.len() > 1);

    if size_map.is_empty() {
        println!("{}", "Дубликатов не найдено!".green());
        wait_for_exit();
        return;
    }

    println!("{}", "Вычисление хешей для потенциальных дубликатов...".cyan());

    let mut hash_map: HashMap<String, Vec<PathBuf>> = HashMap::new();

    for (_, paths) in size_map {
        for path in paths {
            if let Ok(hash) = calculate_hash(&path) {
                hash_map.entry(hash).or_default().push(path);
            }
        }
    }

    hash_map.retain(|_, paths| paths.len() > 1);

    if hash_map.is_empty() {
        println!("{}", "Дубликатов не найдено!".green());
        wait_for_exit();
        return;
    }

    // Сортируем группы для стабильного отображения
    let mut sorted_groups: Vec<(String, Vec<PathBuf>)> = hash_map.into_iter().collect();
    sorted_groups.sort_by(|a, b| a.1[0].cmp(&b.1[0]));

    println!("{}", format!("Найдено {} групп дубликатов.\n", sorted_groups.len()).yellow().bold());

    // ВЫВОДИМ ВСЕ ГРУППЫ ОДНОВРЕМЕННО
    for (group_idx, (hash, paths)) in sorted_groups.iter().enumerate() {
        println!("{}", format!("--- Группа дубликатов {} ---", group_idx + 1).magenta().bold());
        println!("Хеш: {}", hash.bright_black());
        
        for (i, path) in paths.iter().enumerate() {
            println!("  [{}] {}", (i + 1).to_string().cyan(), path.display());
        }
        println!();
    }

    // МЕНЮ ВЫБОРА ДЕЙСТВИЯ
    loop {
        println!("{}", "Выберите действие:".yellow());
        println!("  {} - Оставить все (выйти)", "0".green());
        println!("  {} - Удалить ВСЕ дубликаты во ВСЕХ группах", "d".red());
        println!("  {} - Оставить ПЕРВЫЙ файл в каждой группе (остальные удалить автоматически)", "1".cyan());
        println!("  {} - Пройти по каждой группе по отдельности", "i".magenta());
        
        print!("> ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();
        let input = input.trim();

        if input == "0" {
            println!("{}", "-> Файлы оставлены.".green());
            break;
        } else if input.eq_ignore_ascii_case("d") {
            for (_, paths) in &sorted_groups {
                for path in paths {
                    if let Err(e) = fs::remove_file(path) {
                        eprintln!("{} {}: {}", "Ошибка удаления".red(), path.display(), e);
                    } else {
                        println!("{} {}", "Удален:".red(), path.display());
                    }
                }
            }
            break;
        } else if input == "1" {
            for (_, paths) in &sorted_groups {
                for (i, path) in paths.iter().enumerate() {
                    if i == 0 {
                        println!("{} {}", "Оставлен:".green(), path.display());
                    } else {
                        if let Err(e) = fs::remove_file(path) {
                            eprintln!("{} {}: {}", "Ошибка удаления".red(), path.display(), e);
                        } else {
                            println!("{} {}", "Удален:".red(), path.display());
                        }
                    }
                }
            }
            break;
        } else if input.eq_ignore_ascii_case("i") {
            for (group_idx, (_, paths)) in sorted_groups.iter().enumerate() {
                println!("\n{}", format!("--- Работа с группой {} ---", group_idx + 1).magenta().bold());
                for (i, path) in paths.iter().enumerate() {
                    println!("  [{}] {}", (i + 1).to_string().cyan(), path.display());
                }

                loop {
                    println!("\n{}", "Выберите действие для ЭТОЙ группы:".yellow());
                    println!("  {} - Оставить все", "0".green());
                    println!("  {} - Удалить все дубликаты в этой группе", "d".red());
                    println!("  {} - Оставить ОДИН файл (введите номер от 1 до {})", "<номер>".cyan(), paths.len());
                    
                    print!("> ");
                    io::stdout().flush().unwrap();

                    let mut sub_input = String::new();
                    io::stdin().read_line(&mut sub_input).unwrap();
                    let sub_input = sub_input.trim();

                    if sub_input == "0" {
                        println!("{}", "-> Файлы оставлены.".green());
                        break;
                    } else if sub_input.eq_ignore_ascii_case("d") {
                        for path in paths {
                            if let Err(e) = fs::remove_file(path) {
                                eprintln!("{} {}: {}", "Ошибка удаления".red(), path.display(), e);
                            } else {
                                println!("{} {}", "Удален:".red(), path.display());
                            }
                        }
                        break;
                    } else if let Ok(idx) = sub_input.parse::<usize>() {
                        if idx >= 1 && idx <= paths.len() {
                            for (i, path) in paths.iter().enumerate() {
                                if i + 1 != idx {
                                    if let Err(e) = fs::remove_file(path) {
                                        eprintln!("{} {}: {}", "Ошибка удаления".red(), path.display(), e);
                                    } else {
                                        println!("{} {}", "Удален:".red(), path.display());
                                    }
                                } else {
                                    println!("{} {}", "Оставлен:".green(), path.display());
                                }
                            }
                            break;
                        } else {
                            println!("{}", "Неверный номер файла. Попробуйте еще раз.".red());
                        }
                    } else {
                        println!("{}", "Неверный ввод. Попробуйте еще раз.".red());
                    }
                }
            }
            break;
        } else {
            println!("{}", "Неверный ввод. Попробуйте еще раз.".red());
        }
    }

    println!("\n{}", "Поиск и очистка завершены!".green().bold());
    wait_for_exit();
}

fn calculate_hash(path: &Path) -> Result<String, io::Error> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    let result = hasher.finalize();
    Ok(format!("{:x}", result))
}

fn wait_for_exit() {
    println!("\n{}", "Нажмите Enter для выхода...".bright_black());
    let mut dummy = String::new();
    let _ = io::stdin().read_line(&mut dummy);
}

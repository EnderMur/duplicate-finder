use clap::Parser;
use colored::*;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Интерактивный поиск дубликатов файлов по SHA-256",
    long_about = "Ищет дубликаты файлов по размеру и SHA-256 хешу, а затем позволяет удалить лишние копии или переместить их в отдельную папку.",
    after_help = "Примеры:\n  duplicate-finder --path \"C:/Users/User/Documents\"\n  duplicate-finder --path . --move-to .duplicates\n\nЕсли --move-to не указан, дубликаты при переносе попадут в папку \".duplicates\" внутри выбранного пути."
)]
struct Args {
    #[arg(
        short,
        long,
        value_name = "DIR",
        help = "Папка для поиска дубликатов"
    )]
    path: Option<String>,
    #[arg(
        long,
        value_name = "DIR",
        help = "Куда перемещать дубликаты вместо удаления"
    )]
    move_to: Option<PathBuf>,
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
    let move_target = args
        .move_to
        .unwrap_or_else(|| root_path.join(".duplicates"));

    if !root_path.exists() || !root_path.is_dir() {
        eprintln!("{}", "Ошибка: Указанный путь не существует или не является директорией!".red());
        wait_for_exit();
        return;
    }

    println!("{}", "Поиск файлов...".cyan());
    let scan_spinner = ProgressBar::new_spinner();
    scan_spinner.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(&["⠁", "⠂", "⠄", "⠂"]),
    );
    scan_spinner.set_message("Сканирование директорий");
    scan_spinner.enable_steady_tick(std::time::Duration::from_millis(100));

    let mut size_map: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    let mut scanned_files = 0u64;

    for entry in WalkDir::new(root_path).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.starts_with(&move_target) {
            continue;
        }

        if path.is_file() {
            if let Ok(metadata) = fs::metadata(path) {
                let size = metadata.len();
                if size > 0 {
                    size_map.entry(size).or_default().push(path.to_path_buf());
                }
                scanned_files += 1;
                if scanned_files.is_multiple_of(200) {
                    scan_spinner.set_message(format!("Сканирование директорий: {} файлов", scanned_files));
                }
            }
        }
    }
    scan_spinner.finish_with_message(format!("Сканирование завершено: {} файлов", scanned_files));

    size_map.retain(|_, paths| paths.len() > 1);

    if size_map.is_empty() {
        println!("{}", "Дубликатов не найдено!".green());
        wait_for_exit();
        return;
    }

    println!("{}", "Вычисление хешей для потенциальных дубликатов...".cyan());

    let mut hash_map: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let total_candidates: u64 = size_map.values().map(|paths| paths.len() as u64).sum();
    let hash_progress = ProgressBar::new(total_candidates);
    hash_progress.set_style(
        ProgressStyle::with_template("{bar:40.cyan/blue} {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("##-"),
    );
    hash_progress.set_message("Хеширование файлов");

    for (_, paths) in size_map {
        for path in paths {
            if let Ok(hash) = calculate_hash(&path) {
                hash_map.entry(hash).or_default().push(path);
            }
            hash_progress.inc(1);
        }
    }
    hash_progress.finish_with_message("Хеширование завершено");

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
        println!("  {} - Удалить дубликаты во ВСЕХ группах (первый файл сохранить)", "d".red());
        println!(
            "  {} - Переместить дубликаты во ВСЕХ группах в {}",
            "m".blue(),
            move_target.display()
        );
        println!("  {} - Оставить ПЕРВЫЙ файл в каждой группе, остальные удалить", "1".cyan());
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
            process_groups(&sorted_groups, Operation::Delete, None, &move_target);
            break;
        } else if input.eq_ignore_ascii_case("m") {
            process_groups(&sorted_groups, Operation::Move, None, &move_target);
            break;
        } else if input == "1" {
            process_groups(&sorted_groups, Operation::Delete, None, &move_target);
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
                    println!("  {} - Удалить дубликаты в этой группе (первый файл сохранить)", "d".red());
                    println!(
                        "  {} - Переместить дубликаты в этой группе в {}",
                        "m".blue(),
                        move_target.display()
                    );
                    println!(
                        "  {} - Оставить ОДИН файл (введите номер от 1 до {}), остальные удалить",
                        "<номер>".cyan(),
                        paths.len()
                    );
                    println!(
                        "  {} - Переместить все кроме одного файла (формат: m2, m3, ...)",
                        "<mN>".blue()
                    );
                    
                    print!("> ");
                    io::stdout().flush().unwrap();

                    let mut sub_input = String::new();
                    io::stdin().read_line(&mut sub_input).unwrap();
                    let sub_input = sub_input.trim();

                    if sub_input == "0" {
                        println!("{}", "-> Файлы оставлены.".green());
                        break;
                    } else if sub_input.eq_ignore_ascii_case("d") {
                        process_single_group(paths, Operation::Delete, 0, &move_target);
                        break;
                    } else if sub_input.eq_ignore_ascii_case("m") {
                        process_single_group(paths, Operation::Move, 0, &move_target);
                        break;
                    } else if let Ok(idx) = sub_input.parse::<usize>() {
                        if idx >= 1 && idx <= paths.len() {
                            process_single_group(paths, Operation::Delete, idx - 1, &move_target);
                            break;
                        } else {
                            println!("{}", "Неверный номер файла. Попробуйте еще раз.".red());
                        }
                    } else if let Some(idx) = parse_move_selection(sub_input, paths.len()) {
                        if idx < paths.len() {
                            process_single_group(paths, Operation::Move, idx, &move_target);
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
    if !io::stdin().is_terminal() {
        return;
    }

    println!("\n{}", "Нажмите Enter для выхода...".bright_black());
    let mut dummy = String::new();
    let _ = io::stdin().read_line(&mut dummy);
}

#[derive(Clone, Copy)]
enum Operation {
    Delete,
    Move,
}

fn process_groups(
    groups: &[(String, Vec<PathBuf>)],
    operation: Operation,
    keep_index: Option<usize>,
    move_target: &Path,
) {
    let progress = ProgressBar::new(groups.len() as u64);
    progress.set_style(
        ProgressStyle::with_template("{bar:40.magenta/blue} {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("##-"),
    );
    progress.set_message("Обработка групп");

    for (_, paths) in groups {
        process_single_group(paths, operation, keep_index.unwrap_or(0), move_target);
        progress.inc(1);
    }

    progress.finish_with_message("Обработка завершена");
}

fn process_single_group(
    paths: &[PathBuf],
    operation: Operation,
    keep_index: usize,
    move_target: &Path,
) {
    for (i, path) in paths.iter().enumerate() {
        if i == keep_index {
            println!("{} {}", "Оставлен:".green(), path.display());
            continue;
        }

        match operation {
            Operation::Delete => {
                if let Err(e) = fs::remove_file(path) {
                    eprintln!("{} {}: {}", "Ошибка удаления".red(), path.display(), e);
                } else {
                    println!("{} {}", "Удален:".red(), path.display());
                }
            }
            Operation::Move => match move_duplicate(path, move_target) {
                Ok(new_path) => println!(
                    "{} {} {}",
                    "Перемещен:".blue(),
                    path.display(),
                    format!("-> {}", new_path.display()).bright_black()
                ),
                Err(e) => eprintln!("{} {}: {}", "Ошибка перемещения".red(), path.display(), e),
            },
        }
    }
}

fn move_duplicate(path: &Path, move_target: &Path) -> Result<PathBuf, io::Error> {
    fs::create_dir_all(move_target)?;

    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Путь не содержит имени файла"))?;
    let target_path = unique_destination(move_target, file_name);
    fs::rename(path, &target_path)?;
    Ok(target_path)
}

fn unique_destination(dir: &Path, file_name: &std::ffi::OsStr) -> PathBuf {
    let candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }

    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("duplicate");
    let ext = Path::new(file_name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    for idx in 1.. {
        let new_name = if ext.is_empty() {
            format!("{stem}_{idx}")
        } else {
            format!("{stem}_{idx}.{ext}")
        };
        let new_path = dir.join(new_name);
        if !new_path.exists() {
            return new_path;
        }
    }

    unreachable!()
}

fn parse_move_selection(input: &str, len: usize) -> Option<usize> {
    if input.len() < 2 || !input.starts_with('m') {
        return None;
    }

    let idx = input[1..].parse::<usize>().ok()?;
    if idx == 0 || idx > len {
        return None;
    }

    Some(idx - 1)
}

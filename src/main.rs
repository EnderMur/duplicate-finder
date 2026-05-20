use clap::Parser;
use colored::*;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

const QUICK_HASH_BYTES: usize = 128 * 1024;
const FULL_HASH_BUFFER_BYTES: usize = 1024 * 1024;
const CACHE_FILE_NAME: &str = ".duplicate-cache";

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Интерактивный поиск дубликатов файлов по двухэтапному хешированию",
    long_about = "Ищет дубликаты по размеру, быстрому хешу первых 128 KiB и подтверждает совпадения полным SHA-256. Хеширование выполняется параллельно, а результаты кэшируются в .duplicate-cache рядом с приложением.",
    after_help = "Примеры:\n  duplicate-finder --path \"C:/Users/User/Documents\"\n  duplicate-finder --path . --move-to .duplicates --threads 8 --memory-limit 512MB\n\nЕсли --move-to не указан, дубликаты при переносе попадут в папку \".duplicates\" внутри выбранного пути."
)]
struct Args {
    #[arg(short, long, value_name = "DIR", help = "Папка для поиска дубликатов")]
    path: Option<String>,
    #[arg(long, value_name = "DIR", help = "Куда перемещать дубликаты вместо удаления")]
    move_to: Option<PathBuf>,
    #[arg(long, value_name = "N", help = "Количество потоков для параллельного хеширования")]
    threads: Option<usize>,
    #[arg(
        long,
        value_name = "SIZE",
        default_value = "256MB",
        help = "Лимит памяти для хеширования, например 256MB, 1GB, 131072KB"
    )]
    memory_limit: String,
}

#[derive(Clone)]
struct FileEntry {
    path: PathBuf,
    size: u64,
    modified_unix_secs: u64,
}

#[derive(Clone, Default)]
struct CacheEntry {
    size: u64,
    modified_unix_secs: u64,
    quick_hash: Option<String>,
    full_hash: Option<String>,
}

fn main() {
    let args = Args::parse();
    let memory_limit_bytes = match parse_memory_limit(&args.memory_limit) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{} {}", "Ошибка:".red(), error);
            wait_for_exit();
            return;
        }
    };

    let requested_threads = args.threads.unwrap_or_else(available_threads);
    let max_threads_by_memory =
        std::cmp::max(1, (memory_limit_bytes / FULL_HASH_BUFFER_BYTES as u64) as usize);
    let worker_threads = std::cmp::max(1, requested_threads.min(max_threads_by_memory));

    if let Err(error) = ThreadPoolBuilder::new()
        .num_threads(worker_threads)
        .build_global()
    {
        eprintln!("{} {}", "Ошибка настройки пула потоков:".red(), error);
        wait_for_exit();
        return;
    }

    let path_str = if let Some(p) = args.path {
        p
    } else {
        print!("{}", "Введите путь для поиска дубликатов: ".yellow());
        io::stdout().flush().unwrap();
        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();

        let trimmed = input.trim();
        if trimmed.starts_with('"') && trimmed.ends_with('"') {
            trimmed[1..trimmed.len() - 1].to_string()
        } else {
            trimmed.to_string()
        }
    };

    let root_path = Path::new(&path_str);
    let move_target = args.move_to.unwrap_or_else(|| root_path.join(".duplicates"));

    if !root_path.exists() || !root_path.is_dir() {
        eprintln!(
            "{}",
            "Ошибка: Указанный путь не существует или не является директорией!".red()
        );
        wait_for_exit();
        return;
    }

    let cache_path = match cache_file_path() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{} {}", "Ошибка определения пути кэша:".red(), error);
            wait_for_exit();
            return;
        }
    };
    let mut cache = load_cache(&cache_path);

    println!(
        "{}",
        format!(
            "Сканирование и хеширование: {} поток(ов), лимит памяти {} байт",
            worker_threads, memory_limit_bytes
        )
        .bright_black()
    );
    println!("{}", "Поиск файлов...".cyan());
    let scan_spinner = ProgressBar::new_spinner();
    scan_spinner.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(&["⠁", "⠂", "⠄", "⠂"]),
    );
    scan_spinner.set_message("Сканирование директорий");
    scan_spinner.enable_steady_tick(std::time::Duration::from_millis(100));

    let mut size_map: HashMap<u64, Vec<FileEntry>> = HashMap::new();
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
                    let modified_unix_secs = metadata
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                        .map(|duration| duration.as_secs())
                        .unwrap_or(0);

                    size_map.entry(size).or_default().push(FileEntry {
                        path: path.to_path_buf(),
                        size,
                        modified_unix_secs,
                    });
                }
                scanned_files += 1;
                if scanned_files.is_multiple_of(200) {
                    scan_spinner
                        .set_message(format!("Сканирование директорий: {} файлов", scanned_files));
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

    println!("{}", "Быстрое хеширование кандидатов...".cyan());
    let quick_candidates: Vec<FileEntry> = size_map
        .values()
        .flat_map(|paths| paths.iter().cloned())
        .collect();

    let quick_progress = ProgressBar::new(quick_candidates.len() as u64);
    quick_progress.set_style(
        ProgressStyle::with_template("{bar:40.cyan/blue} {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("##-"),
    );
    quick_progress.set_message("Первые 128 KiB");

    let quick_results: Vec<(FileEntry, Option<String>)> = quick_candidates
        .par_iter()
        .map(|entry| {
            let quick_hash = cached_or_compute_quick_hash(entry, &cache);
            quick_progress.inc(1);
            (entry.clone(), quick_hash)
        })
        .collect();
    quick_progress.finish_with_message("Быстрое хеширование завершено");

    let mut quick_map: HashMap<(u64, String), Vec<FileEntry>> = HashMap::new();
    for (entry, quick_hash) in quick_results {
        if let Some(hash) = quick_hash {
            cache.insert(
                normalize_path_key(&entry.path),
                CacheEntry {
                    size: entry.size,
                    modified_unix_secs: entry.modified_unix_secs,
                    quick_hash: Some(hash.clone()),
                    full_hash: cache
                        .get(&normalize_path_key(&entry.path))
                        .and_then(|cached| cached.full_hash.clone()),
                },
            );
            quick_map.entry((entry.size, hash)).or_default().push(entry);
        }
    }
    quick_map.retain(|_, paths| paths.len() > 1);

    if quick_map.is_empty() {
        println!("{}", "Дубликатов не найдено!".green());
        save_cache(&cache_path, &cache);
        wait_for_exit();
        return;
    }

    println!("{}", "Подтверждение полным SHA-256...".cyan());
    let full_candidates: Vec<FileEntry> = quick_map
        .values()
        .flat_map(|paths| paths.iter().cloned())
        .collect();

    let full_progress = ProgressBar::new(full_candidates.len() as u64);
    full_progress.set_style(
        ProgressStyle::with_template("{bar:40.cyan/blue} {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("##-"),
    );
    full_progress.set_message("Полный SHA-256");

    let full_results: Vec<(FileEntry, Option<String>)> = full_candidates
        .par_iter()
        .map(|entry| {
            let full_hash = cached_or_compute_full_hash(entry, &cache);
            full_progress.inc(1);
            (entry.clone(), full_hash)
        })
        .collect();
    full_progress.finish_with_message("Полный SHA-256 завершен");

    let mut hash_map: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for (entry, full_hash) in full_results {
        if let Some(hash) = full_hash {
            let path_key = normalize_path_key(&entry.path);
            let quick_hash = cache.get(&path_key).and_then(|cached| cached.quick_hash.clone());
            cache.insert(
                path_key,
                CacheEntry {
                    size: entry.size,
                    modified_unix_secs: entry.modified_unix_secs,
                    quick_hash,
                    full_hash: Some(hash.clone()),
                },
            );
            hash_map.entry(hash).or_default().push(entry.path);
        }
    }
    hash_map.retain(|_, paths| paths.len() > 1);
    save_cache(&cache_path, &cache);

    if hash_map.is_empty() {
        println!("{}", "Дубликатов не найдено!".green());
        wait_for_exit();
        return;
    }

    let mut sorted_groups: Vec<(String, Vec<PathBuf>)> = hash_map.into_iter().collect();
    sorted_groups.sort_by(|a, b| a.1[0].cmp(&b.1[0]));

    println!(
        "{}",
        format!("Найдено {} групп дубликатов.\n", sorted_groups.len())
            .yellow()
            .bold()
    );

    for (group_idx, (hash, paths)) in sorted_groups.iter().enumerate() {
        println!(
            "{}",
            format!("--- Группа дубликатов {} ---", group_idx + 1)
                .magenta()
                .bold()
        );
        println!("Хеш: {}", hash.bright_black());

        for (i, path) in paths.iter().enumerate() {
            println!("  [{}] {}", (i + 1).to_string().cyan(), path.display());
        }
        println!();
    }

    loop {
        println!("{}", "Выберите действие:".yellow());
        println!("  {} - Оставить все (выйти)", "0".green());
        println!(
            "  {} - Удалить ВСЕ файлы во ВСЕХ группах",
            "d".red()
        );
        println!(
            "  {} - Переместить дубликаты во ВСЕХ группах в {}",
            "m".blue(),
            move_target.display()
        );
        println!(
            "  {} - Оставить ПЕРВЫЙ файл в каждой группе, остальные удалить",
            "1".cyan()
        );
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
            process_groups(&sorted_groups, Operation::Delete, None, false, &move_target);
            break;
        } else if input.eq_ignore_ascii_case("m") {
            process_groups(&sorted_groups, Operation::Move, Some(0), true, &move_target);
            break;
        } else if input == "1" {
            process_groups(&sorted_groups, Operation::Delete, Some(0), true, &move_target);
            break;
        } else if input.eq_ignore_ascii_case("i") {
            for (group_idx, (_, paths)) in sorted_groups.iter().enumerate() {
                println!(
                    "\n{}",
                    format!("--- Работа с группой {} ---", group_idx + 1)
                        .magenta()
                        .bold()
                );
                for (i, path) in paths.iter().enumerate() {
                    println!("  [{}] {}", (i + 1).to_string().cyan(), path.display());
                }

                loop {
                    println!("\n{}", "Выберите действие для ЭТОЙ группы:".yellow());
                    println!("  {} - Оставить все", "0".green());
                    println!(
                        "  {} - Удалить ВСЕ файлы в этой группе",
                        "d".red()
                    );
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
                        process_single_group(paths, Operation::Delete, None, false, &move_target);
                        break;
                    } else if sub_input.eq_ignore_ascii_case("m") {
                        process_single_group(paths, Operation::Move, Some(0), true, &move_target);
                        break;
                    } else if let Ok(idx) = sub_input.parse::<usize>() {
                        if idx >= 1 && idx <= paths.len() {
                            process_single_group(
                                paths,
                                Operation::Delete,
                                Some(idx - 1),
                                true,
                                &move_target,
                            );
                            break;
                        } else {
                            println!("{}", "Неверный номер файла. Попробуйте еще раз.".red());
                        }
                    } else if let Some(idx) = parse_move_selection(sub_input, paths.len()) {
                        if idx < paths.len() {
                            process_single_group(
                                paths,
                                Operation::Move,
                                Some(idx),
                                true,
                                &move_target,
                            );
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

fn available_threads() -> usize {
    std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1)
}

fn parse_memory_limit(input: &str) -> Result<u64, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("пустое значение для --memory-limit".to_string());
    }

    let split_at = trimmed
        .find(|char: char| !char.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (number_part, unit_part) = trimmed.split_at(split_at);

    if number_part.is_empty() {
        return Err(format!("не удалось прочитать размер памяти: {trimmed}"));
    }

    let number = number_part
        .parse::<u64>()
        .map_err(|_| format!("неверное число в --memory-limit: {trimmed}"))?;
    let unit = unit_part.trim().to_ascii_lowercase();

    let multiplier = match unit.as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1024,
        "m" | "mb" => 1024 * 1024,
        "g" | "gb" => 1024 * 1024 * 1024,
        _ => return Err(format!("неподдерживаемая единица в --memory-limit: {trimmed}")),
    };

    number
        .checked_mul(multiplier)
        .ok_or_else(|| format!("слишком большое значение для --memory-limit: {trimmed}"))
}

fn cache_file_path() -> io::Result<PathBuf> {
    let exe_path = std::env::current_exe()?;
    let exe_dir = exe_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "не удалось определить директорию исполняемого файла",
        )
    })?;
    Ok(exe_dir.join(CACHE_FILE_NAME))
}

fn normalize_path_key(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn load_cache(cache_path: &Path) -> HashMap<String, CacheEntry> {
    let mut cache = HashMap::new();
    let file = match File::open(cache_path) {
        Ok(file) => file,
        Err(_) => return cache,
    };

    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 5 {
            continue;
        }

        let size = match parts[1].parse::<u64>() {
            Ok(value) => value,
            Err(_) => continue,
        };
        let modified_unix_secs = match parts[2].parse::<u64>() {
            Ok(value) => value,
            Err(_) => continue,
        };

        cache.insert(
            parts[0].to_string(),
            CacheEntry {
                size,
                modified_unix_secs,
                quick_hash: empty_to_none(parts[3]),
                full_hash: empty_to_none(parts[4]),
            },
        );
    }

    cache
}

fn save_cache(cache_path: &Path, cache: &HashMap<String, CacheEntry>) {
    let mut lines: Vec<String> = cache
        .iter()
        .map(|(path, entry)| {
            format!(
                "{}\t{}\t{}\t{}\t{}",
                path,
                entry.size,
                entry.modified_unix_secs,
                entry.quick_hash.as_deref().unwrap_or(""),
                entry.full_hash.as_deref().unwrap_or("")
            )
        })
        .collect();
    lines.sort();

    if let Ok(mut file) = File::create(cache_path) {
        for line in lines {
            let _ = writeln!(file, "{line}");
        }
    }
}

fn empty_to_none(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn cached_or_compute_quick_hash(
    entry: &FileEntry,
    cache: &HashMap<String, CacheEntry>,
) -> Option<String> {
    let path_key = normalize_path_key(&entry.path);
    if let Some(cached) = cache.get(&path_key) {
        if cached.size == entry.size && cached.modified_unix_secs == entry.modified_unix_secs {
            if let Some(hash) = &cached.quick_hash {
                return Some(hash.clone());
            }
        }
    }

    match calculate_partial_hash(&entry.path, QUICK_HASH_BYTES) {
        Ok(hash) => Some(hash),
        Err(error) => {
            eprintln!("{} {}: {}", "Ошибка быстрого хеширования".red(), entry.path.display(), error);
            None
        }
    }
}

fn cached_or_compute_full_hash(
    entry: &FileEntry,
    cache: &HashMap<String, CacheEntry>,
) -> Option<String> {
    let path_key = normalize_path_key(&entry.path);
    if let Some(cached) = cache.get(&path_key) {
        if cached.size == entry.size && cached.modified_unix_secs == entry.modified_unix_secs {
            if let Some(hash) = &cached.full_hash {
                return Some(hash.clone());
            }
        }
    }

    match calculate_full_hash(&entry.path) {
        Ok(hash) => Some(hash),
        Err(error) => {
            eprintln!(
                "{} {}: {}",
                "Ошибка полного хеширования".red(),
                entry.path.display(),
                error
            );
            None
        }
    }
}

fn calculate_partial_hash(path: &Path, bytes_to_read: usize) -> Result<String, io::Error> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(QUICK_HASH_BYTES, file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; QUICK_HASH_BYTES.min(bytes_to_read)];
    let mut remaining = bytes_to_read;

    while remaining > 0 {
        let to_read = remaining.min(buffer.len());
        let count = reader.read(&mut buffer[..to_read])?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        remaining -= count;
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn calculate_full_hash(path: &Path) -> Result<String, io::Error> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(FULL_HASH_BUFFER_BYTES, file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; FULL_HASH_BUFFER_BYTES];

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    Ok(format!("{:x}", hasher.finalize()))
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
    keep_selected: bool,
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
        process_single_group(paths, operation, keep_index, keep_selected, move_target);
        progress.inc(1);
    }

    progress.finish_with_message("Обработка завершена");
}

fn process_single_group(
    paths: &[PathBuf],
    operation: Operation,
    keep_index: Option<usize>,
    keep_selected: bool,
    move_target: &Path,
) {
    for (i, path) in paths.iter().enumerate() {
        if keep_selected && keep_index == Some(i) {
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

    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "Путь не содержит имени файла")
    })?;
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

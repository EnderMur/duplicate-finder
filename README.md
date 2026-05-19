# duplicate-finder
Простая CLI-утилита на Rust для поиска и удаления дубликатов файлов по SHA-256 хешу.

---
## Установка

### Клонирование репозитория

```bash
git clone https://github.com/EnderMur/duplicate-finder.git
cd duplicate-finder
```

### Сборка проекта

Убедитесь, что у вас установлен Rust и Cargo.

```bash
cargo build --release
```

Готовый бинарный файл будет находиться в:

```bash
target/release/duplicate-finder
```

---

## Использования

### Запуск с указанием пути

```bash
cargo run -- --path "C:/Users/User/Documents"
```

или

```bash
./duplicate-finder --path "/home/user/files"
```

### Запуск без аргументов

```bash
cargo run
```

После запуска программа попросит вручную ввести путь.

---

## Contributing

Pull Request'ы приветствуются.

Если вы хотите добавить новые функции или исправить ошибки:

1. Сделайте Fork репозитория
2. Создайте новую ветку
3. Внесите изменения
4. Откройте Pull Request

---

## Licence

Проект распространяется под лицензией MIT.

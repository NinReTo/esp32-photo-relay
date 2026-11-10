//! Безопасные имена для принятых файлов.
//!
//! Имя файла присылает плата (а ей — телефон), поэтому доверять ему нельзя:
//! из него убираются папки, выход наверх («..») и всё, что не годится для имени файла.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Наибольшая длина имени файла в байтах.
const MAX_NAME_BYTES: usize = 200;
/// Сколько вариантов имени пробовать, если файл уже есть.
const MAX_NAME_ATTEMPTS: u32 = 1000;
/// Знаки, запрещённые в именах файлов (слэши — всегда, остальные — в Windows).
const FORBIDDEN: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// Делает из присланного имени безопасное. Если после чистки ничего
/// осмысленного не осталось — возвращает запасное имя по дате и времени.
pub fn sanitize_name(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);

    let mut name: String = text
        .chars()
        .filter(|ch| !ch.is_control() && !FORBIDDEN.contains(ch) && !is_direction_mark(*ch))
        .collect();

    // Вырезаем «..» до тех пор, пока они находятся («....» тоже исчезнет).
    while name.contains("..") {
        name = name.replace("..", "");
    }

    // Точки и пробелы по краям имени Windows не любит.
    let mut name = name.trim_matches(|ch| ch == '.' || ch == ' ').to_string();

    // Обрезаем по границе символа, чтобы не разорвать букву из нескольких байт.
    while name.len() > MAX_NAME_BYTES {
        name.pop();
    }

    if name.is_empty() || is_reserved_name(&name) {
        return generated_name();
    }
    name
}

/// Невидимые знаки смены направления письма: ими можно замаскировать
/// настоящее расширение файла: имя с таким знаком читается задом наперёд.
fn is_direction_mark(ch: char) -> bool {
    matches!(ch, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Проверяет, не совпадает ли имя (до первой точки) с именем устройства Windows:
/// CON, PRN, AUX, NUL, COM1..COM9, LPT1..LPT9.
fn is_reserved_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();

    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }

    let numbered = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"));
    matches!(numbered, Some(digit) if digit.len() == 1 && ("1"..="9").contains(&digit))
}

/// Запасное имя вида `photo_20261110_200512.jpg` (время по Гринвичу).
pub fn generated_name() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    name_for_timestamp(seconds)
}

fn name_for_timestamp(unix_seconds: u64) -> String {
    let (year, month, day) = date_from_days((unix_seconds / 86_400) as i64);
    let in_day = unix_seconds % 86_400;
    format!(
        "photo_{year:04}{month:02}{day:02}_{:02}{:02}{:02}.jpg",
        in_day / 3600,
        in_day % 3600 / 60,
        in_day % 60
    )
}

/// Переводит число дней от 1 января 1970 года в год, месяц и день
/// (общеизвестный алгоритм для григорианского календаря).
fn date_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Подбирает путь в папке `dir`. Если такой файл уже есть, добавляет к имени
/// номер (`photo.jpg` → `photo_1.jpg`), чтобы не затереть старое фото.
pub fn unique_path(dir: &Path, name: &str) -> Option<PathBuf> {
    let first = dir.join(name);
    if !first.exists() {
        return Some(first);
    }

    let (base, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    };

    (1..=MAX_NAME_ATTEMPTS)
        .map(|attempt| dir.join(format!("{base}_{attempt}{extension}")))
        .find(|candidate| !candidate.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_ordinary_name() {
        assert_eq!(sanitize_name(b"IMG_0001.jpg"), "IMG_0001.jpg");
    }

    #[test]
    fn keeps_cyrillic_name() {
        assert_eq!(
            sanitize_name("фото с дачи.jpg".as_bytes()),
            "фото с дачи.jpg"
        );
    }

    #[test]
    fn strips_folders_and_parent_references() {
        assert_eq!(
            sanitize_name(b"../../..\\evil/../passwd.jpg"),
            "evilpasswd.jpg"
        );
        assert_eq!(
            sanitize_name(b"C:\\Windows\\system32\\x.jpg\r"),
            "CWindowssystem32x.jpg"
        );
    }

    #[test]
    fn dots_cannot_rejoin_into_parent_reference() {
        let name = sanitize_name(b".a....b./.");
        assert!(!name.contains(".."));
        assert!(!name.contains('/'));
    }

    #[test]
    fn empty_and_dots_only_names_are_replaced() {
        for raw in [&b""[..], b"../..", b" . ", b"\\/"] {
            let name = sanitize_name(raw);
            assert!(
                name.starts_with("photo_") && name.ends_with(".jpg"),
                "{name}"
            );
        }
    }

    #[test]
    fn windows_device_names_are_replaced() {
        for raw in ["com1.jpg", "NUL", "Lpt9.txt", "con.a.b"] {
            assert!(sanitize_name(raw.as_bytes()).starts_with("photo_"), "{raw}");
        }
        assert_eq!(sanitize_name(b"console.jpg"), "console.jpg");
        assert_eq!(sanitize_name(b"com10.jpg"), "com10.jpg");
    }

    #[test]
    fn long_name_is_cut_on_character_boundary() {
        let name = sanitize_name("я".repeat(500).as_bytes());
        assert!(name.len() <= MAX_NAME_BYTES);
        assert!(name.chars().all(|ch| ch == 'я'));
    }

    #[test]
    fn direction_marks_are_removed() {
        assert_eq!(
            sanitize_name("photo\u{202E}gpj.exe".as_bytes()),
            "photogpj.exe"
        );
    }

    #[test]
    fn invalid_utf8_does_not_panic() {
        assert!(!sanitize_name(&[0xff, 0xfe, b'a', b'.', b'j']).is_empty());
    }

    #[test]
    fn timestamp_becomes_date_and_time() {
        assert_eq!(name_for_timestamp(0), "photo_19700101_000000.jpg");
        // 10 ноября 2026 года, 20:05:12 по Гринвичу
        assert_eq!(
            name_for_timestamp(1_794_341_112),
            "photo_20261110_200512.jpg"
        );
        // 29 февраля високосного года
        assert_eq!(
            name_for_timestamp(1_709_164_800),
            "photo_20240229_000000.jpg"
        );
    }

    #[test]
    fn existing_file_gets_a_number() {
        let dir = std::env::temp_dir().join(format!("photo-relay-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        assert_eq!(unique_path(&dir, "a.jpg").unwrap(), dir.join("a.jpg"));
        std::fs::write(dir.join("a.jpg"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.jpg").unwrap(), dir.join("a_1.jpg"));
        std::fs::write(dir.join("a_1.jpg"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.jpg").unwrap(), dir.join("a_2.jpg"));

        std::fs::write(dir.join("noext"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "noext").unwrap(), dir.join("noext_1"));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

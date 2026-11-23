use serde_json::Value;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::hzpak::MAX_PACKED;

pub const MANIFEST_NAME: &str = "horizon-resources.json";

#[derive(Clone, Debug)]
pub struct ScanReport {
    pub root: PathBuf,
    pub files: u64,
    pub directories: u64,
    pub total_bytes: u64,
    pub packed_bytes: u64,
    pub loose_files: u64,
    pub loose_bytes: u64,
    pub manifest_keys: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn validate_source(source: &Path) -> Result<ScanReport, String> {
    if !source.is_dir() {
        return Err(format!("Папка ресурсов не найдена: {}", source.display()));
    }

    let root = source
        .canonicalize()
        .map_err(|e| format!("Не удалось открыть папку ресурсов: {e}"))?;

    let manifest_path = root.join(MANIFEST_NAME);
    if !manifest_path.is_file() {
        return Err(format!(
            "В корне папки отсутствует {MANIFEST_NAME}. Выбери папку, внутри которой лежат manifest, assets и data."
        ));
    }

    let manifest_data = std::fs::read(&manifest_path)
        .map_err(|e| format!("Не удалось прочитать {MANIFEST_NAME}: {e}"))?;
    let manifest: Value = serde_json::from_slice(&manifest_data)
        .map_err(|e| format!("{MANIFEST_NAME} содержит некорректный JSON: {e}"))?;
    let manifest_object = manifest
        .as_object()
        .ok_or_else(|| format!("{MANIFEST_NAME} должен содержать JSON-объект в корне"))?;

    for required in ["assets", "data"] {
        if !root.join(required).is_dir() {
            return Err(format!(
                "Не найдена обязательная папка {required}/. Выбери корень подготовленных ресурсов Horizon NX."
            ));
        }
    }

    let mut files = 0u64;
    let mut directories = 0u64;
    let mut total_bytes = 0u64;
    let mut packed_bytes = 0u64;
    let mut loose_files = 0u64;
    let mut loose_bytes = 0u64;
    let mut warnings = Vec::new();

    for item in WalkDir::new(&root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| !is_junk_name(&entry.file_name().to_string_lossy()))
    {
        let item = item.map_err(|e| format!("Ошибка чтения дерева ресурсов: {e}"))?;
        if item.path() == root.as_path() {
            continue;
        }
        let file_type = item.file_type();
        if file_type.is_symlink() {
            return Err(format!(
                "Символическая ссылка не поддерживается: {}. Скопируй реальный файл или папку.",
                item.path().display()
            ));
        }

        let relative = item
            .path()
            .strip_prefix(&root)
            .map_err(|e| e.to_string())?;
        let relative_text = relative.to_string_lossy().replace('\\', "/");
        if relative_text.as_bytes().len() > u16::MAX as usize {
            return Err(format!(
                "Слишком длинный путь для HZPAK: {relative_text}"
            ));
        }
        if relative_text.contains('\n') || relative_text.contains('\r') {
            return Err(format!("Путь содержит перевод строки: {relative_text}"));
        }

        if file_type.is_dir() {
            directories += 1;
        } else if file_type.is_file() {
            let size = item
                .metadata()
                .map_err(|e| format!("{}: {e}", item.path().display()))?
                .len();
            files += 1;
            total_bytes = total_bytes
                .checked_add(size)
                .ok_or_else(|| "Размер ресурсов превышает допустимый диапазон.".to_owned())?;
            if size <= MAX_PACKED {
                packed_bytes += size;
            } else {
                loose_files += 1;
                loose_bytes += size;
            }
        } else {
            warnings.push(format!(
                "Пропущен неподдерживаемый объект: {}",
                relative_text
            ));
        }
    }

    if files == 0 {
        return Err("Папка ресурсов не содержит файлов.".into());
    }

    let mut manifest_keys: Vec<String> = manifest_object.keys().cloned().collect();
    manifest_keys.sort();

    if loose_files > 0 {
        warnings.push(format!(
            "{loose_files} крупных файлов ({}) останутся обычными файлами рядом с resources.hzpak.",
            format_size(loose_bytes)
        ));
    }

    Ok(ScanReport {
        root,
        files,
        directories,
        total_bytes,
        packed_bytes,
        loose_files,
        loose_bytes,
        manifest_keys,
        warnings,
    })
}

pub fn is_junk_name(name: &str) -> bool {
    name == ".DS_Store"
        || name == ".localized"
        || name == "__MACOSX"
        || name.starts_with("._")
}

pub fn format_size(bytes: u64) -> String {
    const GB: f64 = 1_000_000_000.0;
    const MB: f64 = 1_000_000.0;
    const KB: f64 = 1_000.0;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.2} GB", value / GB)
    } else if value >= MB {
        format!("{:.1} MB", value / MB)
    } else if value >= KB {
        format!("{:.1} KB", value / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_a_minimal_resource_root() {
        let root = std::env::temp_dir().join(format!(
            "nx-toolkit-source-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join(MANIFEST_NAME), r#"{"format":1}"#).unwrap();
        std::fs::write(root.join("assets/example.txt"), "asset").unwrap();

        let report = validate_source(&root).unwrap();
        assert_eq!(report.files, 2);
        assert!(report.total_bytes > 0);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_non_json_manifest() {
        let root = std::env::temp_dir().join(format!(
            "nx-toolkit-invalid-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join(MANIFEST_NAME), "not json").unwrap();

        assert!(validate_source(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}

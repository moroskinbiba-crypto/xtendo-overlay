use crate::hzpak::{Entry, Kind, Top, Writer, MAX_PACKED};
use crate::validate::{is_junk_name, validate_source};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

struct Item {
    source: PathBuf,
    relative: String,
    is_dir: bool,
    size: u64,
    mtime: i64,
    modified: Option<SystemTime>,
}

fn timestamp_id() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    format!("{seconds}-{}", std::process::id())
}

fn modified_seconds(time: Option<SystemTime>) -> i64 {
    time.and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs() as i64)
}

fn collect_items(root: &Path) -> Result<Vec<Item>, String> {
    let mut items = Vec::new();

    for entry in WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| !is_junk_name(&entry.file_name().to_string_lossy()))
    {
        let entry = entry.map_err(|e| format!("Не удалось прочитать дерево ресурсов: {e}"))?;
        if entry.path() == root {
            continue;
        }

        let kind = entry.file_type();
        if kind.is_symlink() {
            return Err(format!(
                "Развёртывание остановлено: обнаружена символическая ссылка {}",
                entry.path().display()
            ));
        }

        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|e| e.to_string())?;
        let relative_text = relative.to_string_lossy().replace('\\', "/");
        if relative_text.is_empty()
            || relative_text.as_bytes().len() > u16::MAX as usize
            || relative_text.contains('\n')
            || relative_text.contains('\r')
        {
            return Err(format!("Путь не поддерживается форматом HZPAK: {relative_text}"));
        }

        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|e| format!("{}: {e}", entry.path().display()))?;
        let is_dir = kind.is_dir();
        if !is_dir && !kind.is_file() {
            return Err(format!(
                "Неподдерживаемый объект в ресурсах: {}",
                entry.path().display()
            ));
        }

        items.push(Item {
            source: entry.path().to_path_buf(),
            relative: relative_text,
            is_dir,
            size: if is_dir { 0 } else { metadata.len() },
            mtime: modified_seconds(metadata.modified().ok()),
            modified: metadata.modified().ok(),
        });
    }

    items.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(items)
}

fn top_level(root: &Path) -> Result<Vec<Top>, String> {
    let mut result = Vec::new();
    let entries = fs::read_dir(root).map_err(|e| format!("{}: {e}", root.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_junk_name(&name) {
            continue;
        }
        let kind = entry
            .file_type()
            .map_err(|e| format!("{}: {e}", entry.path().display()))?;
        if kind.is_symlink() {
            return Err(format!("Ссылка в корне ресурсов не поддерживается: {name}"));
        }
        let metadata = entry.metadata().map_err(|e| e.to_string())?;
        result.push(Top {
            name,
            dir: kind.is_dir(),
            size: if kind.is_dir() { 0 } else { metadata.len() },
        });
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

fn set_modified(path: &Path, modified: Option<SystemTime>) {
    if let Some(time) = modified {
        let _ = filetime::set_file_mtime(path, filetime::FileTime::from_system_time(time));
    }
}

fn path_kind(path: &Path) -> Result<Option<fs::FileType>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let kind = metadata.file_type();
            if kind.is_symlink() {
                return Err(format!(
                    "Отказ от замены ссылки вместо обычного файла/каталога: {}",
                    path.display()
                ));
            }
            Ok(Some(kind))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn remove_if_exists(path: &Path) {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            let _ = fs::remove_dir_all(path);
        }
        Ok(_) => {
            let _ = fs::remove_file(path);
        }
        Err(_) => {}
    }
}

fn restore_old(
    resources: &Path,
    pack: &Path,
    backup_resources: &Path,
    backup_pack: &Path,
    had_resources: bool,
    had_pack: bool,
    remove_new_resources: bool,
) -> Vec<String> {
    let mut problems = Vec::new();
    if remove_new_resources {
        if let Err(error) = fs::remove_dir_all(resources) {
            if error.kind() != std::io::ErrorKind::NotFound {
                problems.push(format!("не удалось убрать новую папку ресурсов: {error}"));
            }
        }
    }
    if had_resources && backup_resources.exists() {
        if let Err(error) = fs::rename(backup_resources, resources) {
            problems.push(format!("не удалось вернуть старую папку ресурсов: {error}"));
        }
    }
    if had_pack && backup_pack.exists() {
        if let Err(error) = fs::rename(backup_pack, pack) {
            problems.push(format!("не удалось вернуть старый resources.hzpak: {error}"));
        }
    }
    problems
}

fn build_staging(
    source: &Path,
    staging: &Path,
    pack_temp: &Path,
) -> Result<u64, String> {
    let items = collect_items(source)?;
    let top = top_level(source)?;
    let root_mtime = fs::metadata(source)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .map_or(0, |time| modified_seconds(Some(time)));

    fs::create_dir(staging)
        .map_err(|e| format!("Не удалось создать временную папку {}: {e}", staging.display()))?;
    let output = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(pack_temp)
    {
        Ok(file) => file,
        Err(error) => {
            remove_if_exists(staging);
            return Err(format!("Не удалось создать временный архив {}: {error}", pack_temp.display()));
        }
    };

    let result = (|| -> Result<u64, String> {
        let mut writer = Writer::begin(BufWriter::with_capacity(4 << 20, output), root_mtime)
            .map_err(|e| e.to_string())?;

        for item in &items {
            let target = staging.join(&item.relative);
            if item.is_dir {
                fs::create_dir_all(&target)
                    .map_err(|e| format!("{}: {e}", target.display()))?;
                set_modified(&target, item.modified);
                writer
                    .entry(
                        &item.relative,
                        Entry {
                            kind: Kind::Dir,
                            size: 0,
                            mtime: item.mtime,
                        },
                    )
                    .map_err(|e| e.to_string())?;
                continue;
            }

            if item.size <= MAX_PACKED {
                let mut data = Vec::with_capacity(item.size.min(2 << 20) as usize);
                File::open(&item.source)
                    .and_then(|mut input| input.read_to_end(&mut data))
                    .map_err(|e| format!("{}: {e}", item.source.display()))?;
                if data.len() as u64 != item.size {
                    return Err(format!(
                        "Файл изменился во время подготовки: {}",
                        item.source.display()
                    ));
                }
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let mut out = File::create(&target)
                    .map_err(|e| format!("{}: {e}", target.display()))?;
                out.write_all(&data).map_err(|e| e.to_string())?;
                out.sync_all().map_err(|e| e.to_string())?;
                set_modified(&target, item.modified);
                writer
                    .file(&item.relative, &data, item.mtime)
                    .map_err(|e| e.to_string())?;
            } else {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                fs::copy(&item.source, &target)
                    .map_err(|e| format!("{}: {e}", item.source.display()))?;
                set_modified(&target, item.modified);
                writer
                    .entry(
                        &item.relative,
                        Entry {
                            kind: Kind::Loose,
                            size: item.size,
                            mtime: item.mtime,
                        },
                    )
                    .map_err(|e| e.to_string())?;
            }
        }

        let mut output = writer.finish(&top).map_err(|e| e.to_string())?;
        output.flush().map_err(|e| e.to_string())?;
        output.get_ref().sync_all().map_err(|e| e.to_string())?;
        Ok(items.len() as u64)
    })();

    let item_count = match result {
        Ok(count) => count,
        Err(error) => {
            remove_if_exists(staging);
            remove_if_exists(pack_temp);
            return Err(error);
        }
    };

    if let Err(error) = crate::hzpak::verify(pack_temp) {
        remove_if_exists(staging);
        remove_if_exists(pack_temp);
        return Err(format!("Проверка созданного HZPAK не пройдена: {error}"));
    }
    Ok(item_count)
}

pub fn deploy_to_sd(source: &Path, sd_root: &Path) -> Result<String, String> {
    let report = validate_source(source)?;
    let source = report.root;
    let sd_root = sd_root
        .canonicalize()
        .map_err(|e| format!("Не удалось открыть корень SD-карты: {e}"))?;
    let switch_dir = sd_root.join("switch");
    if !switch_dir.is_dir() {
        return Err(format!(
            "{} не похож на корень SD-карты: внутри нет папки switch/.",
            sd_root.display()
        ));
    }

    let horizon_dir = switch_dir.join("horizon");
    let resources = horizon_dir.join("resources");
    let pack = horizon_dir.join("resources.hzpak");

    // The source must not be the destination, or a parent/child of it.
    let source_again = source
        .canonicalize()
        .map_err(|e| format!("{}: {e}", source.display()))?;
    let destination_anchor = switch_dir.join("horizon").join("resources");
    if source_again.starts_with(&destination_anchor) || destination_anchor.starts_with(&source_again) {
        return Err("Исходная папка и папка назначения пересекаются. Выбери ресурсы вне SD-карты.".into());
    }

    fs::create_dir_all(&horizon_dir)
        .map_err(|e| format!("{}: {e}", horizon_dir.display()))?;

    let id = timestamp_id();
    let staging = horizon_dir.join(format!("resources.__nxtoolkit_staging-{id}"));
    let pack_temp = horizon_dir.join(format!(".resources.hzpak.tmp-{id}"));
    let backup_root = switch_dir.join("horizon-backups").join(&id);
    let backup_resources = backup_root.join("resources");
    let backup_pack = backup_root.join("resources.hzpak");

    let resource_kind = path_kind(&resources)?;
    if resource_kind.as_ref().is_some_and(|kind| !kind.is_dir()) {
        return Err(format!("{} существует, но не является папкой.", resources.display()));
    }
    let pack_kind = path_kind(&pack)?;
    if pack_kind.as_ref().is_some_and(|kind| !kind.is_file()) {
        return Err(format!("{} существует, но не является обычным файлом.", pack.display()));
    }

    let item_count = match build_staging(&source, &staging, &pack_temp) {
        Ok(count) => count,
        Err(error) => return Err(error),
    };

    let had_resources = resource_kind.is_some();
    let had_pack = pack_kind.is_some();
    if had_resources || had_pack {
        fs::create_dir_all(&backup_root)
            .map_err(|e| format!("Не удалось создать резервную копию: {e}"))?;
    }

    if had_resources {
        if let Err(error) = fs::rename(&resources, &backup_resources) {
            remove_if_exists(&staging);
            remove_if_exists(&pack_temp);
            return Err(format!("Не удалось сохранить старые ресурсы: {error}"));
        }
    }

    if had_pack {
        if let Err(error) = fs::rename(&pack, &backup_pack) {
            let restore = restore_old(
                &resources,
                &pack,
                &backup_resources,
                &backup_pack,
                had_resources,
                false,
                false,
            );
            remove_if_exists(&staging);
            remove_if_exists(&pack_temp);
            return Err(format!(
                "Не удалось сохранить старый архив: {error}. Откат: {}",
                if restore.is_empty() { "успешно".into() } else { restore.join("; ") }
            ));
        }
    }

    if let Err(error) = fs::rename(&staging, &resources) {
        let restore = restore_old(
            &resources,
            &pack,
            &backup_resources,
            &backup_pack,
            had_resources,
            had_pack,
            false,
        );
        remove_if_exists(&staging);
        remove_if_exists(&pack_temp);
        return Err(format!(
            "Не удалось активировать новую папку ресурсов: {error}. Откат: {}",
            if restore.is_empty() { "успешно".into() } else { restore.join("; ") }
        ));
    }

    if let Err(error) = fs::rename(&pack_temp, &pack) {
        let restore = restore_old(
            &resources,
            &pack,
            &backup_resources,
            &backup_pack,
            had_resources,
            had_pack,
            true,
        );
        remove_if_exists(&pack_temp);
        return Err(format!(
            "Не удалось активировать resources.hzpak: {error}. Откат: {}",
            if restore.is_empty() { "успешно".into() } else { restore.join("; ") }
        ));
    }

    let backup_note = if had_resources || had_pack {
        format!("Старая установка сохранена в {}.", backup_root.display())
    } else {
        "Предыдущей установки не было; резервная копия не создавалась.".into()
    };

    Ok(format!(
        "Готово: обработано элементов — {item_count}. Ресурсы установлены в {}. {}",
        resources.display(),
        backup_note
    ))
}

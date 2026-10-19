use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use rust_i18n::Backend;
use serde::Deserialize;
use serde_json::Value;

const MANIFEST_FILE: &str = "manifest.json";
const FORMAT_VERSION: u32 = 1;
const MAX_LOCALE_DIRS: usize = 128;
const MAX_JSON_FILES_PER_PACK: usize = 32;
const MAX_JSON_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocaleSource {
    Bundled,
    User,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocaleInfo {
    pub locale: String,
    pub native_name: String,
    pub english_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IssueKind {
    Io,
    InvalidManifest,
    UnsupportedFormat,
    InvalidLocale,
    InvalidJson,
    InvalidMessage,
    PlaceholderMismatch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocaleIssue {
    pub source: LocaleSource,
    pub kind: IssueKind,
    pub path: PathBuf,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct Bootstrap {
    pub active_locale: String,
    pub locales: Vec<LocaleInfo>,
    pub issues: Vec<LocaleIssue>,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    format_version: u32,
    locale: String,
    native_name: String,
    #[serde(default)]
    english_name: Option<String>,
}

#[derive(Default)]
struct CatalogState {
    catalogs: BTreeMap<String, BTreeMap<String, String>>,
    locales: Vec<LocaleInfo>,
}

static GLOBAL_CATALOG: OnceLock<Arc<RwLock<CatalogState>>> = OnceLock::new();

fn global_catalog() -> &'static Arc<RwLock<CatalogState>> {
    GLOBAL_CATALOG.get_or_init(|| Arc::new(RwLock::new(CatalogState::default())))
}

/// Backend used by the desktop crate's `rust_i18n::i18n!` declaration.
///
/// The returned backend shares the process-wide catalog, so language packs can
/// be loaded before the first lookup and refreshed on a later startup.
#[derive(Clone)]
pub struct RuntimeBackend {
    state: Arc<RwLock<CatalogState>>,
}

impl Backend for RuntimeBackend {
    fn available_locales(&self) -> Vec<Cow<'_, str>> {
        let state = self.state.read().unwrap_or_else(|error| error.into_inner());
        state
            .locales
            .iter()
            .map(|locale| Cow::Owned(locale.locale.clone()))
            .collect()
    }

    fn translate(&self, locale: &str, key: &str) -> Option<Cow<'_, str>> {
        let state = self.state.read().unwrap_or_else(|error| error.into_inner());
        let (_, catalog) = lookup_catalog(&state.catalogs, locale)?;
        catalog.get(key).map(|value| Cow::Owned(value.clone()))
    }

    fn messages_for_locale(&self, locale: &str) -> Option<Vec<(Cow<'_, str>, Cow<'_, str>)>> {
        let state = self.state.read().unwrap_or_else(|error| error.into_inner());
        let (_, catalog) = lookup_catalog(&state.catalogs, locale)?;
        Some(
            catalog
                .iter()
                .map(|(key, value)| (Cow::Owned(key.clone()), Cow::Owned(value.clone())))
                .collect(),
        )
    }
}

pub fn backend() -> RuntimeBackend {
    RuntimeBackend {
        state: Arc::clone(global_catalog()),
    }
}

/// Load bundled and user language packs, then select the saved locale.
///
/// An empty `saved_locale` means "follow the operating system". User packs are
/// merged over bundled packs for the same locale; a malformed override is
/// ignored so one bad file cannot remove a working built-in translation.
pub fn initialize(bundled_root: &Path, user_root: &Path, saved_locale: &str) -> Bootstrap {
    let mut loaded = LoadedCatalogs::default();
    loaded.load_source(bundled_root, LocaleSource::Bundled);
    loaded.load_source(user_root, LocaleSource::User);

    let locales = loaded.locales();
    let active_locale = select_locale(saved_locale, &locales, system_locale().as_deref());

    let mut state = global_catalog()
        .write()
        .unwrap_or_else(|error| error.into_inner());
    state.catalogs = loaded.catalogs;
    state.locales = locales.clone();
    drop(state);

    rust_i18n::set_locale(&active_locale);

    Bootstrap {
        active_locale,
        locales,
        issues: loaded.issues,
    }
}

pub fn locales() -> Vec<LocaleInfo> {
    global_catalog()
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .locales
        .clone()
}

pub fn active_locale() -> String {
    rust_i18n::locale().to_string()
}

/// Keys present in the reference catalog but absent from `locale`.
///
/// User packs may intentionally be partial, so this is diagnostic rather than
/// a load error. Official packs should keep the result empty.
pub fn missing_keys(locale: &str, reference: &str) -> Vec<String> {
    let state = global_catalog()
        .read()
        .unwrap_or_else(|error| error.into_inner());
    let Some((_, reference)) = lookup_catalog(&state.catalogs, reference) else {
        return Vec::new();
    };
    let Some((_, catalog)) = lookup_catalog(&state.catalogs, locale) else {
        return reference.keys().cloned().collect();
    };
    reference
        .keys()
        .filter(|key| !catalog.contains_key(*key))
        .cloned()
        .collect()
}

/// Keys whose `%{placeholder}` set differs from the reference catalog.
pub fn placeholder_mismatches(locale: &str, reference: &str) -> Vec<String> {
    let state = global_catalog()
        .read()
        .unwrap_or_else(|error| error.into_inner());
    let Some((_, reference)) = lookup_catalog(&state.catalogs, reference) else {
        return Vec::new();
    };
    let Some((_, catalog)) = lookup_catalog(&state.catalogs, locale) else {
        return Vec::new();
    };
    reference
        .iter()
        .filter_map(|(key, reference_value)| {
            let value = catalog.get(key)?;
            (placeholders(reference_value) != placeholders(value)).then(|| key.clone())
        })
        .collect()
}

pub fn set_locale(locale: &str) {
    let available = locales();
    let selected = select_locale(locale, &available, system_locale().as_deref());
    rust_i18n::set_locale(&selected);
}

/// Resolve a persisted locale setting (`""` means follow the system).
pub fn effective_locale(saved_locale: &str) -> String {
    select_locale(saved_locale, &locales(), system_locale().as_deref())
}

/// Resolve `saved_locale` against the discovered packs without mutating global
/// state. This is also used by settings preview and tests.
pub fn select_locale(
    saved_locale: &str,
    available: &[LocaleInfo],
    system_locale: Option<&str>,
) -> String {
    let requested = if saved_locale.trim().is_empty() {
        system_locale
    } else {
        Some(saved_locale)
    };

    if let Some(requested) = requested
        && let Some(locale) = negotiate_locale(requested, available)
    {
        return locale;
    }

    available
        .iter()
        .find(|locale| locale.locale.eq_ignore_ascii_case("en"))
        .map(|locale| locale.locale.clone())
        .or_else(|| available.first().map(|locale| locale.locale.clone()))
        .unwrap_or_else(|| "en".to_string())
}

fn system_locale() -> Option<String> {
    sys_locale::get_locale()
}

fn negotiate_locale(requested: &str, available: &[LocaleInfo]) -> Option<String> {
    let normalized = normalize_locale(requested);
    for candidate in fallback_locales(&normalized) {
        if let Some(locale) = available
            .iter()
            .find(|locale| normalize_locale(&locale.locale) == candidate)
        {
            return Some(locale.locale.clone());
        }
    }

    let language = normalized.split('-').next()?;
    available
        .iter()
        .find(|locale| normalize_locale(&locale.locale).split('-').next() == Some(language))
        .map(|locale| locale.locale.clone())
}

fn fallback_locales(locale: &str) -> Vec<String> {
    let mut result = vec![locale.to_string()];
    let mut current = locale;
    while let Some((prefix, _)) = current.rsplit_once('-') {
        result.push(prefix.to_string());
        current = prefix;
    }
    result
}

fn normalize_locale(locale: &str) -> String {
    locale.trim().replace('_', "-").to_ascii_lowercase()
}

fn lookup_catalog<'a>(
    catalogs: &'a BTreeMap<String, BTreeMap<String, String>>,
    requested: &str,
) -> Option<(&'a str, &'a BTreeMap<String, String>)> {
    if let Some((locale, catalog)) = catalogs.get_key_value(requested) {
        return Some((locale.as_str(), catalog));
    }
    let normalized = normalize_locale(requested);
    catalogs
        .iter()
        .find(|(locale, _)| normalize_locale(locale) == normalized)
        .map(|(locale, catalog)| (locale.as_str(), catalog))
}

#[derive(Default)]
struct LoadedCatalogs {
    catalogs: BTreeMap<String, BTreeMap<String, String>>,
    locale_meta: BTreeMap<String, LocaleInfo>,
    issues: Vec<LocaleIssue>,
}

impl LoadedCatalogs {
    fn load_source(&mut self, root: &Path, source: LocaleSource) {
        if !root.exists() {
            return;
        }
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) => {
                self.issue(source, IssueKind::Io, root, error.to_string());
                return;
            }
        };

        let mut dirs = entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_type()
                    .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
            })
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        dirs.sort();

        if dirs.len() > MAX_LOCALE_DIRS {
            self.issue(
                source,
                IssueKind::InvalidManifest,
                root,
                format!(
                    "语言包目录过多（{}，上限 {MAX_LOCALE_DIRS}），超出的目录不会加载",
                    dirs.len()
                ),
            );
            dirs.truncate(MAX_LOCALE_DIRS);
        }

        for dir in dirs {
            self.load_pack(&dir, source);
        }
    }

    fn load_pack(&mut self, dir: &Path, source: LocaleSource) {
        let manifest_path = dir.join(MANIFEST_FILE);
        if !fs::symlink_metadata(&manifest_path).is_ok_and(|metadata| {
            metadata.file_type().is_file() && !metadata.file_type().is_symlink()
        }) {
            self.issue(
                source,
                IssueKind::InvalidManifest,
                &manifest_path,
                "manifest.json 必须是普通文件，不能是符号链接".to_string(),
            );
            return;
        }
        let manifest_bytes = match fs::read(&manifest_path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.issue(source, IssueKind::Io, &manifest_path, error.to_string());
                return;
            }
        };
        let manifest: Manifest = match serde_json::from_slice(&manifest_bytes) {
            Ok(manifest) => manifest,
            Err(error) => {
                self.issue(
                    source,
                    IssueKind::InvalidManifest,
                    &manifest_path,
                    error.to_string(),
                );
                return;
            }
        };
        if manifest.format_version != FORMAT_VERSION {
            self.issue(
                source,
                IssueKind::UnsupportedFormat,
                &manifest_path,
                format!(
                    "不支持的格式版本 {}（当前只支持 {FORMAT_VERSION}）",
                    manifest.format_version
                ),
            );
            return;
        }
        if !valid_locale_code(&manifest.locale) {
            self.issue(
                source,
                IssueKind::InvalidLocale,
                &manifest_path,
                format!("非法 locale：{}", manifest.locale),
            );
            return;
        }
        if manifest.native_name.trim().is_empty() {
            self.issue(
                source,
                IssueKind::InvalidManifest,
                &manifest_path,
                "native_name 不能为空".to_string(),
            );
            return;
        }
        let dir_name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if !dir_name.eq_ignore_ascii_case(&manifest.locale) {
            self.issue(
                source,
                IssueKind::InvalidLocale,
                &manifest_path,
                format!(
                    "目录名 {dir_name:?} 与 manifest locale {:?} 不一致",
                    manifest.locale
                ),
            );
            return;
        }

        let mut files = match locale_json_files(dir) {
            Ok(files) => files,
            Err(error) => {
                self.issue(source, IssueKind::Io, dir, error);
                return;
            }
        };
        files.sort();
        if files.len() > MAX_JSON_FILES_PER_PACK {
            self.issue(
                source,
                IssueKind::InvalidJson,
                dir,
                format!(
                    "语言包 JSON 文件过多（{}，上限 {MAX_JSON_FILES_PER_PACK}）",
                    files.len()
                ),
            );
            return;
        }

        let mut pack = BTreeMap::new();
        for file in files {
            if !load_messages_file(&file, &mut pack, &mut self.issues, source) {
                return;
            }
        }
        if pack.is_empty() {
            self.issue(
                source,
                IssueKind::InvalidJson,
                dir,
                "语言包没有任何翻译条目".to_string(),
            );
            return;
        }

        let locale = self
            .catalogs
            .keys()
            .find(|existing| existing.eq_ignore_ascii_case(&manifest.locale))
            .cloned()
            .unwrap_or(manifest.locale);
        let mut placeholder_conflicts = Vec::new();
        {
            let target = self.catalogs.entry(locale.clone()).or_default();
            for (key, value) in pack {
                if let Some(existing) = target.get(&key)
                    && placeholders(existing) != placeholders(&value)
                {
                    placeholder_conflicts.push(key);
                    continue;
                }
                target.insert(key, value);
            }
        }
        for key in placeholder_conflicts {
            self.issue(
                source,
                IssueKind::PlaceholderMismatch,
                dir,
                format!("{key} 的占位符与现有翻译不一致，已保留现有值"),
            );
        }
        self.locale_meta
            .entry(locale.clone())
            .or_insert(LocaleInfo {
                locale,
                native_name: manifest.native_name,
                english_name: manifest.english_name,
            });
    }

    fn locales(&self) -> Vec<LocaleInfo> {
        self.locale_meta.values().cloned().collect()
    }

    fn issue(&mut self, source: LocaleSource, kind: IssueKind, path: &Path, message: String) {
        self.issues.push(LocaleIssue {
            source,
            kind,
            path: path.to_path_buf(),
            message,
        });
    }
}

fn locale_json_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = fs::read_dir(dir).map_err(|error| error.to_string())?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        if path.file_name().and_then(|name| name.to_str()) == Some(MANIFEST_FILE) {
            continue;
        }
        if metadata.len() > MAX_JSON_FILE_BYTES {
            return Err(format!(
                "{} 超过单文件上限 {MAX_JSON_FILE_BYTES} 字节",
                path.display()
            ));
        }
        files.push(path);
    }
    Ok(files)
}

fn load_messages_file(
    path: &Path,
    pack: &mut BTreeMap<String, String>,
    issues: &mut Vec<LocaleIssue>,
    source: LocaleSource,
) -> bool {
    let namespace = match path.file_stem().and_then(|name| name.to_str()) {
        Some(namespace) if valid_key_segment(namespace) => namespace,
        _ => {
            issues.push(LocaleIssue {
                source,
                kind: IssueKind::InvalidMessage,
                path: path.to_path_buf(),
                message: "翻译文件名必须是合法的命名空间".to_string(),
            });
            return false;
        }
    };
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            issues.push(LocaleIssue {
                source,
                kind: IssueKind::Io,
                path: path.to_path_buf(),
                message: error.to_string(),
            });
            return false;
        }
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => {
            issues.push(LocaleIssue {
                source,
                kind: IssueKind::InvalidJson,
                path: path.to_path_buf(),
                message: error.to_string(),
            });
            return false;
        }
    };

    let mut messages = BTreeMap::new();
    if flatten_messages(namespace, &value, &mut messages).is_err() {
        issues.push(LocaleIssue {
            source,
            kind: IssueKind::InvalidMessage,
            path: path.to_path_buf(),
            message: "翻译文件只能包含以字符串为叶子节点的对象".to_string(),
        });
        return false;
    }

    for (key, value) in messages {
        if placeholders_checked(&value).is_err() {
            issues.push(LocaleIssue {
                source,
                kind: IssueKind::InvalidMessage,
                path: path.to_path_buf(),
                message: format!("{key} 含未闭合的 %{{...}} 占位符"),
            });
            return false;
        }
        if pack.insert(key.clone(), value).is_some() {
            issues.push(LocaleIssue {
                source,
                kind: IssueKind::InvalidMessage,
                path: path.to_path_buf(),
                message: format!("{key} 在语言包内重复"),
            });
            return false;
        }
    }
    true
}

fn flatten_messages(
    prefix: &str,
    value: &Value,
    output: &mut BTreeMap<String, String>,
) -> Result<(), ()> {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if !valid_key_segment(key) {
                    return Err(());
                }
                let next = format!("{prefix}.{key}");
                flatten_messages(&next, child, output)?;
            }
            Ok(())
        }
        Value::String(value) if !value.is_empty() => {
            if output.insert(prefix.to_string(), value.clone()).is_some() {
                return Err(());
            }
            Ok(())
        }
        _ => Err(()),
    }
}

fn valid_key_segment(segment: &str) -> bool {
    !segment.is_empty() && !segment.contains('.') && !segment.chars().any(char::is_whitespace)
}

/// Conservative BCP 47 subset: ASCII letters/digits separated by single
/// hyphens. The manifest is loaded from disk, so reject path-like input early.
pub fn valid_locale_code(locale: &str) -> bool {
    let bytes = locale.as_bytes();
    if bytes.is_empty() || bytes.len() > 35 || !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    let mut previous_hyphen = false;
    for byte in bytes {
        if *byte == b'-' {
            if previous_hyphen {
                return false;
            }
            previous_hyphen = true;
        } else if byte.is_ascii_alphanumeric() {
            previous_hyphen = false;
        } else {
            return false;
        }
    }
    !previous_hyphen
}

fn placeholders(value: &str) -> BTreeSet<String> {
    placeholders_checked(value).unwrap_or_default()
}

fn placeholders_checked(value: &str) -> Result<BTreeSet<String>, ()> {
    let mut placeholders = BTreeSet::new();
    let mut remaining = value;
    while let Some(start) = remaining.find("%{") {
        let after = &remaining[start + 2..];
        let end = after.find('}').ok_or(())?;
        let name = &after[..end];
        if name.is_empty() || name.contains('%') {
            return Err(());
        }
        placeholders.insert(name.to_string());
        remaining = &after[end + 1..];
    }
    Ok(placeholders)
}

/// Translate through the process-wide catalog, including language fallback and
/// the built-in English fallback. This helper is intended for application-layer
/// messages that cannot use the desktop crate's generated `t!` macro.
pub fn translate(key: &str) -> String {
    let state = global_catalog()
        .read()
        .unwrap_or_else(|error| error.into_inner());
    let current = rust_i18n::locale().to_string();
    for locale in fallback_locales(&normalize_locale(&current))
        .into_iter()
        .chain(["en".to_string()])
    {
        if let Some((_, catalog)) = lookup_catalog(&state.catalogs, &locale)
            && let Some(value) = catalog.get(key)
        {
            return value.clone();
        }
    }
    key.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_pack(root: &Path, locale: &str, native: &str, files: &[(&str, &str)]) {
        let dir = root.join(locale);
        fs::create_dir_all(&dir).expect("create locale dir");
        fs::write(
            dir.join(MANIFEST_FILE),
            format!(r#"{{"format_version":1,"locale":"{locale}","native_name":"{native}"}}"#),
        )
        .expect("write manifest");
        for (name, body) in files {
            fs::write(dir.join(name), body).expect("write locale file");
        }
    }

    fn load(bundled: &Path, user: &Path) -> LoadedCatalogs {
        let mut loaded = LoadedCatalogs::default();
        loaded.load_source(bundled, LocaleSource::Bundled);
        loaded.load_source(user, LocaleSource::User);
        loaded
    }

    #[test]
    fn user_pack_adds_language_without_code_changes() {
        let bundled = tempfile::tempdir().expect("bundled temp dir");
        let user = tempfile::tempdir().expect("user temp dir");
        write_pack(
            bundled.path(),
            "en",
            "English",
            &[("app.json", r#"{"save":"Save"}"#)],
        );
        write_pack(
            user.path(),
            "ja",
            "日本語",
            &[
                ("app.json", r#"{"save":"保存"}"#),
                (
                    "gpui_component.json",
                    r#"{"DatePicker":{"placeholder":"日付を選択"}}"#,
                ),
            ],
        );

        let loaded = load(bundled.path(), user.path());

        assert_eq!(
            loaded
                .catalogs
                .get("ja")
                .and_then(|catalog| catalog.get("app.save"))
                .map(String::as_str),
            Some("保存")
        );
        assert_eq!(
            loaded
                .catalogs
                .get("ja")
                .and_then(|catalog| catalog.get("gpui_component.DatePicker.placeholder"))
                .map(String::as_str),
            Some("日付を選択")
        );
        assert!(loaded.issues.is_empty());
    }

    #[test]
    fn malformed_pack_is_reported_without_hiding_other_languages() {
        let bundled = tempfile::tempdir().expect("bundled temp dir");
        let user = tempfile::tempdir().expect("user temp dir");
        write_pack(
            bundled.path(),
            "en",
            "English",
            &[("app.json", r#"{"save":"Save"}"#)],
        );
        let invalid = user.path().join("fr");
        fs::create_dir_all(&invalid).expect("create invalid pack");
        fs::write(invalid.join(MANIFEST_FILE), "{not json").expect("write invalid manifest");
        write_pack(
            user.path(),
            "ja",
            "日本語",
            &[("app.json", r#"{"save":"保存"}"#)],
        );

        let loaded = load(bundled.path(), user.path());

        assert!(loaded.catalogs.contains_key("ja"));
        assert!(!loaded.catalogs.contains_key("fr"));
        assert!(loaded.issues.iter().any(|issue| {
            issue.source == LocaleSource::User
                && issue.kind == IssueKind::InvalidManifest
                && issue.path.ends_with("fr/manifest.json")
        }));
    }

    #[test]
    fn user_override_keeps_existing_translation_when_placeholder_differs() {
        let bundled = tempfile::tempdir().expect("bundled temp dir");
        let user = tempfile::tempdir().expect("user temp dir");
        write_pack(
            bundled.path(),
            "en",
            "English",
            &[("app.json", r#"{"greeting":"Hello, %{name}"}"#)],
        );
        write_pack(
            user.path(),
            "en",
            "English",
            &[("app.json", r#"{"greeting":"Hello"}"#)],
        );

        let loaded = load(bundled.path(), user.path());

        assert_eq!(
            loaded
                .catalogs
                .get("en")
                .and_then(|catalog| catalog.get("app.greeting"))
                .map(String::as_str),
            Some("Hello, %{name}")
        );
        assert!(
            loaded
                .issues
                .iter()
                .any(|issue| issue.kind == IssueKind::PlaceholderMismatch)
        );
    }

    #[test]
    fn locale_selection_negotiates_script_and_language() {
        let available = vec![
            LocaleInfo {
                locale: "en".into(),
                native_name: "English".into(),
                english_name: None,
            },
            LocaleInfo {
                locale: "zh-CN".into(),
                native_name: "简体中文".into(),
                english_name: None,
            },
        ];

        assert_eq!(
            select_locale("zh-Hans-CN", &available, None),
            "zh-CN",
            "script/region fallback should reach the language pack"
        );
        assert_eq!(
            select_locale("", &available, Some("zh-CN")),
            "zh-CN",
            "empty setting follows the system locale"
        );
        assert_eq!(
            select_locale("xx", &available, None),
            "en",
            "unknown saved locale falls back to English"
        );
    }
}

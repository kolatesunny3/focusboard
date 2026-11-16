//! File storage: atomic writes, `.bak` fallback, daily backups, migrations.
//!
//! CONTRACT (fixed — see agents.md): keep every public item and signature below.

use crate::model::{
    DataRoot, EntityFile, FieldDef, FieldType, ListDef, SCHEMA_VERSION, Settings, Task,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const LISTS_FILE: &str = "lists.json";
pub const FIELDS_FILE: &str = "fields.json";
pub const TASKS_FILE: &str = "tasks.json";
pub const SETTINGS_FILE: &str = "settings.json";

/// The four data files, in dependency order (lists → fields → tasks → settings).
const DATA_FILES: [&str; 4] = [LISTS_FILE, FIELDS_FILE, TASKS_FILE, SETTINGS_FILE];

/// Write attempts before giving up — antivirus scanners can hold files briefly.
const WRITE_ATTEMPTS: u32 = 3;
/// Pause between write attempts.
const RETRY_DELAY_MS: u64 = 120;
/// Daily snapshots kept in `backups/`.
const KEEP_BACKUPS: usize = 7;

#[derive(Debug)]
pub struct Error {
    message: String,
}

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Error {}

/// Which files need to be written on the next save.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dirty {
    pub lists: bool,
    pub fields: bool,
    pub tasks: bool,
    pub settings: bool,
}

impl Dirty {
    pub const ALL: Dirty = Dirty {
        lists: true,
        fields: true,
        tasks: true,
        settings: true,
    };

    pub fn any(&self) -> bool {
        self.lists || self.fields || self.tasks || self.settings
    }
}

/// Result of a load: data plus non-fatal warnings (e.g. ".bak was used").
pub struct LoadResult {
    pub data: DataRoot,
    pub warnings: Vec<String>,
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `%LOCALAPPDATA%\FocusBoard` (falls back to the current directory).
    pub fn default_root() -> PathBuf {
        match std::env::var_os("LOCALAPPDATA") {
            Some(base) if !base.is_empty() => PathBuf::from(base).join("FocusBoard"),
            _ => std::env::current_dir()
                .unwrap_or_default()
                .join("FocusBoard"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn load(&self) -> Result<LoadResult, Error> {
        let mut warnings = Vec::new();

        let lists = match self.load_json_with_backup::<EntityFile<ListDef>>(LISTS_FILE, &mut warnings)? {
            Some(file) => {
                check_schema_version(LISTS_FILE, file.schema_version, &mut warnings)?;
                file.items
            }
            None => Vec::new(),
        };
        let fields = match self.load_json_with_backup::<EntityFile<FieldDef>>(FIELDS_FILE, &mut warnings)? {
            Some(file) => {
                check_schema_version(FIELDS_FILE, file.schema_version, &mut warnings)?;
                file.items
            }
            None => Vec::new(),
        };
        let tasks = match self.load_json_with_backup::<EntityFile<Task>>(TASKS_FILE, &mut warnings)? {
            Some(file) => {
                check_schema_version(TASKS_FILE, file.schema_version, &mut warnings)?;
                file.items
            }
            None => Vec::new(),
        };
        let settings = match self.load_json_with_backup::<Settings>(SETTINGS_FILE, &mut warnings)? {
            Some(settings) => {
                check_schema_version(SETTINGS_FILE, settings.schema_version, &mut warnings)?;
                settings
            }
            None => Settings::default(),
        };

        Ok(LoadResult {
            data: DataRoot {
                lists,
                fields,
                tasks,
                settings,
            },
            warnings,
        })
    }

    pub fn save(&self, data: &DataRoot, dirty: Dirty) -> Result<(), Error> {
        if !dirty.any() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.root).map_err(|e| {
            Error::new(format!(
                "could not create data folder {}: {e}",
                self.root.display()
            ))
        })?;

        // Dependency order: files never reference schema that is not on disk yet.
        if dirty.lists {
            let bytes = to_pretty_bytes(&EntityFile::new(data.lists.clone()), LISTS_FILE)?;
            self.write_atomic(LISTS_FILE, &bytes)?;
        }
        if dirty.fields {
            let bytes = to_pretty_bytes(&EntityFile::new(data.fields.clone()), FIELDS_FILE)?;
            self.write_atomic(FIELDS_FILE, &bytes)?;
        }
        if dirty.tasks {
            let bytes = to_pretty_bytes(&EntityFile::new(data.tasks.clone()), TASKS_FILE)?;
            self.write_atomic(TASKS_FILE, &bytes)?;
        }
        if dirty.settings {
            let bytes = to_pretty_bytes(&data.settings, SETTINGS_FILE)?;
            self.write_atomic(SETTINGS_FILE, &bytes)?;
        }
        Ok(())
    }

    /// One snapshot per day; keeps the newest 7.
    pub fn backup_daily(&self) -> Result<(), Error> {
        let backups = self.backups_dir();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let today_dir = backups.join(&today);

        if !today_dir.exists() {
            std::fs::create_dir_all(&today_dir).map_err(|e| {
                Error::new(format!(
                    "could not create backup folder {}: {e}",
                    today_dir.display()
                ))
            })?;
            self.copy_data_files_to(&today_dir)?;
        }

        self.prune_backups(KEEP_BACKUPS)
    }

    /// Snapshot right now, labelled (e.g. "pre-import"); returns the folder.
    pub fn backup_now(&self, label: &str) -> Result<PathBuf, Error> {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let dir = self.backups_dir().join(format!("{label}-{stamp}"));
        std::fs::create_dir_all(&dir).map_err(|e| {
            Error::new(format!(
                "could not create backup folder {}: {e}",
                dir.display()
            ))
        })?;
        self.copy_data_files_to(&dir)?;
        Ok(dir)
    }

    /// Copy the data files into `dir`.
    pub fn export_to(&self, dir: &Path) -> Result<(), Error> {
        std::fs::create_dir_all(dir).map_err(|e| {
            Error::new(format!(
                "could not create export folder {}: {e}",
                dir.display()
            ))
        })?;
        if self.copy_data_files_to(dir)? == 0 {
            return Err(Error::new(format!(
                "no data to export (no data files in {})",
                self.root.display()
            )));
        }
        Ok(())
    }

    /// Validate `dir` contains a readable data set, back up, then replace.
    pub fn import_from(&self, dir: &Path) -> Result<(), Error> {
        let source = Store::new(dir);
        source
            .load()
            .map_err(|e| Error::new(format!("import: {e}")))?;

        if !DATA_FILES.iter().any(|name| dir.join(name).is_file()) {
            return Err(Error::new(format!(
                "import: no data files found in {}",
                dir.display()
            )));
        }

        self.backup_now("pre-import")?;

        std::fs::create_dir_all(&self.root).map_err(|e| {
            Error::new(format!(
                "could not create data folder {}: {e}",
                self.root.display()
            ))
        })?;
        for name in DATA_FILES {
            let src = dir.join(name);
            if src.is_file() {
                copy_file(&src, &self.root.join(name), name)
                    .map_err(|e| Error::new(format!("import: {e}")))?;
            }
        }
        Ok(())
    }

    pub fn backups_dir(&self) -> PathBuf {
        self.root.join("backups")
    }

    /// Read and parse `name`, preferring the main file and falling back to
    /// `<name>.bak`. `Ok(None)` means neither file exists (first run).
    fn load_json_with_backup<T: DeserializeOwned>(
        &self,
        name: &str,
        warnings: &mut Vec<String>,
    ) -> Result<Option<T>, Error> {
        let main_path = self.root.join(name);
        let bak_path = self.root.join(format!("{name}.bak"));

        if let Ok(text) = std::fs::read_to_string(&main_path)
            && let Ok(value) = serde_json::from_str::<T>(&text)
        {
            return Ok(Some(value));
        }

        if let Ok(text) = std::fs::read_to_string(&bak_path)
            && let Ok(value) = serde_json::from_str::<T>(&text)
        {
            warnings.push(format!("Recovered {name} from its backup"));
            return Ok(Some(value));
        }

        if main_path.exists() || bak_path.exists() {
            return Err(Error::new(format!(
                "could not read {name} and its backup is not usable; restore \
                 {name} from the backups folder ({})",
                self.backups_dir().display()
            )));
        }

        Ok(None)
    }

    /// Atomic single-file write with retries: tmp → (old content to `.bak`) → rename.
    fn write_atomic(&self, name: &str, bytes: &[u8]) -> Result<(), Error> {
        let tmp_path = self.root.join(format!("{name}.tmp"));
        let mut last_error: Option<std::io::Error> = None;

        for attempt in 0..WRITE_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS));
            }
            match self.write_atomic_once(name, bytes) {
                Ok(()) => return Ok(()),
                Err(e) => last_error = Some(e),
            }
        }

        let _ = std::fs::remove_file(&tmp_path);
        let reason = last_error
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown error".into());
        Err(Error::new(format!("could not write {name}: {reason}")))
    }

    fn write_atomic_once(&self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        let final_path = self.root.join(name);
        let tmp_path = self.root.join(format!("{name}.tmp"));
        let bak_path = self.root.join(format!("{name}.bak"));

        std::fs::write(&tmp_path, bytes)?;
        if final_path.exists() {
            std::fs::copy(&final_path, &bak_path)?;
        }
        std::fs::rename(&tmp_path, &final_path)
    }

    /// Copy whichever data files exist in the store root into `dest`.
    /// Returns how many files were copied.
    fn copy_data_files_to(&self, dest: &Path) -> Result<usize, Error> {
        let mut copied = 0;
        for name in DATA_FILES {
            let src = self.root.join(name);
            if src.is_file() {
                copy_file(&src, &dest.join(name), name)?;
                copied += 1;
            }
        }
        Ok(copied)
    }

    /// Delete daily snapshots beyond the newest `keep` (by folder name).
    fn prune_backups(&self, keep: usize) -> Result<(), Error> {
        let backups = self.backups_dir();
        let entries = match std::fs::read_dir(&backups) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(Error::new(format!(
                    "could not list backup folder {}: {e}",
                    backups.display()
                )));
            }
        };

        let mut names: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && let Some(name) = entry.file_name().to_str()
            {
                names.push(name.to_owned());
            }
        }
        names.sort_by(|a, b| b.cmp(a)); // newest name first

        for name in names.into_iter().skip(keep) {
            let path = backups.join(&name);
            std::fs::remove_dir_all(&path).map_err(|e| {
                Error::new(format!(
                    "could not delete old backup {}: {e}",
                    path.display()
                ))
            })?;
        }
        Ok(())
    }
}

/// Reject files from newer app versions; warn on older ones (migrate hook below).
fn check_schema_version(name: &str, version: u32, warnings: &mut Vec<String>) -> Result<(), Error> {
    if version > SCHEMA_VERSION {
        return Err(Error::new(format!(
            "data was created by a newer version of FocusBoard (file {name}, version {version})"
        )));
    }
    if version < SCHEMA_VERSION {
        migrate_to_current(version)?;
        warnings.push(format!(
            "migrated {name} from v{version} to v{SCHEMA_VERSION}"
        ));
    }
    Ok(())
}

/// Migration hook. Nothing to migrate yet; older files load as-is.
fn migrate_to_current(_version: u32) -> Result<(), Error> {
    Ok(())
}

/// Pretty-printed JSON with a trailing newline.
fn to_pretty_bytes<T: Serialize>(value: &T, name: &str) -> Result<Vec<u8>, Error> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| Error::new(format!("could not encode {name}: {e}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Copy `src` over `dst`, skipping when both paths are the same file.
fn copy_file(src: &Path, dst: &Path, name: &str) -> Result<(), Error> {
    if is_same_file(src, dst) {
        return Ok(());
    }
    std::fs::copy(src, dst)
        .map_err(|e| Error::new(format!("could not copy {name} to {}: {e}", dst.display())))?;
    Ok(())
}

fn is_same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// A select value survives cleanup when it is null or its option id is in the
/// master list (archived options still count as present).
fn select_option_is_known(field: &FieldDef, value: &Value, lists: &[ListDef]) -> bool {
    match value {
        Value::Null => true,
        Value::String(id) => field
            .list_id
            .as_deref()
            .and_then(|list_id| lists.iter().find(|l| l.id == list_id))
            .is_some_and(|list| list.option(id).is_some()),
        _ => true,
    }
}

/// Remove task values whose field no longer exists, and option ids that are
/// no longer in their master list. Returns true when anything changed.
pub fn cleanup_references(data: &mut DataRoot) -> bool {
    let fields = &data.fields;
    let lists = &data.lists;
    let mut changed = false;

    for task in &mut data.tasks {
        let mut removals: Vec<String> = Vec::new();
        let mut replacements: Vec<(String, Value)> = Vec::new();

        for (field_id, value) in &task.values {
            if value.is_null() {
                continue; // empty values may stay
            }
            let Some(field) = fields.iter().find(|f| &f.id == field_id) else {
                removals.push(field_id.clone());
                continue;
            };
            match field.ty {
                FieldType::Select => {
                    if !select_option_is_known(field, value, lists) {
                        removals.push(field_id.clone());
                    }
                }
                FieldType::Multiselect => {
                    let Value::Array(items) = value else {
                        continue;
                    };
                    let master = field
                        .list_id
                        .as_deref()
                        .and_then(|list_id| lists.iter().find(|l| l.id == list_id));
                    let kept: Vec<Value> = items
                        .iter()
                        .filter(|item| match item {
                            Value::String(id) => master.is_some_and(|m| m.option(id).is_some()),
                            _ => true,
                        })
                        .cloned()
                        .collect();
                    if kept.len() != items.len() {
                        // Keep the key consistently; an emptied array stays as [].
                        replacements.push((field_id.clone(), Value::Array(kept)));
                    }
                }
                _ => {}
            }
        }

        for key in removals {
            task.values.remove(&key);
            changed = true;
        }
        for (key, value) in replacements {
            task.values.insert(key, value);
            changed = true;
        }
    }

    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{OptionDef, Theme, WindowState};
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Process-wide counter so parallel tests never share a temp folder.
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Temporary folder that deletes itself. Named with process id + counter,
    /// and never points at the real `%LOCALAPPDATA%\FocusBoard`.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let unique = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!(
                "focusboard-storage-{tag}-{}-{unique}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn store(&self) -> Store {
            Store::new(&self.path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn option(id: &str, label: &str) -> OptionDef {
        OptionDef {
            id: id.into(),
            label: label.into(),
            color: None,
            archived: false,
        }
    }

    fn sample_data() -> DataRoot {
        let lists = vec![ListDef {
            id: "list-status".into(),
            name: "Status".into(),
            archived: false,
            options: vec![option("opt-todo", "To Do"), option("opt-done", "Done")],
        }];
        let fields = vec![
            FieldDef {
                id: "f-status".into(),
                label: "Status".into(),
                ty: FieldType::Select,
                required: false,
                default_value: Some(json!("opt-todo")),
                list_id: Some("list-status".into()),
                help: None,
                show_in_list: true,
                archived: false,
            },
            FieldDef {
                id: "f-due".into(),
                label: "Due date".into(),
                ty: FieldType::Date,
                required: false,
                default_value: None,
                list_id: None,
                help: None,
                show_in_list: true,
                archived: false,
            },
        ];
        let mut task = Task::new("Ship the MVP");
        task.description = "Keep it small".into();
        task.values.insert("f-status".into(), json!("opt-todo"));
        task.values.insert("f-due".into(), json!("2026-10-31"));
        DataRoot {
            lists,
            fields,
            tasks: vec![task],
            settings: Settings {
                schema_version: SCHEMA_VERSION,
                theme: Theme::Dark,
                sidebar_collapsed: false,
                window: WindowState {
                    width: 1234.0,
                    height: 777.0,
                    x: Some(10.0),
                    y: Some(20.0),
                    maximized: true,
                },
            },
        }
    }

    fn snapshot(data: &DataRoot) -> Value {
        json!({
            "lists": data.lists,
            "fields": data.fields,
            "tasks": data.tasks,
            "settings": data.settings,
        })
    }

    fn dir_entries(path: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(path)
            .expect("read dir")
            .map(|entry| entry.expect("dir entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn storage_round_trip_save_load() {
        let tmp = TempDir::new("round-trip");
        let store = tmp.store();
        let data = sample_data();

        store.save(&data, Dirty::ALL).expect("save");

        let loaded = store.load().expect("load");
        assert!(loaded.warnings.is_empty(), "warnings: {:?}", loaded.warnings);
        assert_eq!(snapshot(&loaded.data), snapshot(&data));
    }

    #[test]
    fn storage_save_writes_only_dirty_files() {
        let tmp = TempDir::new("dirty");
        let store = tmp.store();
        let mut data = sample_data();
        store.save(&data, Dirty::ALL).expect("save all");

        let lists_before = std::fs::read(store.root().join(LISTS_FILE)).expect("read lists");
        data.tasks.push(Task::new("Another task"));
        store
            .save(
                &data,
                Dirty {
                    tasks: true,
                    ..Dirty::default()
                },
            )
            .expect("save tasks only");

        let lists_after = std::fs::read(store.root().join(LISTS_FILE)).expect("read lists");
        assert_eq!(lists_before, lists_after, "lists.json must stay untouched");

        let loaded = store.load().expect("load");
        assert_eq!(loaded.data.tasks.len(), 2);
    }

    #[test]
    fn storage_recovers_corrupt_file_from_backup() {
        let tmp = TempDir::new("bak");
        let store = tmp.store();
        let mut data = sample_data();
        store.save(&data, Dirty::ALL).expect("first save");

        data.lists.push(ListDef {
            id: "list-extra".into(),
            name: "Extra".into(),
            archived: false,
            options: vec![option("opt-x", "X")],
        });
        data.tasks.push(Task::new("Later task"));
        store.save(&data, Dirty::ALL).expect("second save");

        std::fs::write(store.root().join(LISTS_FILE), b"{ corrupted").expect("corrupt main");

        let loaded = store.load().expect("load");
        assert_eq!(
            loaded.data.lists.len(),
            1,
            "backup must hold the previous content"
        );
        assert_eq!(loaded.data.lists[0].id, "list-status");
        assert_eq!(loaded.data.tasks.len(), 2, "other files load from main");
        assert!(
            loaded
                .warnings
                .iter()
                .any(|w| w == "Recovered lists.json from its backup"),
            "warnings: {:?}",
            loaded.warnings
        );
    }

    #[test]
    fn storage_fails_when_main_and_backup_are_corrupt() {
        let tmp = TempDir::new("corrupt");
        let store = tmp.store();
        std::fs::write(store.root().join(TASKS_FILE), b"garbage").expect("write main");
        std::fs::write(
            store.root().join(format!("{TASKS_FILE}.bak")),
            b"garbage",
        )
        .expect("write bak");

        let Err(err) = store.load() else {
            panic!("load must fail when main and backup are corrupt");
        };
        assert!(
            err.message().contains(TASKS_FILE),
            "message: {}",
            err.message()
        );
        assert!(
            err.message().contains("backups"),
            "message: {}",
            err.message()
        );
    }

    #[test]
    fn storage_rejects_newer_schema_version() {
        let tmp = TempDir::new("newer");
        let store = tmp.store();
        let newer =
            serde_json::to_vec_pretty(&json!({ "schemaVersion": 99, "items": [] })).expect("json");
        std::fs::write(store.root().join(TASKS_FILE), newer).expect("write");

        let Err(err) = store.load() else {
            panic!("load must refuse data from a newer version");
        };
        assert!(
            err.message().contains("newer version of FocusBoard"),
            "message: {}",
            err.message()
        );
        assert!(
            err.message().contains(TASKS_FILE),
            "message: {}",
            err.message()
        );
        assert!(err.message().contains("99"), "message: {}", err.message());
    }

    #[test]
    fn storage_migrates_older_schema_version_with_warning() {
        let tmp = TempDir::new("older");
        let store = tmp.store();
        let older =
            serde_json::to_vec_pretty(&json!({ "schemaVersion": 0, "items": [] })).expect("json");
        std::fs::write(store.root().join(FIELDS_FILE), older).expect("write");

        let loaded = store.load().expect("load");
        assert!(loaded.data.fields.is_empty());
        assert!(
            loaded
                .warnings
                .iter()
                .any(|w| w == "migrated fields.json from v0 to v1"),
            "warnings: {:?}",
            loaded.warnings
        );
    }

    #[test]
    fn storage_leaves_no_tmp_files_after_save() {
        let tmp = TempDir::new("tmp-files");
        let store = tmp.store();
        store.save(&sample_data(), Dirty::ALL).expect("save");

        for name in dir_entries(store.root()) {
            assert!(!name.ends_with(".tmp"), "leftover temp file: {name}");
        }
    }

    #[test]
    fn storage_backup_daily_creates_and_prunes() {
        let tmp = TempDir::new("daily");
        let store = tmp.store();
        store.save(&sample_data(), Dirty::ALL).expect("save");

        let backups = store.backups_dir();
        for day in 1..=8 {
            std::fs::create_dir_all(backups.join(format!("1999-01-0{day}"))).expect("fake backup");
        }

        store.backup_daily().expect("backup_daily");
        store.backup_daily().expect("second call must be a no-op");

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let today_dir = backups.join(&today);
        assert!(today_dir.is_dir(), "today's backup folder must exist");
        for name in DATA_FILES {
            assert!(
                today_dir.join(name).is_file(),
                "{name} must be copied into today's backup"
            );
        }

        assert!(
            !backups.join("1999-01-01").exists(),
            "oldest backup must be pruned"
        );
        assert!(
            !backups.join("1999-01-02").exists(),
            "second oldest backup must be pruned"
        );
        assert!(backups.join("1999-01-03").exists());

        let remaining = dir_entries(&backups)
            .into_iter()
            .filter(|name| backups.join(name).is_dir())
            .count();
        assert_eq!(remaining, 7, "backups must be pruned to 7");
    }

    #[test]
    fn storage_export_wipe_import_round_trip() {
        let store_tmp = TempDir::new("export-store");
        let export_tmp = TempDir::new("export-dir");
        let store = store_tmp.store();
        let data = sample_data();
        store.save(&data, Dirty::ALL).expect("save");

        store.export_to(export_tmp.path()).expect("export");
        for name in DATA_FILES {
            assert!(
                export_tmp.path().join(name).is_file(),
                "{name} must be exported"
            );
        }

        // Wipe the store: no data files, no backups.
        for entry in std::fs::read_dir(store.root()).expect("read store") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                std::fs::remove_dir_all(&path).expect("wipe dir");
            } else {
                std::fs::remove_file(&path).expect("wipe file");
            }
        }

        store.import_from(export_tmp.path()).expect("import");

        let loaded = store.load().expect("load");
        assert_eq!(snapshot(&loaded.data), snapshot(&data));
    }

    #[test]
    fn storage_cleanup_removes_dangling_references() {
        let mut data = sample_data();
        data.fields.push(FieldDef {
            id: "f-tags".into(),
            label: "Tags".into(),
            ty: FieldType::Multiselect,
            required: false,
            default_value: None,
            list_id: Some("list-status".into()),
            help: None,
            show_in_list: false,
            archived: false,
        });

        let mut task = Task::new("Dangling");
        task.values.insert("f-status".into(), json!("opt-todo"));
        task.values.insert("f-gone".into(), json!("whatever"));
        task.values.insert("f-null".into(), json!(null));
        task.values
            .insert("f-tags".into(), json!(["opt-done", "opt-missing"]));
        data.tasks.push(task);

        // Existing task holding an option id that vanished from the master list.
        data.tasks[0]
            .values
            .insert("f-status".into(), json!("opt-missing"));

        assert!(cleanup_references(&mut data), "first pass must report changes");

        let cleaned = &data.tasks[1];
        assert!(
            !cleaned.values.contains_key("f-gone"),
            "value of a deleted field must go"
        );
        assert_eq!(
            cleaned.values.get("f-null"),
            Some(&json!(null)),
            "null values may stay"
        );
        assert_eq!(cleaned.values.get("f-status"), Some(&json!("opt-todo")));
        assert_eq!(cleaned.values.get("f-tags"), Some(&json!(["opt-done"])));
        assert!(
            !data.tasks[0].values.contains_key("f-status"),
            "missing option id must go"
        );

        assert!(
            !cleanup_references(&mut data),
            "second pass must be a no-op"
        );
    }

    #[test]
    fn storage_import_from_invalid_json_is_rejected() {
        let store_tmp = TempDir::new("import-store");
        let store = store_tmp.store();
        let data = sample_data();
        store.save(&data, Dirty::ALL).expect("save");

        let bad_tmp = TempDir::new("import-bad");
        std::fs::write(bad_tmp.path().join(TASKS_FILE), b"not json").expect("write bad tasks");
        let lists = serde_json::to_vec(&EntityFile::new(data.lists.clone())).expect("json");
        std::fs::write(bad_tmp.path().join(LISTS_FILE), lists).expect("write lists");

        let Err(err) = store.import_from(bad_tmp.path()) else {
            panic!("invalid import must fail");
        };
        assert!(
            err.message().starts_with("import: "),
            "message: {}",
            err.message()
        );

        let empty_tmp = TempDir::new("import-empty");
        let Err(err) = store.import_from(empty_tmp.path()) else {
            panic!("import from an empty folder must fail");
        };
        assert!(err.message().contains("import"), "message: {}", err.message());

        let loaded = store.load().expect("load");
        assert_eq!(
            snapshot(&loaded.data),
            snapshot(&data),
            "current data must be untouched"
        );
    }

    #[test]
    fn storage_load_of_root_without_files_is_default_and_writes_nothing() {
        let tmp = TempDir::new("empty");
        let store = tmp.store();

        let loaded = store.load().expect("load");
        assert!(loaded.warnings.is_empty(), "warnings: {:?}", loaded.warnings);
        assert!(loaded.data.lists.is_empty());
        assert!(loaded.data.fields.is_empty());
        assert!(loaded.data.tasks.is_empty());
        assert_eq!(loaded.data.settings.schema_version, SCHEMA_VERSION);
        assert!(
            dir_entries(store.root()).is_empty(),
            "load must never write anything"
        );
    }

    #[test]
    fn storage_save_with_empty_dirty_is_noop() {
        let tmp = TempDir::new("noop");
        let store = tmp.store();
        std::fs::remove_dir_all(store.root()).expect("clean root first");

        store.save(&sample_data(), Dirty::default()).expect("no-op save");
        assert!(
            !store.root().exists(),
            "a no-op save must not create anything"
        );
    }
}

//! Core data model — the single source of truth for the JSON files on disk.
//!
//! JSON uses camelCase keys except the field type, which is stored as `"type"`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Version written into every data file. Bump this when a migration is added.
pub const SCHEMA_VERSION: u32 = 1;

/// Wrapper for entity files: `{ "schemaVersion": N, "items": [...] }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityFile<T> {
    pub schema_version: u32,
    pub items: Vec<T>,
}

impl<T> EntityFile<T> {
    pub fn new(items: Vec<T>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            items,
        }
    }
}

/// Field type as stored in `fields.json` under `"type"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    Text,
    Longtext,
    Number,
    Date,
    Datetime,
    Checkbox,
    Select,
    Multiselect,
}

impl FieldType {
    /// True for the two types that require a dropdown master.
    pub fn needs_list(self) -> bool {
        matches!(self, FieldType::Select | FieldType::Multiselect)
    }
}

/// One option inside a dropdown master. Order = position in `ListDef::options`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionDef {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub archived: bool,
}

/// A dropdown master. One master can be referenced by many fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListDef {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub options: Vec<OptionDef>,
}

impl ListDef {
    pub fn option(&self, id: &str) -> Option<&OptionDef> {
        self.options.iter().find(|o| o.id == id)
    }
}

/// A user-defined attribute. Order = position in `fields.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldDef {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub ty: FieldType,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(default)]
    pub show_in_list: bool,
    #[serde(default)]
    pub archived: bool,
}

/// A task. `values` maps field id -> JSON value (encoding by field type).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub values: BTreeMap<String, serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
}

impl Task {
    pub fn new(title: impl Into<String>) -> Self {
        let now = now_iso();
        Self {
            id: new_id(),
            title: title.into(),
            description: String::new(),
            values: BTreeMap::new(),
            created_at: now.clone(),
            updated_at: now,
            archived_at: None,
        }
    }

    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WindowState {
    /// Last non-maximized window geometry, in physical pixels, as reported by the
    /// windowing system. Restoring clamps it to the monitor work area.
    pub width: f32,
    pub height: f32,
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: 1500.0,
            height: 950.0,
            x: None,
            y: None,
            maximized: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub schema_version: u32,
    pub theme: Theme,
    pub window: WindowState,
    pub sidebar_collapsed: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            theme: Theme::System,
            window: WindowState::default(),
            sidebar_collapsed: false,
        }
    }
}

/// Everything loaded from disk, kept in memory by the app.
#[derive(Debug, Clone, Default)]
pub struct DataRoot {
    pub lists: Vec<ListDef>,
    pub fields: Vec<FieldDef>,
    pub tasks: Vec<Task>,
    pub settings: Settings,
}

/// Fresh data for a first run: Status, Priority, Due date. Fully editable later.
pub fn seeded_data() -> DataRoot {
    use serde_json::json;

    let status_options = vec![
        OptionDef {
            id: new_id(),
            label: "To Do".into(),
            color: Some("#64748b".into()),
            archived: false,
        },
        OptionDef {
            id: new_id(),
            label: "In Progress".into(),
            color: Some("#f59e0b".into()),
            archived: false,
        },
        OptionDef {
            id: new_id(),
            label: "Done".into(),
            color: Some("#22c55e".into()),
            archived: false,
        },
    ];
    let status = ListDef {
        id: new_id(),
        name: "Status".into(),
        archived: false,
        options: status_options.clone(),
    };

    let priority = ListDef {
        id: new_id(),
        name: "Priority".into(),
        archived: false,
        options: vec![
            OptionDef {
                id: new_id(),
                label: "Low".into(),
                color: Some("#64748b".into()),
                archived: false,
            },
            OptionDef {
                id: new_id(),
                label: "Medium".into(),
                color: Some("#f59e0b".into()),
                archived: false,
            },
            OptionDef {
                id: new_id(),
                label: "High".into(),
                color: Some("#ef4444".into()),
                archived: false,
            },
        ],
    };

    let fields = vec![
        FieldDef {
            id: new_id(),
            label: "Status".into(),
            ty: FieldType::Select,
            required: false,
            default_value: Some(json!(status_options[0].id)),
            list_id: Some(status.id.clone()),
            help: None,
            show_in_list: true,
            archived: false,
        },
        FieldDef {
            id: new_id(),
            label: "Priority".into(),
            ty: FieldType::Select,
            required: false,
            default_value: None,
            list_id: Some(priority.id.clone()),
            help: None,
            show_in_list: true,
            archived: false,
        },
        FieldDef {
            id: new_id(),
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

    DataRoot {
        lists: vec![status, priority],
        fields,
        tasks: Vec::new(),
        settings: Settings::default(),
    }
}

/// New UUID v4 id, used for every entity.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Current UTC timestamp, e.g. `2026-10-19T12:34:56Z`.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Today's local date as `YYYY-MM-DD`.
pub fn today_iso() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_json_uses_camel_case_and_type_key() {
        let field = FieldDef {
            id: "f1".into(),
            label: "Due date".into(),
            ty: FieldType::Date,
            required: false,
            default_value: None,
            list_id: None,
            help: None,
            show_in_list: true,
            archived: false,
        };
        let json = serde_json::to_string(&field).unwrap();
        assert!(json.contains("\"showInList\":true"), "json was {json}");
        assert!(json.contains("\"type\":\"date\""), "json was {json}");
        assert!(!json.contains("\"defaultValue\""), "None must be skipped: {json}");
    }

    #[test]
    fn task_round_trips_with_values() {
        let mut task = Task::new("Buy milk");
        task.values
            .insert("f1".into(), serde_json::json!("2026-10-20"));
        task.values.insert("f2".into(), serde_json::json!(3.5));
        let json = serde_json::to_string(&task).unwrap();
        let back: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(back.title, "Buy milk");
        assert_eq!(back.values.len(), 2);
    }

    #[test]
    fn unknown_keys_are_tolerated() {
        let json = r#"{"id":"t1","title":"x","createdAt":"2026-01-01T00:00:00Z",
            "updatedAt":"2026-01-01T00:00:00Z","futureKey":42}"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert_eq!(task.title, "x");
    }

    #[test]
    fn entity_file_has_schema_version() {
        let file = EntityFile::new(vec![1, 2, 3]);
        let json = serde_json::to_string(&file).unwrap();
        assert_eq!(json, r#"{"schemaVersion":1,"items":[1,2,3]}"#);
    }

    #[test]
    fn seeded_data_is_consistent() {
        let data = seeded_data();
        assert_eq!(data.lists.len(), 2);
        assert_eq!(data.fields.len(), 3);
        let status = &data.lists[0];
        let status_field = &data.fields[0];
        assert_eq!(status_field.list_id.as_deref(), Some(status.id.as_str()));
    }

    #[test]
    fn ids_are_unique() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
    }
}

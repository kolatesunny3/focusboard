//! Field kind mapping and value encoding/validation.
//!
//! CONTRACT (fixed — see agents.md): every public item and signature in this
//! file is a stable API used by other modules. This module is the single place
//! that encodes, decodes, and validates a value.

use crate::model::{FieldDef, FieldType, ListDef};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use serde_json::Value;

pub const KIND_TEXT: i32 = 0;
pub const KIND_LONGTEXT: i32 = 1;
pub const KIND_NUMBER: i32 = 2;
pub const KIND_DATE: i32 = 3;
pub const KIND_DATETIME: i32 = 4;
pub const KIND_CHECKBOX: i32 = 5;
pub const KIND_SELECT: i32 = 6;
pub const KIND_MULTISELECT: i32 = 7;

/// UI kind int for a field type. These ints are never stored in JSON.
pub fn kind_of(ty: FieldType) -> i32 {
    match ty {
        FieldType::Text => KIND_TEXT,
        FieldType::Longtext => KIND_LONGTEXT,
        FieldType::Number => KIND_NUMBER,
        FieldType::Date => KIND_DATE,
        FieldType::Datetime => KIND_DATETIME,
        FieldType::Checkbox => KIND_CHECKBOX,
        FieldType::Select => KIND_SELECT,
        FieldType::Multiselect => KIND_MULTISELECT,
    }
}

/// True for `null`, empty string, or empty array. `false` is NOT empty.
pub fn is_empty_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

/// Shape/type validation of a stored value (tolerant of missing references).
///
/// The shape must match the field type; option ids and list ids are NOT
/// resolved here (dangling references are legal and render as "(missing)").
pub fn validate(field: &FieldDef, value: &Value) -> Result<(), String> {
    if value.is_null() {
        return Ok(());
    }
    match field.ty {
        FieldType::Text | FieldType::Longtext => {
            if value.is_string() {
                Ok(())
            } else {
                Err("expected text".into())
            }
        }
        FieldType::Number => match value.as_f64() {
            Some(f) if f.is_finite() => Ok(()),
            _ => Err("expected a number".into()),
        },
        FieldType::Date => match value.as_str() {
            Some(s) if NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok() => Ok(()),
            _ => Err("expected a YYYY-MM-DD date".into()),
        },
        FieldType::Datetime => match value.as_str() {
            Some(s) if DateTime::parse_from_rfc3339(s).is_ok() => Ok(()),
            _ => Err("expected an ISO 8601 date and time".into()),
        },
        FieldType::Checkbox => {
            if value.is_boolean() {
                Ok(())
            } else {
                Err("expected true or false".into())
            }
        }
        FieldType::Select => {
            if value.is_string() {
                Ok(())
            } else {
                Err("expected an option id".into())
            }
        }
        FieldType::Multiselect => match value.as_array() {
            Some(items) if items.iter().all(Value::is_string) => Ok(()),
            _ => Err("expected a list of option ids".into()),
        },
    }
}

/// Display string for tables and pickers. Resolves option ids via `lists`.
pub fn display(field: &FieldDef, value: &Value, lists: &[ListDef]) -> String {
    if is_empty_value(value) {
        return String::new();
    }
    match field.ty {
        FieldType::Text | FieldType::Longtext => value
            .as_str()
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default(),
        FieldType::Number => format_number(value),
        FieldType::Date => value.as_str().unwrap_or_default().to_string(),
        FieldType::Datetime => match value.as_str() {
            Some(raw) => datetime_to_local(raw),
            None => value.to_string(),
        },
        FieldType::Checkbox => match value.as_bool() {
            Some(true) => "Yes".to_string(),
            Some(false) => "No".to_string(),
            None => String::new(),
        },
        FieldType::Select => match value.as_str() {
            Some(id) => option_label(field, id, lists),
            None => String::new(),
        },
        FieldType::Multiselect => match value.as_array() {
            Some(ids) => ids
                .iter()
                .map(|id| match id.as_str() {
                    Some(s) => option_label(field, s, lists),
                    None => "(missing)".to_string(),
                })
                .collect::<Vec<_>>()
                .join(", "),
            None => String::new(),
        },
    }
}

/// Parse UI input (canonical string) into a JSON value.
pub fn parse_input(field: &FieldDef, s: &str) -> Result<Value, String> {
    match field.ty {
        FieldType::Text | FieldType::Longtext => {
            let t = s.trim_end();
            if t.is_empty() {
                Ok(Value::Null)
            } else {
                Ok(Value::String(t.to_string()))
            }
        }
        FieldType::Number => {
            let t = s.trim();
            if t.is_empty() {
                return Ok(Value::Null);
            }
            match t.parse::<f64>() {
                Ok(f) if f.is_finite() => serde_json::Number::from_f64(f)
                    .map(Value::Number)
                    .ok_or_else(|| "not a number".to_string()),
                _ => Err("not a number".to_string()),
            }
        }
        FieldType::Date => {
            let t = s.trim();
            if t.is_empty() {
                return Ok(Value::Null);
            }
            match NaiveDate::parse_from_str(t, "%Y-%m-%d") {
                Ok(d) => Ok(Value::String(d.format("%Y-%m-%d").to_string())),
                Err(_) => Err("use YYYY-MM-DD".to_string()),
            }
        }
        FieldType::Datetime => {
            let t = s.trim();
            if t.is_empty() {
                return Ok(Value::Null);
            }
            const FORMATS: [&str; 4] = [
                "%Y-%m-%d %H:%M",
                "%Y-%m-%dT%H:%M",
                "%Y-%m-%d %H:%M:%S",
                "%Y-%m-%dT%H:%M:%S",
            ];
            let naive = FORMATS
                .iter()
                .find_map(|fmt| NaiveDateTime::parse_from_str(t, fmt).ok())
                .ok_or_else(|| "use YYYY-MM-DD HH:MM".to_string())?;
            let local = Local
                .from_local_datetime(&naive)
                .earliest()
                .ok_or_else(|| "use YYYY-MM-DD HH:MM".to_string())?;
            Ok(Value::String(
                local
                    .with_timezone(&Utc)
                    .to_rfc3339_opts(SecondsFormat::Secs, true),
            ))
        }
        FieldType::Checkbox => match s.trim().to_ascii_lowercase().as_str() {
            "" | "false" => Ok(Value::Bool(false)),
            "true" => Ok(Value::Bool(true)),
            _ => Err("use true or false".to_string()),
        },
        FieldType::Select => {
            let t = s.trim();
            if t.is_empty() {
                Ok(Value::Null)
            } else {
                Ok(Value::String(t.to_string()))
            }
        }
        FieldType::Multiselect => {
            let mut ids: Vec<String> = Vec::new();
            for part in s.split(',') {
                let t = part.trim();
                if !t.is_empty() && !ids.iter().any(|id| id.as_str() == t) {
                    ids.push(t.to_string());
                }
            }
            Ok(Value::Array(ids.into_iter().map(Value::String).collect()))
        }
    }
}

/// Canonical edit string shown in inputs (inverse of `parse_input`).
pub fn to_input(field: &FieldDef, value: &Value) -> String {
    if value.is_null() {
        return String::new();
    }
    match field.ty {
        FieldType::Text | FieldType::Longtext => value.as_str().unwrap_or_default().to_string(),
        FieldType::Number => format_number(value),
        FieldType::Date => value.as_str().unwrap_or_default().to_string(),
        FieldType::Datetime => match value.as_str() {
            Some(raw) => datetime_to_local(raw),
            None => value.to_string(),
        },
        FieldType::Checkbox => match value.as_bool() {
            Some(b) => b.to_string(),
            None => String::new(),
        },
        FieldType::Select => value.as_str().unwrap_or_default().to_string(),
        FieldType::Multiselect => value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default(),
    }
}

/// Value a new task gets for this field (field default or type zero-value).
pub fn default_for(field: &FieldDef) -> Value {
    if let Some(v) = &field.default_value {
        return v.clone();
    }
    match field.ty {
        FieldType::Text | FieldType::Longtext => Value::String(String::new()),
        FieldType::Number => Value::from(0),
        FieldType::Date => Value::String(crate::model::today_iso()),
        FieldType::Datetime => Value::String(crate::model::now_iso()),
        FieldType::Checkbox => Value::Bool(false),
        FieldType::Select => Value::Null,
        FieldType::Multiselect => Value::Array(Vec::new()),
    }
}

/// Shortest decimal form of a stored number (`3.5`, `4`).
fn format_number(value: &Value) -> String {
    if let Some(i) = value.as_i64() {
        i.to_string()
    } else if let Some(u) = value.as_u64() {
        u.to_string()
    } else if let Some(f) = value.as_f64() {
        format!("{f}")
    } else {
        String::new()
    }
}

/// UTC RFC3339 string -> local `YYYY-MM-DD HH:MM`; raw string if unparseable.
fn datetime_to_local(raw: &str) -> String {
    match DateTime::parse_from_rfc3339(raw) {
        Ok(dt) => dt
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        Err(_) => raw.to_string(),
    }
}

/// Label for one option id via the field's master, tolerant of dangling refs.
fn option_label(field: &FieldDef, option_id: &str, lists: &[ListDef]) -> String {
    let option = field
        .list_id
        .as_deref()
        .and_then(|list_id| lists.iter().find(|l| l.id == list_id))
        .and_then(|list| list.option(option_id));
    match option {
        Some(o) if o.archived => format!("{} (archived)", o.label),
        Some(o) => o.label.clone(),
        None => "(missing)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FieldDef, FieldType, ListDef, OptionDef};
    use serde_json::json;

    fn field(ty: FieldType) -> FieldDef {
        FieldDef {
            id: "f1".into(),
            label: "Test".into(),
            ty,
            required: false,
            default_value: None,
            list_id: None,
            help: None,
            show_in_list: false,
            archived: false,
        }
    }

    fn list_field(ty: FieldType, list_id: &str) -> FieldDef {
        FieldDef {
            list_id: Some(list_id.into()),
            ..field(ty)
        }
    }

    fn lists_fixture() -> Vec<ListDef> {
        vec![ListDef {
            id: "l1".into(),
            name: "Status".into(),
            archived: false,
            options: vec![
                OptionDef {
                    id: "o1".into(),
                    label: "To Do".into(),
                    color: None,
                    archived: false,
                },
                OptionDef {
                    id: "o2".into(),
                    label: "Done".into(),
                    color: None,
                    archived: true,
                },
            ],
        }]
    }

    const ALL_TYPES: [FieldType; 8] = [
        FieldType::Text,
        FieldType::Longtext,
        FieldType::Number,
        FieldType::Date,
        FieldType::Datetime,
        FieldType::Checkbox,
        FieldType::Select,
        FieldType::Multiselect,
    ];

    #[test]
    fn kind_mapping_is_stable() {
        assert_eq!(kind_of(FieldType::Text), 0);
        assert_eq!(kind_of(FieldType::Longtext), 1);
        assert_eq!(kind_of(FieldType::Number), 2);
        assert_eq!(kind_of(FieldType::Date), 3);
        assert_eq!(kind_of(FieldType::Datetime), 4);
        assert_eq!(kind_of(FieldType::Checkbox), 5);
        assert_eq!(kind_of(FieldType::Select), 6);
        assert_eq!(kind_of(FieldType::Multiselect), 7);
    }

    #[test]
    fn empty_value_rules() {
        assert!(is_empty_value(&Value::Null));
        assert!(is_empty_value(&json!("")));
        assert!(is_empty_value(&json!([])));
        assert!(!is_empty_value(&json!(false)));
        assert!(!is_empty_value(&json!(0)));
        assert!(!is_empty_value(&json!("x")));
    }

    #[test]
    fn null_is_valid_for_every_type() {
        for ty in ALL_TYPES {
            assert!(validate(&field(ty), &Value::Null).is_ok(), "{ty:?}");
        }
    }

    #[test]
    fn validate_text_like() {
        assert!(validate(&field(FieldType::Text), &json!("hi")).is_ok());
        assert_eq!(
            validate(&field(FieldType::Text), &json!(5)).unwrap_err(),
            "expected text"
        );
        assert!(validate(&field(FieldType::Longtext), &json!("a\nb")).is_ok());
        assert!(validate(&field(FieldType::Longtext), &json!(true)).is_err());
    }

    #[test]
    fn validate_number_date_datetime() {
        assert!(validate(&field(FieldType::Number), &json!(3.5)).is_ok());
        assert!(validate(&field(FieldType::Number), &json!(7)).is_ok());
        assert_eq!(
            validate(&field(FieldType::Number), &json!("3.5")).unwrap_err(),
            "expected a number"
        );

        assert!(validate(&field(FieldType::Date), &json!("2026-10-19")).is_ok());
        assert_eq!(
            validate(&field(FieldType::Date), &json!("2026-13-01")).unwrap_err(),
            "expected a YYYY-MM-DD date"
        );
        assert_eq!(
            validate(&field(FieldType::Date), &json!(20261019)).unwrap_err(),
            "expected a YYYY-MM-DD date"
        );

        assert!(validate(&field(FieldType::Datetime), &json!("2026-10-19T12:00:00Z")).is_ok());
        assert!(
            validate(
                &field(FieldType::Datetime),
                &json!("2026-10-19T12:00:00+02:00")
            )
            .is_ok()
        );
        assert!(validate(&field(FieldType::Datetime), &json!("2026-10-19 12:00")).is_err());
    }

    #[test]
    fn validate_checkbox_select_multiselect() {
        assert!(validate(&field(FieldType::Checkbox), &json!(true)).is_ok());
        assert!(validate(&field(FieldType::Checkbox), &json!(false)).is_ok());
        assert_eq!(
            validate(&field(FieldType::Checkbox), &json!(1)).unwrap_err(),
            "expected true or false"
        );

        assert!(validate(&field(FieldType::Select), &json!("anything")).is_ok());
        assert_eq!(
            validate(&field(FieldType::Select), &json!(4)).unwrap_err(),
            "expected an option id"
        );

        assert!(validate(&field(FieldType::Multiselect), &json!(["a", "b"])).is_ok());
        assert!(validate(&field(FieldType::Multiselect), &json!([])).is_ok());
        assert_eq!(
            validate(&field(FieldType::Multiselect), &json!(["a", 1])).unwrap_err(),
            "expected a list of option ids"
        );
        assert!(validate(&field(FieldType::Multiselect), &json!("a")).is_err());
    }

    #[test]
    fn round_trip_parse_input_to_input() {
        let cases: Vec<(FieldDef, &str)> = vec![
            (field(FieldType::Text), "hello world"),
            (field(FieldType::Longtext), "line one\nline two"),
            (field(FieldType::Number), "3.5"),
            (field(FieldType::Number), "-0.25"),
            (field(FieldType::Date), "2026-10-19"),
            // Datetime T-forms and seconds forms are covered by
            // `datetime_local_round_trip_and_storage`; they canonicalize.
            (field(FieldType::Datetime), "2026-06-15 12:30"),
            (field(FieldType::Checkbox), "true"),
            (field(FieldType::Checkbox), "false"),
            (field(FieldType::Select), "opt-1"),
            (field(FieldType::Multiselect), "a,b"),
        ];
        for (f, input) in cases {
            let value = parse_input(&f, input).unwrap_or_else(|e| panic!("{input:?}: {e}"));
            assert_eq!(to_input(&f, &value), input, "{input:?}");
        }
    }

    #[test]
    fn empty_input_becomes_null_or_zero() {
        assert_eq!(
            parse_input(&field(FieldType::Text), "   ").unwrap(),
            Value::Null
        );
        assert_eq!(
            parse_input(&field(FieldType::Longtext), "").unwrap(),
            Value::Null
        );
        assert_eq!(
            parse_input(&field(FieldType::Number), "  ").unwrap(),
            Value::Null
        );
        assert_eq!(
            parse_input(&field(FieldType::Date), "").unwrap(),
            Value::Null
        );
        assert_eq!(
            parse_input(&field(FieldType::Datetime), "").unwrap(),
            Value::Null
        );
        assert_eq!(
            parse_input(&field(FieldType::Select), " ").unwrap(),
            Value::Null
        );
        assert_eq!(
            parse_input(&field(FieldType::Checkbox), "").unwrap(),
            json!(false)
        );
        assert_eq!(
            parse_input(&field(FieldType::Multiselect), "").unwrap(),
            json!([])
        );

        assert_eq!(to_input(&field(FieldType::Text), &Value::Null), "");
        assert_eq!(to_input(&field(FieldType::Select), &Value::Null), "");
    }

    #[test]
    fn parse_input_rejects_invalid() {
        assert_eq!(
            parse_input(&field(FieldType::Number), "abc").unwrap_err(),
            "not a number"
        );
        assert_eq!(
            parse_input(&field(FieldType::Number), "NaN").unwrap_err(),
            "not a number"
        );
        assert_eq!(
            parse_input(&field(FieldType::Date), "19/10/2026").unwrap_err(),
            "use YYYY-MM-DD"
        );
        assert_eq!(
            parse_input(&field(FieldType::Datetime), "yesterday").unwrap_err(),
            "use YYYY-MM-DD HH:MM"
        );
        assert!(parse_input(&field(FieldType::Checkbox), "maybe").is_err());
    }

    #[test]
    fn datetime_local_round_trip_and_storage() {
        let f = field(FieldType::Datetime);
        for input in [
            "2026-06-15 12:30",
            "2026-06-15T12:30",
            "2026-06-15 12:30:45",
            "2026-06-15T12:30:45",
        ] {
            let value = parse_input(&f, input).unwrap_or_else(|e| panic!("{input:?}: {e}"));
            let stored = value.as_str().expect("stored as string");
            assert!(stored.ends_with('Z'), "stored {stored} must be UTC");
            assert!(DateTime::parse_from_rfc3339(stored).is_ok(), "{stored}");
            assert_eq!(to_input(&f, &value), "2026-06-15 12:30", "{input:?}");
            assert_eq!(display(&f, &value, &[]), "2026-06-15 12:30", "{input:?}");
        }
    }

    #[test]
    fn display_datetime_uses_local_time() {
        let f = field(FieldType::Datetime);
        let utc = "2026-06-15T10:30:00Z";
        let expected = DateTime::parse_from_rfc3339(utc)
            .unwrap()
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string();
        assert_eq!(display(&f, &json!(utc), &[]), expected);
        assert_eq!(to_input(&f, &json!(utc)), expected);
        assert_eq!(display(&f, &json!("not a date"), &[]), "not a date");
        assert_eq!(to_input(&f, &json!("not a date")), "not a date");
    }

    #[test]
    fn numbers_print_without_trailing_zeros() {
        let f = field(FieldType::Number);
        assert_eq!(display(&f, &json!(3.5), &[]), "3.5");
        assert_eq!(display(&f, &json!(4.0), &[]), "4");
        assert_eq!(display(&f, &json!(7), &[]), "7");
        assert_eq!(display(&f, &json!(-0.25), &[]), "-0.25");
        assert_eq!(to_input(&f, &json!(4.0)), "4");
        assert_eq!(to_input(&f, &json!(3.5)), "3.5");
    }

    #[test]
    fn text_display_collapses_whitespace() {
        let f = field(FieldType::Longtext);
        assert_eq!(display(&f, &json!("a\nb"), &[]), "a b");
        assert_eq!(display(&f, &json!("a\tb"), &[]), "a b");
        assert_eq!(display(&f, &json!("a\n\n\nb"), &[]), "a b");
        assert_eq!(display(&f, &Value::Null, &[]), "");
        assert_eq!(display(&f, &json!(""), &[]), "");
    }

    #[test]
    fn checkbox_display_is_yes_no() {
        let f = field(FieldType::Checkbox);
        assert_eq!(display(&f, &json!(true), &[]), "Yes");
        assert_eq!(display(&f, &json!(false), &[]), "No");
        assert_eq!(to_input(&f, &json!(true)), "true");
        assert_eq!(to_input(&f, &json!(false)), "false");
    }

    #[test]
    fn select_display_resolves_labels() {
        let f = list_field(FieldType::Select, "l1");
        let lists = lists_fixture();
        assert_eq!(display(&f, &json!("o1"), &lists), "To Do");
        assert_eq!(display(&f, &json!("o2"), &lists), "Done (archived)");
        assert_eq!(display(&f, &json!("nope"), &lists), "(missing)");
        assert_eq!(display(&f, &Value::Null, &lists), "");
        assert_eq!(display(&f, &json!("o1"), &[]), "(missing)");
        assert_eq!(
            display(&field(FieldType::Select), &json!("o1"), &lists),
            "(missing)"
        );
    }

    #[test]
    fn multiselect_display_joins_labels_in_order() {
        let f = list_field(FieldType::Multiselect, "l1");
        let lists = lists_fixture();
        assert_eq!(
            display(&f, &json!(["o2", "o1", "zz"]), &lists),
            "Done (archived), To Do, (missing)"
        );
        assert_eq!(display(&f, &json!(["o1"]), &lists), "To Do");
        assert_eq!(display(&f, &json!([]), &lists), "");
    }

    #[test]
    fn multiselect_parse_dedupes_keeping_order() {
        let f = field(FieldType::Multiselect);
        assert_eq!(
            parse_input(&f, "a, b, a , c,,b").unwrap(),
            json!(["a", "b", "c"])
        );
        assert_eq!(to_input(&f, &json!(["a", "b"])), "a,b");
        assert_eq!(to_input(&f, &json!([])), "");
    }

    #[test]
    fn defaults_per_type_and_override() {
        assert_eq!(default_for(&field(FieldType::Text)), json!(""));
        assert_eq!(default_for(&field(FieldType::Longtext)), json!(""));
        assert_eq!(default_for(&field(FieldType::Number)), json!(0));
        assert_eq!(
            default_for(&field(FieldType::Date)),
            json!(crate::model::today_iso())
        );
        let dt = default_for(&field(FieldType::Datetime));
        let s = dt.as_str().expect("datetime default is a string");
        assert!(s.ends_with('Z'), "default {s} must be UTC");
        assert!(DateTime::parse_from_rfc3339(s).is_ok(), "{s}");
        assert_eq!(default_for(&field(FieldType::Checkbox)), json!(false));
        assert_eq!(default_for(&field(FieldType::Select)), Value::Null);
        assert_eq!(default_for(&field(FieldType::Multiselect)), json!([]));

        let mut f = field(FieldType::Number);
        f.default_value = Some(json!(42));
        assert_eq!(default_for(&f), json!(42));
    }
}

//! List logic: columns, sorting, search, filters.
//!
//! CONTRACT (fixed — see agents.md): keep every public item and signature below.
//!
//! Value encoding per field type (features.md): text/longtext → JSON string;
//! number → JSON number; date → `YYYY-MM-DD`; datetime → RFC3339 UTC; checkbox
//! → bool; select → option id; multiselect → array of option ids. A missing key
//! in `task.values` or `null` means "empty".

use crate::model::{FieldDef, FieldType, ListDef, Task};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortField {
    /// Hand-arranged order (rows dragged in the grid). This is the default.
    Manual,
    Title,
    Created,
    Updated,
    Custom(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterOp {
    Contains,
    Equals,
    Before,
    After,
    AnyOf,
    HasOption,
    IsTrue,
    IsFalse,
    Empty,
    NotEmpty,
}

#[derive(Debug, Clone)]
pub struct Filter {
    pub field_id: String,
    pub op: FilterOp,
    pub text: String,
    pub options: Vec<String>,
}

/// Custom columns shown in the list view are capped at 5 (features.md).
const MAX_COLUMNS: usize = 5;

/// Fields that become table columns: not archived, `show_in_list`, max 5.
pub fn visible_columns(fields: &[FieldDef]) -> Vec<&FieldDef> {
    fields
        .iter()
        .filter(|f| !f.archived && f.show_in_list)
        .take(MAX_COLUMNS)
        .collect()
}

/// Sort in place; missing values last; ties broken by title (case-insensitive).
pub fn sort_tasks(
    tasks: &mut [Task],
    by: &SortField,
    dir: SortDir,
    fields: &[FieldDef],
    lists: &[ListDef],
) {
    // Manual order = the order the tasks are stored in, left untouched.
    if *by == SortField::Manual {
        return;
    }

    let field = match by {
        SortField::Custom(id) => fields.iter().find(|f| f.id == *id),
        _ => None,
    };

    // Option id -> position in the master list, used to sort select values.
    let select_positions: HashMap<&str, usize> = field
        .and_then(|f| f.list_id.as_deref())
        .and_then(|list_id| lists.iter().find(|l| l.id == list_id))
        .map(|list| {
            list.options
                .iter()
                .enumerate()
                .map(|(i, option)| (option.id.as_str(), i))
                .collect()
        })
        .unwrap_or_default();

    tasks.sort_by(|a, b| {
        let primary = match by {
            // Manual returns early above; this arm only keeps the match total.
            SortField::Manual => Ordering::Equal,
            SortField::Title => apply_dir(cmp_ci(&a.title, &b.title), dir),
            SortField::Created => apply_dir(a.created_at.cmp(&b.created_at), dir),
            SortField::Updated => apply_dir(a.updated_at.cmp(&b.updated_at), dir),
            // Unknown field id falls back to sorting by title.
            SortField::Custom(_) => match field {
                None => apply_dir(cmp_ci(&a.title, &b.title), dir),
                Some(f) => cmp_custom(a, b, f, dir, &select_positions),
            },
        };
        primary
            .then_with(|| cmp_ci(&a.title, &b.title))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// New order of `ids` after dragging the item at `from` onto the slot at `to`.
/// `to` is clamped, an out-of-range `from` changes nothing.
pub fn reorder(ids: &[String], from: usize, to: usize) -> Vec<String> {
    let mut order = ids.to_vec();
    if order.is_empty() || from >= order.len() {
        return order;
    }
    let moved = order.remove(from);
    order.insert(to.min(order.len()), moved);
    order
}

/// Give the tasks named in `order` those slots, in that order.
///
/// `order` lists the new order of the rows the user sees (a subset of `tasks`:
/// the rest is hidden by a search or a filter). Each listed task is put in a slot
/// that a listed task occupies today, so unlisted tasks hold their place.
pub fn apply_order(tasks: &mut Vec<Task>, order: &[String]) {
    let slots: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
    let ids: HashSet<&str> = slots.iter().map(String::as_str).collect();
    // Only ids that really exist can change slot.
    let listed: HashSet<&str> = order
        .iter()
        .map(String::as_str)
        .filter(|id| ids.contains(id))
        .collect();
    let mut pool: HashMap<String, Task> = std::mem::take(tasks)
        .into_iter()
        .map(|t| (t.id.clone(), t))
        .collect();

    // The caller passes the ids of the rows on screen, so both sides hold the same
    // ids and every listed slot can take the next queued id.
    debug_assert_eq!(
        slots
            .iter()
            .filter(|id| listed.contains(id.as_str()))
            .count(),
        listed.len()
    );

    let mut incoming = order.iter();
    for id in slots {
        let key = if listed.contains(id.as_str()) {
            incoming.next().map(String::as_str).unwrap_or(id.as_str())
        } else {
            id.as_str()
        };
        // The fallback keeps a task alive if `order` ever names an id twice.
        if let Some(task) = pool.remove(key).or_else(|| pool.remove(id.as_str())) {
            tasks.push(task);
        }
    }
}

/// Case-insensitive substring search over title + description.
pub fn matches_search(task: &Task, query: &str) -> bool {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    task.title.to_lowercase().contains(&needle)
        || task.description.to_lowercase().contains(&needle)
}

/// All filters must match. Unknown field ids are ignored.
pub fn matches_filters(
    task: &Task,
    filters: &[Filter],
    fields: &[FieldDef],
    lists: &[ListDef],
) -> bool {
    filters
        .iter()
        .all(|filter| filter_matches(task, filter, fields, lists))
}

/// One filter predicate. `lists` is part of the callsite contract, but no
/// current operator needs the masters (options come from the filter itself).
fn filter_matches(task: &Task, filter: &Filter, fields: &[FieldDef], _lists: &[ListDef]) -> bool {
    let Some(field) = fields.iter().find(|f| f.id == filter.field_id) else {
        return true; // unknown field: the filter is ignored
    };
    let value = task.values.get(&filter.field_id);
    match filter.op {
        FilterOp::Contains => match field.ty {
            FieldType::Text | FieldType::Longtext => {
                contains_ci(opt_str(value).unwrap_or(""), &filter.text)
            }
            FieldType::Number => match opt_num(value) {
                Some(number) => contains_ci(&number_string(number), &filter.text),
                None => false,
            },
            _ => true,
        },
        FilterOp::Equals => match field.ty {
            FieldType::Text | FieldType::Longtext => equals_ci(opt_str(value).unwrap_or(""), &filter.text),
            FieldType::Number => match filter.text.trim().parse::<f64>() {
                Ok(target) => opt_num(value).map(|n| n == target).unwrap_or(false),
                Err(_) => true, // unparsable text: the filter is ignored
            },
            _ => true,
        },
        FilterOp::Before | FilterOp::After => match field.ty {
            FieldType::Date | FieldType::Datetime => {
                if !valid_iso_text(field.ty, &filter.text) {
                    return true; // unparsable text: the filter is ignored
                }
                match opt_str(value) {
                    Some(stored) => {
                        let ord = stored.cmp(filter.text.as_str());
                        if filter.op == FilterOp::Before {
                            ord == Ordering::Less
                        } else {
                            ord == Ordering::Greater
                        }
                    }
                    None => false, // no date is neither before nor after
                }
            }
            _ => true,
        },
        FilterOp::AnyOf => match field.ty {
            FieldType::Select => opt_str(value)
                .map(|id| filter.options.iter().any(|o| o == id))
                .unwrap_or(false),
            _ => true,
        },
        FilterOp::HasOption => match field.ty {
            FieldType::Multiselect => match value {
                Some(Value::Array(items)) => items.iter().any(|item| {
                    item.as_str()
                        .map(|id| filter.options.iter().any(|o| o == id))
                        .unwrap_or(false)
                }),
                _ => false,
            },
            _ => true,
        },
        FilterOp::IsTrue => match field.ty {
            FieldType::Checkbox => opt_bool(value) == Some(true),
            _ => true,
        },
        FilterOp::IsFalse => match field.ty {
            FieldType::Checkbox => opt_bool(value) == Some(false),
            _ => true,
        },
        FilterOp::Empty => is_empty_value(value),
        FilterOp::NotEmpty => !is_empty_value(value),
    }
}

/// Compare one custom-field value pair by field type.
fn cmp_custom(
    a: &Task,
    b: &Task,
    field: &FieldDef,
    dir: SortDir,
    select_positions: &HashMap<&str, usize>,
) -> Ordering {
    let av = a.values.get(&field.id);
    let bv = b.values.get(&field.id);
    match field.ty {
        FieldType::Text | FieldType::Longtext => cmp_opt(
            opt_str(av).map(str::to_lowercase),
            opt_str(bv).map(str::to_lowercase),
            dir,
        ),
        FieldType::Number => cmp_opt(opt_num(av), opt_num(bv), dir),
        FieldType::Date | FieldType::Datetime => cmp_opt(opt_str(av), opt_str(bv), dir),
        FieldType::Select => cmp_opt(
            opt_str(av).and_then(|id| select_positions.get(id).copied()),
            opt_str(bv).and_then(|id| select_positions.get(id).copied()),
            dir,
        ),
        FieldType::Checkbox => cmp_opt(opt_bool(av), opt_bool(bv), dir),
        FieldType::Multiselect => cmp_opt(opt_arr_len(av), opt_arr_len(bv), dir),
    }
}

/// Two present values follow `dir`; an empty value always sorts last.
fn cmp_opt<T: PartialOrd>(a: Option<T>, b: Option<T>, dir: SortDir) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => {
            apply_dir(a.partial_cmp(&b).unwrap_or(Ordering::Equal), dir)
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn apply_dir(ord: Ordering, dir: SortDir) -> Ordering {
    match dir {
        SortDir::Asc => ord,
        SortDir::Desc => ord.reverse(),
    }
}

/// Case-insensitive string comparison.
fn cmp_ci(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

fn equals_ci(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// String form of a number as stored (used by `Contains` on number fields).
fn number_string(number: f64) -> String {
    if number.fract() == 0.0 && number.abs() < 1e15 {
        format!("{}", number as i64)
    } else {
        format!("{number}")
    }
}

/// Present string value; missing, `null` and `""` count as empty.
fn opt_str(value: Option<&Value>) -> Option<&str> {
    match value {
        Some(Value::String(s)) if !s.is_empty() => Some(s),
        _ => None,
    }
}

/// Present number value; missing, `null` and unparsable text count as empty.
fn opt_num(value: Option<&Value>) -> Option<f64> {
    let number = match value {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    number.filter(|n| !n.is_nan())
}

/// Present checkbox value; missing and `null` count as empty.
fn opt_bool(value: Option<&Value>) -> Option<bool> {
    match value {
        Some(Value::Bool(b)) => Some(*b),
        _ => None,
    }
}

/// Number of selected options; missing, `null` and `[]` count as empty.
fn opt_arr_len(value: Option<&Value>) -> Option<usize> {
    match value {
        Some(Value::Array(items)) if !items.is_empty() => Some(items.len()),
        _ => None,
    }
}

/// No value at all, `null`, empty string or empty array.
fn is_empty_value(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        Some(Value::Array(items)) => items.is_empty(),
        _ => false,
    }
}

/// Tolerant check that filter text looks like the ISO form the type stores.
fn valid_iso_text(ty: FieldType, text: &str) -> bool {
    match ty {
        FieldType::Date => chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok(),
        FieldType::Datetime => chrono::DateTime::parse_from_rfc3339(text).is_ok(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OptionDef;
    use serde_json::json;
    use std::collections::BTreeMap;

    // ---------- fixtures ----------

    fn task(id: &str, title: &str) -> Task {
        Task {
            id: id.to_string(),
            title: title.to_string(),
            description: String::new(),
            values: BTreeMap::new(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            archived_at: None,
        }
    }

    fn set(task: &mut Task, field_id: &str, value: Value) {
        task.values.insert(field_id.to_string(), value);
    }

    fn with_value(mut task: Task, field_id: &str, value: Value) -> Task {
        set(&mut task, field_id, value);
        task
    }

    fn field(id: &str, ty: FieldType) -> FieldDef {
        FieldDef {
            id: id.to_string(),
            label: id.to_string(),
            ty,
            required: false,
            default_value: None,
            list_id: None,
            help: None,
            show_in_list: true,
            archived: false,
        }
    }

    fn list(id: &str, options: &[&str]) -> ListDef {
        ListDef {
            id: id.to_string(),
            name: id.to_string(),
            archived: false,
            options: options
                .iter()
                .map(|oid| OptionDef {
                    id: (*oid).to_string(),
                    label: (*oid).to_string(),
                    color: None,
                    archived: false,
                })
                .collect(),
        }
    }

    fn filter(field_id: &str, op: FilterOp, text: &str) -> Filter {
        Filter {
            field_id: field_id.to_string(),
            op,
            text: text.to_string(),
            options: Vec::new(),
        }
    }

    fn filter_opts(field_id: &str, op: FilterOp, options: &[&str]) -> Filter {
        Filter {
            field_id: field_id.to_string(),
            op,
            text: String::new(),
            options: options.iter().map(|o| (*o).to_string()).collect(),
        }
    }

    fn titles(tasks: &[Task]) -> Vec<&str> {
        tasks.iter().map(|t| t.title.as_str()).collect()
    }

    fn custom(id: &str) -> SortField {
        SortField::Custom(id.to_string())
    }

    // ---------- visible_columns ----------

    #[test]
    fn visible_columns_skips_archived_and_hidden_in_order() {
        let mut archived = field("b", FieldType::Text);
        archived.archived = true;
        let mut hidden = field("c", FieldType::Text);
        hidden.show_in_list = false;
        let fields = vec![
            field("a", FieldType::Text),
            archived,
            hidden,
            field("d", FieldType::Text),
        ];
        let visible: Vec<&str> = visible_columns(&fields).iter().map(|f| f.id.as_str()).collect();
        assert_eq!(visible, vec!["a", "d"]);
    }

    #[test]
    fn visible_columns_caps_at_five() {
        let fields: Vec<FieldDef> = (0..7)
            .map(|i| field(&format!("f{i}"), FieldType::Text))
            .collect();
        let visible = visible_columns(&fields);
        assert_eq!(visible.len(), 5);
        assert_eq!(visible[4].id, "f4");
    }

    // ---------- manual order (drag & drop) ----------

    #[test]
    fn manual_sort_keeps_the_stored_order() {
        let mut tasks = vec![task("1", "zebra"), task("2", "apple"), task("3", "mango")];
        sort_tasks(&mut tasks, &SortField::Manual, SortDir::Asc, &[], &[]);
        assert_eq!(titles(&tasks), vec!["zebra", "apple", "mango"]);
        sort_tasks(&mut tasks, &SortField::Manual, SortDir::Desc, &[], &[]);
        assert_eq!(titles(&tasks), vec!["zebra", "apple", "mango"]);
    }

    #[test]
    fn reorder_moves_an_id_and_clamps_the_target() {
        let ids = ["a", "b", "c", "d"];
        let list: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        let expect = |v: &[&str]| -> Vec<String> { v.iter().map(|s| s.to_string()).collect() };
        assert_eq!(reorder(&list, 0, 2), expect(&["b", "c", "a", "d"]));
        assert_eq!(reorder(&list, 3, 0), expect(&["d", "a", "b", "c"]));
        assert_eq!(reorder(&list, 0, 9), expect(&["b", "c", "d", "a"]));
        assert_eq!(reorder(&list, 9, 0), expect(&["a", "b", "c", "d"]));
        assert_eq!(reorder(&list, 1, 1), expect(&["a", "b", "c", "d"]));
        assert!(reorder(&[], 0, 0).is_empty());
    }

    #[test]
    fn apply_order_keeps_unlisted_tasks_in_their_slots() {
        // b and d are hidden by a filter, so only a, c and e move around.
        let mut tasks = vec![
            task("a", "A"),
            task("b", "B"),
            task("c", "C"),
            task("d", "D"),
            task("e", "E"),
        ];
        let order: Vec<String> = ["c", "e", "a"].iter().map(|s| s.to_string()).collect();
        apply_order(&mut tasks, &order);
        let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["c", "b", "e", "d", "a"]);
    }

    #[test]
    fn apply_order_never_loses_a_task() {
        let mut tasks = vec![task("a", "A"), task("b", "B")];
        // An id that is not in the list at all leaves everything alone.
        apply_order(&mut tasks, &["zz".to_string()]);
        assert_eq!(titles(&tasks), vec!["A", "B"]);
        // A repeated id must not make the task it names disappear.
        apply_order(&mut tasks, &["b".to_string(), "b".to_string()]);
        assert_eq!(tasks.len(), 2);
        assert_eq!(titles(&tasks), vec!["A", "B"]);
    }

    // ---------- sorting: built-ins ----------

    #[test]
    fn sort_by_title_is_case_insensitive_both_directions() {
        let mut tasks = vec![task("1", "banana"), task("2", "Apple"), task("3", "cherry")];
        sort_tasks(&mut tasks, &SortField::Title, SortDir::Asc, &[], &[]);
        assert_eq!(titles(&tasks), vec!["Apple", "banana", "cherry"]);
        sort_tasks(&mut tasks, &SortField::Title, SortDir::Desc, &[], &[]);
        assert_eq!(titles(&tasks), vec!["cherry", "banana", "Apple"]);
    }

    #[test]
    fn sort_by_created_and_updated_uses_iso_string_order() {
        let mk = |id: &str, created: &str, updated: &str| {
            let mut t = task(id, id);
            t.created_at = created.to_string();
            t.updated_at = updated.to_string();
            t
        };
        let mut tasks = vec![
            mk("a", "2026-01-02T00:00:00Z", "2026-03-01T00:00:00Z"),
            mk("b", "2026-01-03T00:00:00Z", "2026-01-01T00:00:00Z"),
            mk("c", "2026-01-01T00:00:00Z", "2026-02-01T00:00:00Z"),
        ];
        sort_tasks(&mut tasks, &SortField::Created, SortDir::Asc, &[], &[]);
        assert_eq!(titles(&tasks), vec!["c", "a", "b"]);
        sort_tasks(&mut tasks, &SortField::Created, SortDir::Desc, &[], &[]);
        assert_eq!(titles(&tasks), vec!["b", "a", "c"]);
        sort_tasks(&mut tasks, &SortField::Updated, SortDir::Asc, &[], &[]);
        assert_eq!(titles(&tasks), vec!["b", "c", "a"]);
        sort_tasks(&mut tasks, &SortField::Updated, SortDir::Desc, &[], &[]);
        assert_eq!(titles(&tasks), vec!["a", "c", "b"]);
    }

    #[test]
    fn sort_unknown_custom_field_falls_back_to_title() {
        let mut tasks = vec![task("1", "banana"), task("2", "Apple")];
        sort_tasks(&mut tasks, &custom("nope"), SortDir::Asc, &[], &[]);
        assert_eq!(titles(&tasks), vec!["Apple", "banana"]);
        sort_tasks(&mut tasks, &custom("nope"), SortDir::Desc, &[], &[]);
        assert_eq!(titles(&tasks), vec!["banana", "Apple"]);
    }

    // ---------- sorting: custom field types (same data set, both directions) ----------

    #[test]
    fn sort_text_asc_desc_with_empty_values_last() {
        let fields = vec![field("f", FieldType::Text)];
        let mut tasks = vec![
            with_value(task("a", "Banana"), "f", json!("banana")),
            with_value(task("b", "Apple"), "f", json!("apple")),
            task("d", "Task D"), // missing key
            with_value(task("c", "Task C"), "f", json!("")), // empty string
            with_value(task("e", "Cherry"), "f", json!("Cherry")),
        ];
        sort_tasks(&mut tasks, &custom("f"), SortDir::Asc, &fields, &[]);
        assert_eq!(titles(&tasks), vec!["Apple", "Banana", "Cherry", "Task C", "Task D"]);
        sort_tasks(&mut tasks, &custom("f"), SortDir::Desc, &fields, &[]);
        assert_eq!(titles(&tasks), vec!["Cherry", "Banana", "Apple", "Task C", "Task D"]);
    }

    #[test]
    fn sort_number_asc_desc_with_missing_last() {
        let fields = vec![field("n", FieldType::Number)];
        let mut tasks = vec![
            with_value(task("a", "Ten"), "n", json!(10)),
            with_value(task("b", "Two"), "n", json!(2)),
            with_value(task("c", "Missing"), "n", json!(null)),
            with_value(task("d", "Fraction"), "n", json!(3.5)),
        ];
        sort_tasks(&mut tasks, &custom("n"), SortDir::Asc, &fields, &[]);
        assert_eq!(titles(&tasks), vec!["Two", "Fraction", "Ten", "Missing"]);
        sort_tasks(&mut tasks, &custom("n"), SortDir::Desc, &fields, &[]);
        assert_eq!(titles(&tasks), vec!["Ten", "Fraction", "Two", "Missing"]);
    }

    #[test]
    fn sort_date_and_datetime_asc_desc_with_missing_last() {
        let fields = vec![field("d", FieldType::Date), field("dt", FieldType::Datetime)];
        let mut tasks = vec![
            with_value(
                with_value(task("a", "January"), "d", json!("2026-01-15")),
                "dt",
                json!("2026-01-15T09:00:00Z"),
            ),
            with_value(
                with_value(task("b", "December"), "d", json!("2025-12-31")),
                "dt",
                json!("2025-12-31T08:00:00Z"),
            ),
            task("c", "None"),
        ];
        sort_tasks(&mut tasks, &custom("d"), SortDir::Asc, &fields, &[]);
        assert_eq!(titles(&tasks), vec!["December", "January", "None"]);
        sort_tasks(&mut tasks, &custom("d"), SortDir::Desc, &fields, &[]);
        assert_eq!(titles(&tasks), vec!["January", "December", "None"]);
        sort_tasks(&mut tasks, &custom("dt"), SortDir::Asc, &fields, &[]);
        assert_eq!(titles(&tasks), vec!["December", "January", "None"]);
        sort_tasks(&mut tasks, &custom("dt"), SortDir::Desc, &fields, &[]);
        assert_eq!(titles(&tasks), vec!["January", "December", "None"]);
    }

    #[test]
    fn sort_select_follows_master_option_order_unknown_and_empty_last() {
        let lists = vec![list("l", &["o1", "o2", "o3"])];
        let mut select = field("s", FieldType::Select);
        select.list_id = Some("l".to_string());
        let fields = vec![select];
        let mut tasks = vec![
            with_value(task("a", "High"), "s", json!("o3")),
            with_value(task("b", "Low"), "s", json!("o1")),
            with_value(task("c", "Unknown"), "s", json!("nope")),
            with_value(task("d", "Missing"), "s", json!(null)),
            with_value(task("e", "Medium"), "s", json!("o2")),
        ];
        sort_tasks(&mut tasks, &custom("s"), SortDir::Asc, &fields, &lists);
        assert_eq!(
            titles(&tasks),
            vec!["Low", "Medium", "High", "Missing", "Unknown"]
        );
        sort_tasks(&mut tasks, &custom("s"), SortDir::Desc, &fields, &lists);
        assert_eq!(
            titles(&tasks),
            vec!["High", "Medium", "Low", "Missing", "Unknown"]
        );
    }

    #[test]
    fn sort_checkbox_false_before_true_missing_last() {
        let fields = vec![field("cb", FieldType::Checkbox)];
        let mut tasks = vec![
            with_value(task("a", "On"), "cb", json!(true)),
            with_value(task("b", "Off"), "cb", json!(false)),
            task("c", "Missing"),
        ];
        sort_tasks(&mut tasks, &custom("cb"), SortDir::Asc, &fields, &[]);
        assert_eq!(titles(&tasks), vec!["Off", "On", "Missing"]);
        sort_tasks(&mut tasks, &custom("cb"), SortDir::Desc, &fields, &[]);
        assert_eq!(titles(&tasks), vec!["On", "Off", "Missing"]);
    }

    #[test]
    fn sort_multiselect_by_selected_count_empty_last() {
        let fields = vec![field("ms", FieldType::Multiselect)];
        let mut tasks = vec![
            with_value(task("a", "One"), "ms", json!(["x"])),
            with_value(task("c", "NoneSelected"), "ms", json!([])),
            with_value(task("b", "Three"), "ms", json!(["x", "y", "z"])),
            task("d", "Missing"),
        ];
        sort_tasks(&mut tasks, &custom("ms"), SortDir::Asc, &fields, &[]);
        assert_eq!(
            titles(&tasks),
            vec!["One", "Three", "Missing", "NoneSelected"]
        );
        sort_tasks(&mut tasks, &custom("ms"), SortDir::Desc, &fields, &[]);
        assert_eq!(
            titles(&tasks),
            vec!["Three", "One", "Missing", "NoneSelected"]
        );
    }

    #[test]
    fn equal_values_tie_break_by_title_then_id_in_both_directions() {
        let fields = vec![field("n", FieldType::Number)];
        let mut tasks = vec![
            with_value(task("a", "Beta"), "n", json!(5)),
            with_value(task("b", "Alpha"), "n", json!(5)),
            with_value(task("y", "alpha"), "n", json!(5)),
            with_value(task("z", "Bravo"), "n", json!(5)),
        ];
        // Title decides before id; id decides between case-insensitive equals.
        sort_tasks(&mut tasks, &custom("n"), SortDir::Asc, &fields, &[]);
        assert_eq!(titles(&tasks), vec!["Alpha", "alpha", "Beta", "Bravo"]);
        sort_tasks(&mut tasks, &custom("n"), SortDir::Desc, &fields, &[]);
        assert_eq!(titles(&tasks), vec!["Alpha", "alpha", "Beta", "Bravo"]);
    }

    // ---------- search ----------

    #[test]
    fn search_is_case_insensitive_over_title_and_description() {
        let mut t = task("1", "Buy Milk");
        t.description = "From the corner shop".to_string();
        assert!(matches_search(&t, ""));
        assert!(matches_search(&t, "   "));
        assert!(matches_search(&t, "milk"));
        assert!(matches_search(&t, "BUY"));
        assert!(matches_search(&t, "corner"));
        assert!(matches_search(&t, "shop"));
        assert!(!matches_search(&t, "bread"));
        assert!(!matches_search(&t, "milk shop")); // not contiguous
    }

    // ---------- filters ----------

    fn filter_fields() -> Vec<FieldDef> {
        let mut select = field("sel", FieldType::Select);
        select.list_id = Some("l_sel".to_string());
        let mut multiselect = field("ms", FieldType::Multiselect);
        multiselect.list_id = Some("l_ms".to_string());
        vec![
            field("txt", FieldType::Text),
            field("long", FieldType::Longtext),
            field("num", FieldType::Number),
            field("date", FieldType::Date),
            field("dt", FieldType::Datetime),
            field("cb", FieldType::Checkbox),
            select,
            multiselect,
        ]
    }

    fn filter_lists() -> Vec<ListDef> {
        vec![
            list("l_sel", &["s1", "s2", "s3"]),
            list("l_ms", &["s1", "s2", "s3"]),
        ]
    }

    fn populated_task() -> Task {
        let mut t = task("populated", "Write report");
        set(&mut t, "txt", json!("Hello World"));
        set(&mut t, "long", json!("Long body text"));
        set(&mut t, "num", json!(42.5));
        set(&mut t, "date", json!("2026-05-01"));
        set(&mut t, "dt", json!("2026-05-01T10:00:00Z"));
        set(&mut t, "cb", json!(true));
        set(&mut t, "sel", json!("s2"));
        set(&mut t, "ms", json!(["s1", "s3"]));
        t
    }

    fn contrasting_task() -> Task {
        let mut t = task("contrasting", "Other task");
        set(&mut t, "txt", json!("Bye"));
        set(&mut t, "long", json!("Short"));
        set(&mut t, "num", json!(7));
        set(&mut t, "date", json!("2026-06-15"));
        set(&mut t, "dt", json!("2026-06-15T10:00:00Z"));
        set(&mut t, "cb", json!(false));
        set(&mut t, "sel", json!("s3"));
        set(&mut t, "ms", json!(["s2"]));
        t
    }

    #[test]
    fn contains_matches_and_rejects_for_text_and_number() {
        let (fields, lists) = (filter_fields(), filter_lists());
        let hit = populated_task();
        let miss = contrasting_task();

        let f = filter("txt", FilterOp::Contains, "hello wORld");
        assert!(matches_filters(&hit, std::slice::from_ref(&f), &fields, &lists));
        assert!(!matches_filters(&miss, &[f], &fields, &lists));

        let f = filter("long", FilterOp::Contains, "BODY");
        assert!(matches_filters(&hit, std::slice::from_ref(&f), &fields, &lists));
        assert!(!matches_filters(&miss, &[f], &fields, &lists));

        let f = filter("num", FilterOp::Contains, "42");
        assert!(matches_filters(&hit, std::slice::from_ref(&f), &fields, &lists));
        assert!(!matches_filters(&miss, &[f], &fields, &lists));
    }

    #[test]
    fn equals_matches_and_rejects_for_text_and_number() {
        let (fields, lists) = (filter_fields(), filter_lists());
        let hit = populated_task();
        let miss = contrasting_task();

        let f = filter("txt", FilterOp::Equals, "HELLO world");
        assert!(matches_filters(&hit, std::slice::from_ref(&f), &fields, &lists));
        assert!(!matches_filters(&miss, &[f], &fields, &lists));

        let f = filter("num", FilterOp::Equals, "42.5");
        assert!(matches_filters(&hit, std::slice::from_ref(&f), &fields, &lists));
        assert!(!matches_filters(&miss, &[f], &fields, &lists));

        // Unparsable number text: filter ignored for every task.
        let f = filter("num", FilterOp::Equals, "not a number");
        assert!(matches_filters(&hit, std::slice::from_ref(&f), &fields, &lists));
        assert!(matches_filters(&miss, &[f], &fields, &lists));
    }

    #[test]
    fn before_and_after_match_and_reject_for_date_and_datetime() {
        let (fields, lists) = (filter_fields(), filter_lists());
        let earlier = populated_task(); // 2026-05-01
        let later = contrasting_task(); // 2026-06-15

        let f = filter("date", FilterOp::Before, "2026-06-01");
        assert!(matches_filters(&earlier, std::slice::from_ref(&f), &fields, &lists));
        assert!(!matches_filters(&later, &[f], &fields, &lists));

        let f = filter("date", FilterOp::After, "2026-06-01");
        assert!(!matches_filters(&earlier, std::slice::from_ref(&f), &fields, &lists));
        assert!(matches_filters(&later, &[f], &fields, &lists));

        let f = filter("dt", FilterOp::Before, "2026-06-01T00:00:00Z");
        assert!(matches_filters(&earlier, std::slice::from_ref(&f), &fields, &lists));
        assert!(!matches_filters(&later, &[f], &fields, &lists));

        let f = filter("dt", FilterOp::After, "2026-06-01T00:00:00Z");
        assert!(!matches_filters(&earlier, std::slice::from_ref(&f), &fields, &lists));
        assert!(matches_filters(&later, &[f], &fields, &lists));

        // Unparsable filter text: ignored for every task.
        let junk = filter("date", FilterOp::Before, "tomorrow");
        assert!(matches_filters(&earlier, std::slice::from_ref(&junk), &fields, &lists));
        assert!(matches_filters(&later, &[junk], &fields, &lists));

        // A missing date is neither before nor after.
        let empty = task("empty", "No date");
        let f = filter("date", FilterOp::Before, "2026-06-01");
        assert!(!matches_filters(&empty, &[f], &fields, &lists));
    }

    #[test]
    fn any_of_and_has_option_match_and_reject() {
        let (fields, lists) = (filter_fields(), filter_lists());
        let hit = populated_task();
        let miss = contrasting_task();

        let f = filter_opts("sel", FilterOp::AnyOf, &["s1", "s2"]);
        assert!(matches_filters(&hit, std::slice::from_ref(&f), &fields, &lists));
        assert!(!matches_filters(&miss, &[f], &fields, &lists));

        let f = filter_opts("ms", FilterOp::HasOption, &["s3"]);
        assert!(matches_filters(&hit, std::slice::from_ref(&f), &fields, &lists));
        assert!(!matches_filters(&miss, &[f], &fields, &lists));
    }

    #[test]
    fn checkbox_is_true_and_is_false_match_and_reject() {
        let (fields, lists) = (filter_fields(), filter_lists());
        let on = populated_task(); // cb: true
        let off = contrasting_task(); // cb: false

        let f = filter("cb", FilterOp::IsTrue, "");
        assert!(matches_filters(&on, std::slice::from_ref(&f), &fields, &lists));
        assert!(!matches_filters(&off, &[f], &fields, &lists));

        let f = filter("cb", FilterOp::IsFalse, "");
        assert!(!matches_filters(&on, std::slice::from_ref(&f), &fields, &lists));
        assert!(matches_filters(&off, &[f], &fields, &lists));

        // A missing checkbox is not stored true or false.
        let none = task("none", "No checkbox");
        assert!(!matches_filters(&none, &[filter("cb", FilterOp::IsTrue, "")], &fields, &lists));
        assert!(!matches_filters(&none, &[filter("cb", FilterOp::IsFalse, "")], &fields, &lists));
    }

    #[test]
    fn empty_and_not_empty_match_and_reject() {
        let (fields, lists) = (filter_fields(), filter_lists());
        let full = populated_task();

        let mut sparse = task("sparse", "Sparse");
        set(&mut sparse, "txt", json!("")); // empty string
        set(&mut sparse, "long", Value::Null); // null
        set(&mut sparse, "ms", json!([])); // empty array
        // "num" and "cb" keys are missing entirely.

        for f in [
            filter("txt", FilterOp::Empty, ""),
            filter("long", FilterOp::Empty, ""),
            filter("num", FilterOp::Empty, ""),
            filter("ms", FilterOp::Empty, ""),
            filter("cb", FilterOp::Empty, ""),
        ] {
            assert!(matches_filters(&sparse, std::slice::from_ref(&f), &fields, &lists), "{f:?}");
            assert!(!matches_filters(&full, &[f], &fields, &lists));
        }

        let f = filter("txt", FilterOp::NotEmpty, "");
        assert!(matches_filters(&full, std::slice::from_ref(&f), &fields, &lists));
        assert!(!matches_filters(&sparse, &[f], &fields, &lists));

        // false is a stored value, so it is not empty.
        let off = with_value(task("off", "Off"), "cb", json!(false));
        assert!(!matches_filters(&off, &[filter("cb", FilterOp::Empty, "")], &fields, &lists));
    }

    #[test]
    fn unknown_field_id_filter_is_ignored() {
        let (fields, lists) = (filter_fields(), filter_lists());
        let f = filter("does-not-exist", FilterOp::Contains, "zzz");
        assert!(matches_filters(&populated_task(), std::slice::from_ref(&f), &fields, &lists));
        assert!(matches_filters(&contrasting_task(), &[f], &fields, &lists));
    }

    #[test]
    fn ops_that_do_not_fit_the_field_type_are_ignored() {
        let (fields, lists) = (filter_fields(), filter_lists());
        let hit = populated_task();
        let miss = contrasting_task();

        let f = filter("cb", FilterOp::Before, "2026-06-01"); // Before on checkbox
        assert!(matches_filters(&hit, std::slice::from_ref(&f), &fields, &lists));
        assert!(matches_filters(&miss, &[f], &fields, &lists));

        let f = filter_opts("txt", FilterOp::AnyOf, &["s1"]); // AnyOf on text
        assert!(matches_filters(&hit, std::slice::from_ref(&f), &fields, &lists));
        assert!(matches_filters(&miss, &[f], &fields, &lists));

        let f = filter("date", FilterOp::Contains, "2026"); // Contains on date
        assert!(matches_filters(&hit, std::slice::from_ref(&f), &fields, &lists));
        assert!(matches_filters(&miss, &[f], &fields, &lists));
    }

    #[test]
    fn filters_combine_with_and() {
        let (fields, lists) = (filter_fields(), filter_lists());
        let hit = populated_task();

        let contains = filter("txt", FilterOp::Contains, "hello");
        let equals = filter("num", FilterOp::Equals, "42.5");
        assert!(matches_filters(&hit, &[contains.clone(), equals.clone()], &fields, &lists));

        let contradicts = filter("num", FilterOp::Equals, "7");
        assert!(!matches_filters(&hit, &[contains, equals, contradicts], &fields, &lists));

        assert!(matches_filters(&hit, &[], &fields, &lists)); // no filters: everything matches
    }
}

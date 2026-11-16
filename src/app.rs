//! Application state: loads data, drives the UI models, and saves changes.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::Duration;

use chrono::Datelike;
use serde_json::Value;
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::model::{self, DataRoot, FieldDef, FieldType, ListDef, OptionDef, Task, Theme as AppTheme};
use crate::storage::{self, Dirty, Store};
use crate::table::{self, Filter, FilterOp, SortDir, SortField};
use crate::values;
use crate::{
	AppWindow, ColumnInfo, EditorTask, FieldRow, FieldSettingRow, FilterRow, ListSettingRow,
	OptionSettingRow, PickOption, TableCell, TaskRow, Theme as UiTheme,
};

const TYPE_NAMES: [&str; 8] = [
	"Text",
	"Long text",
	"Number",
	"Date",
	"Date & time",
	"Checkbox",
	"Dropdown",
	"Multi-select",
];

const OPTION_COLORS: [&str; 8] = [
	"#64748b", "#0ea5e9", "#8b5cf6", "#ec4899", "#f59e0b", "#22c55e", "#ef4444", "#14b8a6",
];

const SAVE_DEBOUNCE_MS: u64 = 700;

fn type_from_index(i: i32) -> FieldType {
	match i {
		1 => FieldType::Longtext,
		2 => FieldType::Number,
		3 => FieldType::Date,
		4 => FieldType::Datetime,
		5 => FieldType::Checkbox,
		6 => FieldType::Select,
		7 => FieldType::Multiselect,
		_ => FieldType::Text,
	}
}

fn theme_index(theme: AppTheme) -> i32 {
	match theme {
		AppTheme::System => 0,
		AppTheme::Light => 1,
		AppTheme::Dark => 2,
	}
}

fn theme_from_index(i: i32) -> AppTheme {
	match i {
		1 => AppTheme::Light,
		2 => AppTheme::Dark,
		_ => AppTheme::System,
	}
}

/// Parse `#rrggbb` (or `rrggbb`) into a colour; `None` when malformed.
fn hex_color(hex: &str) -> Option<Color> {
    let digits = hex.strip_prefix('#').unwrap_or(hex);
    if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&digits[0..2], 16).ok()? as f32 / 255.0;
    let g = u8::from_str_radix(&digits[2..4], 16).ok()? as f32 / 255.0;
    let b = u8::from_str_radix(&digits[4..6], 16).ok()? as f32 / 255.0;
    Some(Color::from_rgb_f32(r, g, b))
}

fn color_of(hex: Option<&String>) -> Color {
    hex.and_then(|h| hex_color(h))
        .unwrap_or_else(|| Color::from_rgb_f32(0.5, 0.5, 0.5))
}

fn filter_kind(ty: FieldType) -> i32 {
	match ty {
		FieldType::Date | FieldType::Datetime => 1,
		FieldType::Checkbox => 2,
		FieldType::Select | FieldType::Multiselect => 3,
		_ => 0,
	}
}

fn filter_ops(ty: FieldType) -> Vec<SharedString> {
	match ty {
		FieldType::Date | FieldType::Datetime => {
			vec!["Before".into(), "After".into(), "Is empty".into(), "Is not empty".into()]
		}
		FieldType::Checkbox => vec!["Any".into(), "On".into(), "Off".into()],
		_ => Vec::new(),
	}
}

fn op_index_of(op: &FilterOp, ty: FieldType) -> i32 {
	match ty {
		FieldType::Date | FieldType::Datetime => match op {
			FilterOp::Before => 0,
			FilterOp::After => 1,
			FilterOp::Empty => 2,
			FilterOp::NotEmpty => 3,
			_ => 0,
		},
		FieldType::Checkbox => match op {
			FilterOp::IsTrue => 1,
			FilterOp::IsFalse => 2,
			_ => 0,
		},
		_ => 0,
	}
}

/// A filter only counts (and matches) when it actually constrains something.
fn filter_is_meaningful(f: &Filter) -> bool {
	match f.op {
		FilterOp::Contains | FilterOp::Equals | FilterOp::Before | FilterOp::After => {
			!f.text.trim().is_empty()
		}
		FilterOp::AnyOf | FilterOp::HasOption => !f.options.is_empty(),
		FilterOp::Empty | FilterOp::NotEmpty | FilterOp::IsTrue | FilterOp::IsFalse => true,
	}
}

#[derive(Debug, Clone)]
enum PickerTarget {
	Field(String),
	Filter(String),
}

#[derive(Debug, Clone)]
enum ConfirmAction {
	DeleteTask(String),
	DeleteField(String),
	DeleteList(String),
	DeleteOption(String),
	ImportFrom(PathBuf),
}

/// A task being created or edited. Edits land here, never in the stored task:
/// Save is the only place that writes to `data.tasks`.
#[derive(Debug, Clone)]
struct Draft {
	task: Task,
	editing_id: Option<String>,
}

pub struct App {
	ui: AppWindow,
	store: Store,
	data: RefCell<DataRoot>,
	dirty: RefCell<Dirty>,
	save_timer: Timer,
	self_weak: RefCell<Option<Weak<App>>>,

	tasks_model: Rc<VecModel<TaskRow>>,
	columns_model: Rc<VecModel<ColumnInfo>>,
	editor_model: Rc<VecModel<EditorTask>>,
	editor_fields_model: Rc<VecModel<FieldRow>>,
	fields_model: Rc<VecModel<FieldSettingRow>>,
	lists_model: Rc<VecModel<ListSettingRow>>,
	filter_rows_model: Rc<VecModel<FilterRow>>,
	picker_model: Rc<VecModel<PickOption>>,

	selected: RefCell<Option<String>>,
	search: RefCell<String>,
	sort_by: RefCell<SortField>,
	sort_dir: RefCell<SortDir>,
	filters: RefCell<Vec<Filter>>,
	include_archived: RefCell<bool>,
	picker_target: RefCell<Option<PickerTarget>>,
	pending_date_field: RefCell<Option<String>>,
	pending_confirm: RefCell<Option<ConfirmAction>>,
	modal_editing: RefCell<Option<String>>,
	draft: RefCell<Option<Draft>>,
}

/// Load data, build the window, and run the event loop.
pub fn run() -> Result<(), slint::PlatformError> {
	let ui = AppWindow::new()?;
	let store = Store::new(Store::default_root());

	let (data, dirty) = match store.load() {
		Ok(loaded) => {
			let first_run = !store.root().join(storage::LISTS_FILE).exists()
				&& !store.root().join(storage::FIELDS_FILE).exists();
			let mut data = if first_run {
				model::seeded_data()
			} else {
				loaded.data
			};
			let mut dirty = if first_run {
				Dirty::ALL
			} else {
				Dirty::default()
			};
			for warning in &loaded.warnings {
				eprintln!("FocusBoard: {warning}");
			}
			if storage::cleanup_references(&mut data) {
				dirty.tasks = true;
			}
			(data, dirty)
		}
		Err(err) => {
			let backups = store.backups_dir();
			rfd::MessageDialog::new()
				.set_title("FocusBoard could not start")
				.set_description(format!(
					"{err}\n\nA backup may be available in:\n{}",
					backups.display()
				))
				.set_level(rfd::MessageLevel::Error)
				.show();
			return Ok(());
		}
	};

	let app = App::new(ui.clone_strong(), store, data, dirty);
	let _ = app.store.backup_daily();

	let weak = Rc::downgrade(&app);
	ui.window().on_close_requested(move || {
		if let Some(app) = weak.upgrade() {
			app.capture_window_state();
			app.save_now();
		}
		slint::CloseRequestResponse::HideWindow
	});

	// The window is shown first: the initial layout pass would otherwise
	// override a size applied before the window exists.
	ui.show()?;
	let window_state = app.data.borrow().settings.window.clone();
	apply_window_state(&ui, &window_state);
	ui.run()
}

/// The primary monitor's work area (screen minus taskbar) in physical pixels.
#[cfg(windows)]
fn work_area() -> Option<(i32, i32, u32, u32)> {
	#[repr(C)]
	struct Rect {
		left: i32,
		top: i32,
		right: i32,
		bottom: i32,
	}
	const SPI_GETWORKAREA: u32 = 0x0030;
	unsafe extern "system" {
		fn SystemParametersInfoW(action: u32, param: u32, data: *mut Rect, win_ini: u32) -> i32;
	}

	let mut rect = Rect {
		left: 0,
		top: 0,
		right: 0,
		bottom: 0,
	};
	let ok = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut rect, 0) };
	let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
	if ok != 0 && w > 0 && h > 0 {
		Some((rect.left, rect.top, w as u32, h as u32))
	} else {
		None
	}
}

#[cfg(not(windows))]
fn work_area() -> Option<(i32, i32, u32, u32)> {
	None
}

/// Whether Windows is set to dark mode for apps. Read from the per-user setting
/// (`AppsUseLightTheme`); when it is missing or unreadable, assume light.
#[cfg(windows)]
fn system_prefers_dark() -> bool {
	use std::os::windows::ffi::OsStrExt;

	const HKEY_CURRENT_USER: *mut core::ffi::c_void =
		0x8000_0001_u32 as i32 as isize as *mut core::ffi::c_void;
	// Only accept a DWORD value.
	const RRF_RT_REG_DWORD: u32 = 0x0000_0010;

	#[link(name = "advapi32")]
	unsafe extern "system" {
		fn RegGetValueW(
			hkey: *mut core::ffi::c_void,
			sub_key: *const u16,
			value: *const u16,
			flags: u32,
			kind: *mut u32,
			data: *mut u8,
			data_size: *mut u32,
		) -> i32;
	}

	fn wide(text: &str) -> Vec<u16> {
		std::ffi::OsStr::new(text)
			.encode_wide()
			.chain(std::iter::once(0))
			.collect()
	}

	let sub_key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
	let value = wide("AppsUseLightTheme");
	let mut use_light: u32 = 1;
	let mut size = std::mem::size_of::<u32>() as u32;
	let status = unsafe {
		RegGetValueW(
			HKEY_CURRENT_USER,
			sub_key.as_ptr(),
			value.as_ptr(),
			RRF_RT_REG_DWORD,
			std::ptr::null_mut(),
			&mut use_light as *mut u32 as *mut u8,
			&mut size,
		)
	};
	status == 0 && use_light == 0
}

#[cfg(not(windows))]
fn system_prefers_dark() -> bool {
	false
}

/// Restore the saved window size and position, clamped to the available screen so
/// the window can never end up (mostly) off-screen. Sizes are in physical pixels:
/// that is what the windowing system works in, so no scale factor is needed (and
/// the scale factor is not reliable before the window has been shown).
fn apply_window_state(ui: &AppWindow, state: &model::WindowState) {
	const DEFAULT_W: f32 = 1500.0;
	const DEFAULT_H: f32 = 950.0;

	let work = work_area();
	let mut width = if state.width > 0.0 { state.width } else { DEFAULT_W };
	let mut height = if state.height > 0.0 { state.height } else { DEFAULT_H };
	if let Some((_, _, work_w, work_h)) = work {
		width = width.min(work_w as f32);
		height = height.min(work_h as f32);
	}
	ui.window()
		.set_size(slint::PhysicalSize::new(width as u32, height as u32));

	let position = match (state.x, state.y) {
		(Some(x), Some(y)) => Some((x as i32, y as i32)),
		_ => work.map(|(wx, wy, ww, wh)| {
			(
				wx + (ww as f32 - width) as i32 / 2,
				wy + (wh as f32 - height) as i32 / 2,
			)
		}),
	};
	if let Some((mut x, mut y)) = position {
		if let Some((wx, wy, ww, wh)) = work {
			// pull the window fully on screen when it fits; otherwise pin its top-left corner
			let max_x = (wx + ww as i32 - width as i32).max(wx);
			let max_y = (wy + wh as i32 - height as i32).max(wy);
			x = x.clamp(wx, max_x);
			y = y.clamp(wy, max_y);
		}
		ui.window()
			.set_position(slint::PhysicalPosition::new(x, y));
	}
	if state.maximized {
		ui.window().set_maximized(true);
	}
}

fn call(weak: &Weak<App>, f: impl FnOnce(&App)) {
	if let Some(app) = weak.upgrade() {
		f(&app);
	}
}

impl App {
	fn new(ui: AppWindow, store: Store, data: DataRoot, dirty: Dirty) -> Rc<Self> {
		let tasks_model = Rc::new(VecModel::default());
		let columns_model = Rc::new(VecModel::default());
		let editor_model = Rc::new(VecModel::default());
		let editor_fields_model = Rc::new(VecModel::default());
		let fields_model = Rc::new(VecModel::default());
		let lists_model = Rc::new(VecModel::default());
		let filter_rows_model = Rc::new(VecModel::default());
		let picker_model = Rc::new(VecModel::default());

		ui.set_tasks(ModelRc::from(tasks_model.clone()));
		ui.set_columns(ModelRc::from(columns_model.clone()));
		ui.set_editor(ModelRc::from(editor_model.clone()));
		ui.set_editor_fields(ModelRc::from(editor_fields_model.clone()));
		ui.set_settings_fields(ModelRc::from(fields_model.clone()));
		ui.set_settings_lists(ModelRc::from(lists_model.clone()));
		ui.set_filter_rows(ModelRc::from(filter_rows_model.clone()));
		ui.set_picker_options(ModelRc::from(picker_model.clone()));
		ui.set_modal_type_names(ModelRc::from(Rc::new(VecModel::from(
			TYPE_NAMES
				.iter()
				.map(|s| SharedString::from(*s))
				.collect::<Vec<_>>(),
		))));
		ui.set_data_path(store.root().display().to_string().into());
		ui.set_status_text("Ready".into());

		let theme = data.settings.theme;

		let app = Rc::new(Self {
			ui,
			store,
			data: RefCell::new(data),
			dirty: RefCell::new(dirty),
			save_timer: Timer::default(),
			self_weak: RefCell::new(None),
			tasks_model,
			columns_model,
			editor_model,
			editor_fields_model,
			fields_model,
			lists_model,
			filter_rows_model,
			picker_model,
			selected: RefCell::new(None),
			search: RefCell::new(String::new()),
			sort_by: RefCell::new(SortField::Manual),
			sort_dir: RefCell::new(SortDir::Asc),
			filters: RefCell::new(Vec::new()),
			include_archived: RefCell::new(false),
			picker_target: RefCell::new(None),
			pending_date_field: RefCell::new(None),
			pending_confirm: RefCell::new(None),
			modal_editing: RefCell::new(None),
			draft: RefCell::new(None),
		});
		*app.self_weak.borrow_mut() = Some(Rc::downgrade(&app));

		app.ui
			.set_sidebar_collapsed(app.data.borrow().settings.sidebar_collapsed);
		app.apply_theme(theme);
		app.wire();
		app.rebuild_all();
		if app.dirty.borrow().any() {
			app.save_now();
		}
		app
	}

	// ---------------------------------------------------------------- wiring

	fn wire(self: &Rc<Self>) {
		let ui = self.ui.clone_strong();

		macro_rules! connect {
			($setter:ident, |$app:ident, $($arg:ident),*| $body:block) => {
				{
					let weak = Rc::downgrade(self);
					ui.$setter(move |$($arg),*| {
						call(&weak, |$app| $body);
					});
				}
			};
		}

		connect!(on_quick_add, |app, title| {
			app.quick_add(title.to_string());
		});
		connect!(on_add_task, |app,| {
			app.add_task();
		});
		connect!(on_save_task, |app,| {
			app.save_task_form();
		});
		connect!(on_cancel_task, |app,| {
			app.close_task_form();
		});
		connect!(on_search, |app, q| {
			app.set_search(q.to_string());
		});
		connect!(on_toggle_archived, |app, v| {
			app.set_include_archived(v);
		});
		connect!(on_sort_column, |app, i| {
			app.sort_column(i);
		});
		connect!(on_move_task, |app, from, to| {
			app.move_task(from, to);
		});
		connect!(on_select_task, |app, id| {
			app.select_task(id.to_string());
		});
		connect!(on_close_editor, |app,| {
			app.close_editor();
		});
		connect!(on_edit_title, |app, t| {
			app.edit_title(t.to_string());
		});
		connect!(on_edit_description, |app, t| {
			app.edit_description(t.to_string());
		});
		connect!(on_edit_field, |app, id, t| {
			app.edit_field(id.to_string(), t.to_string());
		});
		connect!(on_toggle_checkbox, |app, id, c| {
			app.toggle_checkbox(id.to_string(), c);
		});
		connect!(on_pick_field, |app, id| {
			app.pick_field(id.to_string());
		});
		connect!(on_select_option, |app, id, i| {
			app.select_option(id.to_string(), i);
		});
		connect!(on_archive_task, |app, id| {
			app.set_task_archived(id.to_string(), true);
		});
		connect!(on_restore_task, |app, id| {
			app.set_task_archived(id.to_string(), false);
		});
		connect!(on_delete_task, |app, id| {
			app.ask_delete_task(id.to_string());
		});
		connect!(on_option_clicked, |app, id| {
			app.option_clicked(id.to_string());
		});
		connect!(on_filter_text, |app, id, t| {
			app.filter_text(id.to_string(), t.to_string());
		});
		connect!(on_filter_op, |app, id, i| {
			app.filter_op(id.to_string(), i);
		});
		connect!(on_open_filter_pick, |app, id| {
			app.open_filter_pick(id.to_string());
		});
		connect!(on_clear_filters, |app,| {
			app.clear_filters();
		});
		connect!(on_date_accepted, |app, y, m, d| {
			app.date_accepted(y, m, d);
		});

		// fields settings
		connect!(on_field_new, |app,| {
			app.field_modal_new();
		});
		connect!(on_field_edit, |app, id| {
			app.field_modal_edit(id.to_string());
		});
		connect!(on_field_save, |app,| {
			app.field_save();
		});
		connect!(on_field_move, |app, id, dir| {
			app.field_move(id.to_string(), dir);
		});
		connect!(on_field_archive, |app, id| {
			app.field_archived(id.to_string(), true);
		});
		connect!(on_field_restore, |app, id| {
			app.field_archived(id.to_string(), false);
		});
		connect!(on_field_delete, |app, id| {
			app.ask_delete_field(id.to_string());
		});
		connect!(on_field_toggle_required, |app, id, v| {
			app.field_toggle(id.to_string(), v, true);
		});
		connect!(on_field_toggle_show, |app, id, v| {
			app.field_toggle(id.to_string(), v, false);
		});

		// list settings
		connect!(on_list_add, |app, name| {
			app.list_add(name.to_string());
		});
		connect!(on_list_rename, |app, id, name| {
			app.list_rename(id.to_string(), name.to_string());
		});
		connect!(on_list_move, |app, id, dir| {
			app.list_move(id.to_string(), dir);
		});
		connect!(on_list_archive, |app, id| {
			app.list_archived(id.to_string(), true);
		});
		connect!(on_list_restore, |app, id| {
			app.list_archived(id.to_string(), false);
		});
		connect!(on_list_delete, |app, id| {
			app.ask_delete_list(id.to_string());
		});
		connect!(on_option_add, |app, list, label| {
			app.option_add(list.to_string(), label.to_string());
		});
		connect!(on_option_rename, |app, id, label| {
			app.option_rename(id.to_string(), label.to_string());
		});
		connect!(on_option_color, |app, id, hex| {
			app.option_color(id.to_string(), hex.to_string());
		});
		connect!(on_option_move, |app, id, dir| {
			app.option_move(id.to_string(), dir);
		});
		connect!(on_option_archive, |app, id| {
			app.option_archived(id.to_string(), true);
		});
		connect!(on_option_restore, |app, id| {
			app.option_archived(id.to_string(), false);
		});
		connect!(on_option_delete, |app, id| {
			app.ask_delete_option(id.to_string());
		});

		// data settings
		connect!(on_open_data_folder, |app,| {
			let root = app.store.root().to_path_buf();
			app.open_folder(root);
		});
		connect!(on_open_backups_folder, |app,| {
			let dir = app.store.backups_dir();
			app.open_folder(dir);
		});
		connect!(on_export_data, |app,| {
			app.export_data();
		});
		connect!(on_import_data, |app,| {
			app.ask_import();
		});
		connect!(on_set_theme, |app, i| {
			app.set_theme(i);
		});
		connect!(on_set_sidebar_collapsed, |app, v| {
			app.set_sidebar_collapsed(v);
		});
		connect!(on_confirm_yes, |app,| {
			app.confirm_yes();
		});
	}

	// -------------------------------------------------------------- saving

	fn mark_dirty(&self, f: impl FnOnce(&mut Dirty)) {
		{
			let mut dirty = self.dirty.borrow_mut();
			f(&mut dirty);
		}
		self.ui.set_status_text("Editing\u{2026}".into());
		if let Some(weak) = self.self_weak.borrow().clone() {
			self.save_timer.start(
				TimerMode::SingleShot,
				Duration::from_millis(SAVE_DEBOUNCE_MS),
				move || {
					if let Some(app) = weak.upgrade() {
						app.save_now();
					}
				},
			);
		}
	}

	fn save_now(&self) {
		let dirty = *self.dirty.borrow();
		if !dirty.any() {
			return;
		}
		let result = {
			let data = self.data.borrow();
			self.store.save(&data, dirty)
		};
		match result {
			Ok(()) => {
				*self.dirty.borrow_mut() = Dirty::default();
				let now = chrono::Local::now().format("%H:%M:%S");
				self.ui.set_status_text(format!("Saved {now}").into());
			}
			Err(err) => {
				self.ui
					.set_status_text(format!("Save failed: {err}").into());
			}
		}
	}

	fn capture_window_state(&self) {
		// physical pixels, matching what apply_window_state() expects
		let window = self.ui.window();
		let maximized = window.is_maximized();
		{
			let mut data = self.data.borrow_mut();
			let state = &mut data.settings.window;
			state.maximized = maximized;
			// While maximized the window is screen-sized: keep the last restored
			// geometry instead so that un-maximizing lands somewhere sensible.
			if !maximized {
				let size = window.size();
				let position = window.position();
				state.width = size.width as f32;
				state.height = size.height as f32;
				state.x = Some(position.x as f32);
				state.y = Some(position.y as f32);
			}
		}
		self.mark_dirty(|d| d.settings = true);
	}

	fn apply_theme(&self, theme: AppTheme) {
		self.ui.set_theme_mode(theme_index(theme));
		// "Follow system" has to be resolved to a concrete light/dark value here:
		// theme.slint needs a yes/no answer for the app's own colours and Slint only
		// exposes the system scheme to its bundled styles.
		let resolved = match theme {
			AppTheme::System => {
				if system_prefers_dark() {
					2
				} else {
					1
				}
			}
			other => theme_index(other),
		};
		self.ui.global::<UiTheme>().invoke_apply_scheme(resolved);
	}

	// ----------------------------------------------------------- rebuilding

	fn rebuild_all(&self) {
		self.rebuild_columns();
		self.rebuild_rows();
		self.rebuild_editor();
		self.rebuild_settings_fields();
		self.rebuild_lists();
		self.rebuild_filters();
	}

	fn rebuild_columns(&self) {
		let data = self.data.borrow();
		let columns = table::visible_columns(&data.fields);
		let sort_field = self.sort_by.borrow().clone();
		let desc = matches!(*self.sort_dir.borrow(), SortDir::Desc);

		let mut items = Vec::with_capacity(columns.len() + 1);
		items.push(ColumnInfo {
			label: "Title".into(),
			sort: match &sort_field {
				SortField::Title => {
					if desc {
						2
					} else {
						1
					}
				}
				_ => 0,
			},
		});
		for f in &columns {
			let sort = match &sort_field {
				SortField::Custom(id) if id == &f.id => {
					if desc {
						2
					} else {
						1
					}
				}
				_ => 0,
			};
			items.push(ColumnInfo {
				label: f.label.clone().into(),
				sort,
			});
		}
		self.columns_model.set_vec(items);
	}

	fn visible_tasks(&self) -> Vec<Task> {
		let data = self.data.borrow();
		let include_archived = *self.include_archived.borrow();
		let search = self.search.borrow().clone();
		let filters = self.filters.borrow().clone();

		let mut tasks: Vec<Task> = data
			.tasks
			.iter()
			.filter(|t| include_archived || !t.is_archived())
			.filter(|t| table::matches_search(t, &search))
			.filter(|t| table::matches_filters(t, &filters, &data.fields, &data.lists))
			.cloned()
			.collect();

		let by = self.sort_by.borrow().clone();
		let dir = *self.sort_dir.borrow();
		table::sort_tasks(&mut tasks, &by, dir, &data.fields, &data.lists);

		let (active, archived): (Vec<Task>, Vec<Task>) =
			tasks.into_iter().partition(|t| !t.is_archived());
		let mut result = active;
		result.extend(archived);
		result
	}

	fn task_row(&self, task: &Task, data: &DataRoot) -> TaskRow {
		let columns = table::visible_columns(&data.fields);
		let cells: Vec<TableCell> = columns
			.iter()
			.map(|f| {
				let value = task.values.get(&f.id).cloned().unwrap_or(Value::Null);
				let text = values::display(f, &value, &data.lists);
				let dot = match f.ty {
					FieldType::Select | FieldType::Multiselect => {
						option_color_of(f, &value, data)
					}
					_ => None,
				};
				match dot {
					Some(color) => TableCell {
						text: text.into(),
						dot: color,
						has_dot: true,
					},
					None => TableCell {
						text: text.into(),
						dot: Color::default(),
						has_dot: false,
					},
				}
			})
			.collect();

		TaskRow {
			id: task.id.clone().into(),
			title: task.title.clone().into(),
			cells: ModelRc::from(Rc::new(VecModel::from(cells))),
			archived: task.is_archived(),
			meta: task.updated_at.clone().into(),
		}
	}

	fn rebuild_rows(&self) {
		let tasks = self.visible_tasks();
		let rows: Vec<TaskRow> = {
			let data = self.data.borrow();
			tasks.iter().map(|t| self.task_row(t, &data)).collect()
		};
		self.tasks_model.set_vec(rows);

		// "shown / total": total counts every task, including archived ones, so the
		// number does not seem to shrink when the archived toggle is switched on.
		let total = self.data.borrow().tasks.len();
		let shown = self.tasks_model.row_count();
		let active_filters = self
			.filters
			.borrow()
			.iter()
			.filter(|f| filter_is_meaningful(f))
			.count();
		self.ui.set_total_count(total as i32);
		self.ui.set_shown_count(shown as i32);
		self.ui.set_filter_count(active_filters as i32);
		self.ui.set_show_archived(*self.include_archived.borrow());
	}

	fn field_row(&self, task: &Task, field: &FieldDef, data: &DataRoot) -> FieldRow {
		let value = task.values.get(&field.id).cloned().unwrap_or(Value::Null);
		let value_label = match field.ty {
			FieldType::Select | FieldType::Multiselect => values::display(field, &value, &data.lists),
			_ => String::new(),
		};
		// Single selects get an inline combo: index 0 is the empty choice and the
		// rest are option labels, so the widget never needs the option-id popup.
		let (options, option_index) = if field.ty == FieldType::Select {
			let mut labels = vec!["Choose\u{2026}".to_string()];
			let mut index = 0;
			for (i, option) in field_picker_options(field, &value, data).into_iter().enumerate() {
				if option.selected {
					index = i + 1;
				}
				labels.push(if option.archived {
					format!("{} (archived)", option.label)
				} else {
					option.label.to_string()
				});
			}
			(labels, index as i32)
		} else {
			(Vec::new(), 0)
		};
		FieldRow {
			id: field.id.clone().into(),
			label: field.label.clone().into(),
			kind: values::kind_of(field.ty),
			text: values::to_input(field, &value).into(),
			checked: matches!(value, Value::Bool(true)),
			value_label: value_label.into(),
			error: "".into(),
			options: ModelRc::from(Rc::new(VecModel::from(
				options
					.into_iter()
					.map(SharedString::from)
					.collect::<Vec<_>>(),
			))),
			option_index,
		}
	}

	/// Option ids in the same order as `field_row`'s combo labels.
	fn select_option_ids(&self, field_id: &str) -> Vec<String> {
		let data = self.data.borrow();
		let Some(field) = data.fields.iter().find(|f| f.id == field_id) else {
			return Vec::new();
		};
		let value = self
			.draft
			.borrow()
			.as_ref()
			.and_then(|d| d.task.values.get(field_id).cloned())
			.unwrap_or(Value::Null);
		field_picker_options(field, &value, &data)
			.into_iter()
			.map(|option| option.id.to_string())
			.collect()
	}

	/// Rebuild the modal form from the draft (empty when no form is open).
	fn rebuild_editor(&self) {
		let Some(draft) = self.draft.borrow().as_ref().map(|d| d.task.clone()) else {
			self.editor_model.set_vec(Vec::new());
			self.editor_fields_model.set_vec(Vec::new());
			return;
		};
		let data = self.data.borrow();
		self.editor_model.set_vec(vec![EditorTask {
			id: draft.id.clone().into(),
			title: draft.title.clone().into(),
			description: draft.description.clone().into(),
			archived: draft.is_archived(),
		}]);
		let rows: Vec<FieldRow> = data
			.fields
			.iter()
			.filter(|f| !f.archived)
			.map(|f| self.field_row(&draft, f, &data))
			.collect();
		self.editor_fields_model.set_vec(rows);
	}

	fn refresh_editor_field(&self, field_id: &str, error: Option<(String, String)>) {
		let Some(task) = self.draft.borrow().as_ref().map(|d| d.task.clone()) else {
			return;
		};
		let idx = (0..self.editor_fields_model.row_count()).find(|&i| {
			self.editor_fields_model
				.row_data(i)
				.map(|r| r.id.as_str() == field_id)
				.unwrap_or(false)
		});
		let Some(i) = idx else { return };
		let row = {
			let data = self.data.borrow();
			let Some(field) = data.fields.iter().find(|f| f.id == field_id) else {
				return;
			};
			let mut row = self.field_row(&task, field, &data);
			if let Some((text, message)) = error {
				row.text = text.into();
				row.error = message.into();
			}
			row
		};
		self.editor_fields_model.set_row_data(i, row);
	}

	fn rebuild_settings_fields(&self) {
		let data = self.data.borrow();
		let rows: Vec<FieldSettingRow> = data
			.fields
			.iter()
			.map(|f| {
				let list_name = f
					.list_id
					.as_ref()
					.and_then(|id| data.lists.iter().find(|l| &l.id == id))
					.map(|l| l.name.clone())
					.unwrap_or_default();
				FieldSettingRow {
					id: f.id.clone().into(),
					label: f.label.clone().into(),
					type_name: TYPE_NAMES[values::kind_of(f.ty) as usize].into(),
					list_name: list_name.into(),
					required: f.required,
					show_in_list: f.show_in_list,
					archived: f.archived,
				}
			})
			.collect();
		self.fields_model.set_vec(rows);
	}

	fn rebuild_lists(&self) {
		let data = self.data.borrow();
		let rows: Vec<ListSettingRow> = data
			.lists
			.iter()
			.map(|l| {
				let options: Vec<OptionSettingRow> = l
					.options
					.iter()
					.map(|o| OptionSettingRow {
						id: o.id.clone().into(),
						label: o.label.clone().into(),
						swatch: color_of(o.color.as_ref()),
						hex: o.color.clone().unwrap_or_default().into(),
						archived: o.archived,
					})
					.collect();
				ListSettingRow {
					id: l.id.clone().into(),
					name: l.name.clone().into(),
					archived: l.archived,
					option_count: l.options.len() as i32,
					options: ModelRc::from(Rc::new(VecModel::from(options))),
				}
			})
			.collect();
		self.lists_model.set_vec(rows);
	}

	fn filter_row(&self, field: &FieldDef) -> FilterRow {
		let kind = filter_kind(field.ty);
		let existing = self
			.filters
			.borrow()
			.iter()
			.find(|f| f.field_id == field.id)
			.cloned();

		let mut row = FilterRow {
			field_id: field.id.clone().into(),
			label: field.label.clone().into(),
			kind,
			text: "".into(),
			op_index: 0,
			ops: ModelRc::from(Rc::new(VecModel::<SharedString>::default())),
			summary: "".into(),
		};

		match kind {
			0 => {
				if let Some(f) = existing {
					row.text = f.text.into();
				}
			}
			1 | 2 => {
				row.ops = ModelRc::from(Rc::new(VecModel::from(filter_ops(field.ty))));
				if let Some(f) = &existing {
					row.op_index = op_index_of(&f.op, field.ty);
					row.text = f.text.clone().into();
				}
			}
			3 => {
				if let Some(f) = &existing {
					let data = self.data.borrow();
					row.summary = summary_of_options(field, &f.options, &data).into();
				}
			}
			_ => {}
		}
		row
	}

	fn rebuild_filters(&self) {
		let rows: Vec<FilterRow> = {
			let data = self.data.borrow();
			data.fields
				.iter()
				.filter(|f| !f.archived)
				.map(|f| self.filter_row(f))
				.collect()
		};
		self.filter_rows_model.set_vec(rows);
	}

	fn rebuild_picker(&self) {
		let target = self.picker_target.borrow().clone();
		let Some(target) = target else {
			self.picker_model.set_vec(Vec::new());
			return;
		};
		let options = {
			let data = self.data.borrow();
			match target {
				PickerTarget::Field(field_id) => {
					let Some(task_id) = self.selected.borrow().clone() else {
						return;
					};
					let Some(field) = data.fields.iter().find(|f| f.id == field_id) else {
						return;
					};
					let value = data
						.tasks
						.iter()
						.find(|t| t.id == task_id)
						.and_then(|t| t.values.get(&field_id).cloned())
						.unwrap_or(Value::Null);
					field_picker_options(field, &value, &data)
				}
				PickerTarget::Filter(field_id) => {
					let Some(field) = data.fields.iter().find(|f| f.id == field_id) else {
						return;
					};
					let selected = self
						.filters
						.borrow()
						.iter()
						.find(|f| f.field_id == field_id)
						.map(|f| f.options.clone())
						.unwrap_or_default();
					list_picker_options(field, &selected, &data)
				}
			}
		};
		self.picker_model.set_vec(options);
	}

	// ---------------------------------------------------------------- tasks

	fn quick_add(&self, title: String) {
		let title = title.trim().to_string();
		if title.is_empty() {
			return;
		}
		let task = {
			let data = self.data.borrow();
			let mut task = Task::new(title);
			for field in data.fields.iter().filter(|f| !f.archived) {
				let value = field
					.default_value
					.clone()
					.unwrap_or_else(|| values::default_for(field));
				if !values::is_empty_value(&value) {
					task.values.insert(field.id.clone(), value);
				}
			}
			task
		};
		self.data.borrow_mut().tasks.push(task);
		self.mark_dirty(|d| d.tasks = true);
		self.rebuild_rows();
	}

	fn task_with_defaults(&self) -> Task {
		let data = self.data.borrow();
		let mut task = Task::new("");
		for field in data.fields.iter().filter(|f| !f.archived) {
			let value = field
				.default_value
				.clone()
				.unwrap_or_else(|| values::default_for(field));
			if !values::is_empty_value(&value) {
				task.values.insert(field.id.clone(), value);
			}
		}
		task
	}

	/// "New task" only opens a draft form. Nothing is stored until Save.
	fn add_task(&self) {
		let task = self.task_with_defaults();
		*self.draft.borrow_mut() = Some(Draft {
			task,
			editing_id: None,
		});
		*self.selected.borrow_mut() = None;
		self.ui.set_selected_id("".into());
		self.ui.set_has_selection(false);
		self.ui.set_modal_title("New task".into());
		self.ui.set_task_form_editing(false);
		self.ui.set_modal_error("".into());
		self.ui.set_modal_height(520.0);
		self.ui.set_modal_kind(3);
		self.rebuild_editor();
		self.ui.set_modal_open(true);
	}

	fn set_search(&self, query: String) {
		*self.search.borrow_mut() = query;
		self.rebuild_rows();
	}

	fn set_include_archived(&self, value: bool) {
		*self.include_archived.borrow_mut() = value;
		self.rebuild_rows();
	}

	/// Header click: ascending, then descending, then back to the hand-arranged
	/// order (drag & drop), which is what the list starts with.
	fn sort_column(&self, index: i32) {
		let by = if index <= 0 {
			SortField::Title
		} else {
			let data = self.data.borrow();
			let columns = table::visible_columns(&data.fields);
			match columns.get((index - 1) as usize) {
				Some(f) => SortField::Custom(f.id.clone()),
				None => return,
			}
		};
		if *self.sort_by.borrow() == by {
			if matches!(*self.sort_dir.borrow(), SortDir::Asc) {
				*self.sort_dir.borrow_mut() = SortDir::Desc;
			} else {
				*self.sort_by.borrow_mut() = SortField::Manual;
				*self.sort_dir.borrow_mut() = SortDir::Asc;
			}
		} else {
			*self.sort_by.borrow_mut() = by;
			*self.sort_dir.borrow_mut() = SortDir::Asc;
		}
		self.rebuild_columns();
		self.rebuild_rows();
	}

	/// Drag & drop: put the row at `from` where the row at `to` sits. The list
	/// switches to the hand-arranged order, since that is what the user just made.
	fn move_task(&self, from: i32, to: i32) {
		let rows: Vec<String> = (0..self.tasks_model.row_count())
			.filter_map(|i| self.tasks_model.row_data(i).map(|r| r.id.to_string()))
			.collect();
		if from < 0 || to < 0 || from == to {
			return;
		}
		let (from, to) = (from as usize, to as usize);
		if from >= rows.len() || to >= rows.len() {
			return;
		}
		let order = table::reorder(&rows, from, to);
		table::apply_order(&mut self.data.borrow_mut().tasks, &order);
		*self.sort_by.borrow_mut() = SortField::Manual;
		*self.sort_dir.borrow_mut() = SortDir::Asc;
		self.mark_dirty(|d| d.tasks = true);
		self.rebuild_columns();
		self.rebuild_rows();
	}

	/// Clicking a row opens the same draft form, pre-filled from the stored task.
	fn select_task(&self, id: String) {
		let Some(task) = self
			.data
			.borrow()
			.tasks
			.iter()
			.find(|t| t.id == id)
			.cloned()
		else {
			return;
		};
		*self.draft.borrow_mut() = Some(Draft {
			task,
			editing_id: Some(id.clone()),
		});
		*self.selected.borrow_mut() = Some(id.clone());
		self.ui.set_selected_id(id.into());
		self.ui.set_has_selection(true);
		self.ui.set_modal_title("Edit task".into());
		self.ui.set_task_form_editing(true);
		self.ui.set_modal_error("".into());
		self.ui.set_modal_height(520.0);
		self.ui.set_modal_kind(3);
		self.rebuild_editor();
		self.ui.set_modal_open(true);
	}

	fn close_task_form(&self) {
		*self.draft.borrow_mut() = None;
		self.ui.set_modal_open(false);
		self.ui.set_modal_error("".into());
		self.editor_model.set_vec(Vec::new());
		self.editor_fields_model.set_vec(Vec::new());
	}

	fn close_editor(&self) {
		self.close_task_form();
		*self.selected.borrow_mut() = None;
		self.ui.set_selected_id("".into());
		self.ui.set_has_selection(false);
	}

	fn select_option(&self, field_id: String, index: i32) {
		let ids = self.select_option_ids(&field_id);
		let value = if index <= 0 || index as usize > ids.len() {
			Value::Null
		} else {
			Value::String(ids[index as usize - 1].clone())
		};
		if let Some(draft) = self.draft.borrow_mut().as_mut() {
			draft.task.values.insert(field_id.clone(), value);
		}
		self.refresh_editor_field(&field_id, None);
	}

	/// Validate the draft and write it back only on Save.
	fn save_task_form(&self) {
		let Some(mut draft) = self.draft.borrow().clone() else {
			return;
		};
		let title = draft.task.title.trim().to_string();
		if title.is_empty() {
			self.ui.set_modal_error("Give the task a title.".into());
			return;
		}
		draft.task.title = title;
		{
			let data = self.data.borrow();
			for field in data.fields.iter().filter(|f| !f.archived) {
				let value = draft
					.task
					.values
					.get(&field.id)
					.cloned()
					.unwrap_or(Value::Null);
				if field.required && values::is_empty_value(&value) {
					self.ui
						.set_modal_error(format!("{} is required.", field.label).into());
					return;
				}
				if let Err(message) = values::validate(field, &value) {
					self.ui
						.set_modal_error(format!("{}: {message}", field.label).into());
					return;
				}
			}
		}
		let id = draft.task.id.clone();
		draft.task.updated_at = model::now_iso();
		{
			let mut data = self.data.borrow_mut();
			if let Some(editing_id) = &draft.editing_id {
				if let Some(pos) = data.tasks.iter().position(|t| t.id == *editing_id) {
					let stored = data.tasks[pos].clone();
					draft.task.created_at = stored.created_at;
					draft.task.archived_at = stored.archived_at;
					data.tasks[pos] = draft.task;
				}
			} else {
				data.tasks.push(draft.task);
			}
		}
		self.mark_dirty(|d| d.tasks = true);
		self.rebuild_rows();
		self.close_task_form();
		*self.selected.borrow_mut() = Some(id.clone());
		self.ui.set_selected_id(id.into());
		self.ui.set_has_selection(true);
	}

	fn edit_title(&self, title: String) {
		if let Some(draft) = self.draft.borrow_mut().as_mut() {
			draft.task.title = title;
		}
		self.ui.set_modal_error("".into());
	}

	fn edit_description(&self, description: String) {
		if let Some(draft) = self.draft.borrow_mut().as_mut() {
			draft.task.description = description;
		}
	}

	fn edit_field(&self, field_id: String, text: String) {
		let parsed = {
			let data = self.data.borrow();
			let Some(field) = data.fields.iter().find(|f| f.id == field_id) else {
				return;
			};
			values::parse_input(field, &text)
		};
		match parsed {
			Ok(value) => {
				if let Some(draft) = self.draft.borrow_mut().as_mut() {
					draft.task.values.insert(field_id.clone(), value);
				}
				self.refresh_editor_field(&field_id, None);
			}
			Err(message) => {
				self.refresh_editor_field(&field_id, Some((text, message)));
			}
		}
	}

	fn toggle_checkbox(&self, field_id: String, checked: bool) {
		if let Some(draft) = self.draft.borrow_mut().as_mut() {
			draft.task
				.values
				.insert(field_id.clone(), Value::Bool(checked));
		}
		self.refresh_editor_field(&field_id, None);
	}

	fn pick_field(&self, field_id: String) {
		let Some(task_id) = self.selected.borrow().clone() else {
			return;
		};
		let (ty, label, value) = {
			let data = self.data.borrow();
			let Some(field) = data.fields.iter().find(|f| f.id == field_id) else {
				return;
			};
			let value = data
				.tasks
				.iter()
				.find(|t| t.id == task_id)
				.and_then(|t| t.values.get(&field_id).cloned())
				.unwrap_or(Value::Null);
			(field.ty, field.label.clone(), value)
		};
		match ty {
			FieldType::Date => {
				let (y, m, d) = value
					.as_str()
					.and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
					.map(|d| (d.year(), d.month() as i32, d.day() as i32))
					.unwrap_or_else(|| {
						let today = chrono::Local::now().date_naive();
						(today.year(), today.month() as i32, today.day() as i32)
					});
				*self.pending_date_field.borrow_mut() = Some(field_id);
				self.ui.invoke_open_date_picker(y, m, d);
			}
			FieldType::Select | FieldType::Multiselect => {
				*self.picker_target.borrow_mut() = Some(PickerTarget::Field(field_id));
				self.rebuild_picker();
				let multi = ty == FieldType::Multiselect;
				self.ui.invoke_open_picker(label.into(), multi);
			}
			_ => {}
		}
	}

	fn option_clicked(&self, option_id: String) {
		let target = self.picker_target.borrow().clone();
		let Some(target) = target else { return };
		match target {
			PickerTarget::Field(field_id) => {
				let multi = {
					let data = self.data.borrow();
					data.fields
						.iter()
						.find(|f| f.id == field_id)
						.map(|f| f.ty == FieldType::Multiselect)
						.unwrap_or(false)
				};
				{
					let mut draft = self.draft.borrow_mut();
					let Some(d) = draft.as_mut() else { return };
					let task = &mut d.task;
					if multi {
						let mut selected: Vec<String> = match task.values.get(&field_id) {
							Some(Value::Array(items)) => items
								.iter()
								.filter_map(|v| v.as_str().map(|s| s.to_string()))
								.collect(),
							_ => Vec::new(),
						};
						if let Some(pos) = selected.iter().position(|s| s == &option_id) {
							selected.remove(pos);
						} else {
							selected.push(option_id.clone());
						}
						task.values.insert(
							field_id.clone(),
							Value::Array(selected.into_iter().map(Value::String).collect()),
						);
					} else {
						task.values
							.insert(field_id.clone(), Value::String(option_id.clone()));
					}
				}
				self.rebuild_picker();
				self.refresh_editor_field(&field_id, None);
				if !multi {
					self.ui.invoke_close_picker();
				}
			}
			PickerTarget::Filter(field_id) => {
				let (ty, label) = {
					let data = self.data.borrow();
					match data.fields.iter().find(|f| f.id == field_id) {
						Some(f) => (f.ty, f.label.clone()),
						None => return,
					}
				};
				let mut options = self
					.filters
					.borrow()
					.iter()
					.find(|f| f.field_id == field_id)
					.map(|f| f.options.clone())
					.unwrap_or_default();
				if let Some(pos) = options.iter().position(|s| s == &option_id) {
					options.remove(pos);
				} else {
					options.push(option_id.clone());
				}
				let op = if ty == FieldType::Multiselect {
					FilterOp::HasOption
				} else {
					FilterOp::AnyOf
				};
				if options.is_empty() {
					self.set_filter(&field_id, None);
				} else {
					self.set_filter(
						&field_id,
						Some(Filter {
							field_id: field_id.clone(),
							op,
							text: String::new(),
							options,
						}),
					);
				}
				let _ = label;
				self.rebuild_picker();
				self.rebuild_filters();
				self.rebuild_rows();
			}
		}
	}

	fn set_task_archived(&self, id: String, archived: bool) {
		{
			let mut data = self.data.borrow_mut();
			let Some(task) = data.tasks.iter_mut().find(|t| t.id == id) else {
				return;
			};
			task.archived_at = if archived {
				Some(model::now_iso())
			} else {
				None
			};
			task.updated_at = model::now_iso();
		}
		self.mark_dirty(|d| d.tasks = true);
		self.rebuild_rows();
		self.close_task_form();
	}

	fn ask_delete_task(&self, id: String) {
		let title = {
			let data = self.data.borrow();
			data.tasks
				.iter()
				.find(|t| t.id == id)
				.map(|t| t.title.clone())
				.unwrap_or_default()
		};
		*self.pending_confirm.borrow_mut() = Some(ConfirmAction::DeleteTask(id));
		let label = if title.trim().is_empty() {
			"(untitled task)".to_string()
		} else {
			title
		};
		self.show_confirm(
			"Delete task",
			&format!("\u{201C}{label}\u{201D} will be deleted permanently."),
			"Delete",
		);
	}

	fn date_accepted(&self, year: i32, month: i32, day: i32) {
		let Some(field_id) = self.pending_date_field.borrow_mut().take() else {
			return;
		};
		let text = format!("{year:04}-{month:02}-{day:02}");
		self.edit_field(field_id, text);
	}

	// -------------------------------------------------------------- filters

	fn set_filter(&self, field_id: &str, filter: Option<Filter>) {
		let mut filters = self.filters.borrow_mut();
		filters.retain(|f| f.field_id != field_id);
		if let Some(f) = filter {
			filters.push(f);
		}
	}

	fn filter_text(&self, field_id: String, text: String) {
		let ty = {
			let data = self.data.borrow();
			match data.fields.iter().find(|f| f.id == field_id) {
				Some(f) => f.ty,
				None => return,
			}
		};
		let existing_op = self
			.filters
			.borrow()
			.iter()
			.find(|f| f.field_id == field_id)
			.map(|f| f.op.clone());
		let filter = match ty {
			FieldType::Text | FieldType::Longtext | FieldType::Number => {
				if text.trim().is_empty() {
					None
				} else {
					Some(Filter {
						field_id: field_id.clone(),
						op: FilterOp::Contains,
						text,
						options: Vec::new(),
					})
				}
			}
			FieldType::Date | FieldType::Datetime => {
				let op = existing_op.unwrap_or(FilterOp::Before);
				let op = match op {
					FilterOp::Before | FilterOp::After | FilterOp::Empty | FilterOp::NotEmpty => op,
					_ => FilterOp::Before,
				};
				Some(Filter {
					field_id: field_id.clone(),
					op,
					text,
					options: Vec::new(),
				})
			}
			_ => None,
		};
		self.set_filter(&field_id, filter);
		self.rebuild_rows();
	}

	fn filter_op(&self, field_id: String, index: i32) {
		let ty = {
			let data = self.data.borrow();
			match data.fields.iter().find(|f| f.id == field_id) {
				Some(f) => f.ty,
				None => return,
			}
		};
		let text = self
			.filters
			.borrow()
			.iter()
			.find(|f| f.field_id == field_id)
			.map(|f| f.text.clone())
			.unwrap_or_default();
		let filter = match ty {
			FieldType::Date | FieldType::Datetime => {
				let op = match index {
					1 => FilterOp::After,
					2 => FilterOp::Empty,
					3 => FilterOp::NotEmpty,
					_ => FilterOp::Before,
				};
				Some(Filter {
					field_id: field_id.clone(),
					op,
					text,
					options: Vec::new(),
				})
			}
			FieldType::Checkbox => match index {
				1 => Some(Filter {
					field_id: field_id.clone(),
					op: FilterOp::IsTrue,
					text: String::new(),
					options: Vec::new(),
				}),
				2 => Some(Filter {
					field_id: field_id.clone(),
					op: FilterOp::IsFalse,
					text: String::new(),
					options: Vec::new(),
				}),
				_ => None,
			},
			_ => None,
		};
		self.set_filter(&field_id, filter);
		self.rebuild_rows();
	}

	fn open_filter_pick(&self, field_id: String) {
		let label = {
			let data = self.data.borrow();
			match data.fields.iter().find(|f| f.id == field_id) {
				Some(f) => f.label.clone(),
				None => return,
			}
		};
		*self.picker_target.borrow_mut() = Some(PickerTarget::Filter(field_id));
		self.rebuild_picker();
		self.ui
			.invoke_open_picker(format!("Filter: {label}").into(), true);
	}

	fn clear_filters(&self) {
		self.filters.borrow_mut().clear();
		self.rebuild_filters();
		self.rebuild_rows();
	}

	// ------------------------------------------------------- fields settings

	fn list_names(&self) -> Vec<SharedString> {
		self.data
			.borrow()
			.lists
			.iter()
			.filter(|l| !l.archived)
			.map(|l| SharedString::from(l.name.clone()))
			.collect()
	}

	fn field_modal_new(&self) {
		*self.modal_editing.borrow_mut() = None;
		self.ui.set_modal_kind(1);
		self.ui.set_modal_title("New field".into());
		self.ui.set_modal_text("".into());
		self.ui.set_modal_type_index(0);
		self.ui.set_modal_type_locked(false);
		self.ui.set_modal_list_names(ModelRc::from(Rc::new(VecModel::from(
			self.list_names(),
		))));
		self.ui.set_modal_list_index(0);
		self.ui.set_modal_required(false);
		self.ui.set_modal_show_in_list(true);
		self.ui.set_modal_error("".into());
		self.ui.set_modal_height(310.0);
		self.ui.set_modal_open(true);
	}

	fn field_modal_edit(&self, field_id: String) {
		let (label, ty, list_index, required, show_in_list, has_values) = {
			let data = self.data.borrow();
			let Some(field) = data.fields.iter().find(|f| f.id == field_id) else {
				return;
			};
			let active_lists: Vec<&str> = data
				.lists
				.iter()
				.filter(|l| !l.archived)
				.map(|l| l.id.as_str())
				.collect();
			let list_index = field
				.list_id
				.as_ref()
				.and_then(|id| active_lists.iter().position(|l| l == id))
				.unwrap_or(0) as i32;
			let has_values = data.tasks.iter().any(|t| {
				t.values
					.get(&field_id)
					.map(|v| !values::is_empty_value(v))
					.unwrap_or(false)
			});
			(
				field.label.clone(),
				field.ty,
				list_index,
				field.required,
				field.show_in_list,
				has_values,
			)
		};
		*self.modal_editing.borrow_mut() = Some(field_id);
		self.ui.set_modal_kind(1);
		self.ui.set_modal_title("Edit field".into());
		self.ui.set_modal_text(label.into());
		self.ui.set_modal_type_index(values::kind_of(ty));
		self.ui.set_modal_type_locked(has_values);
		self.ui.set_modal_list_names(ModelRc::from(Rc::new(VecModel::from(
			self.list_names(),
		))));
		self.ui.set_modal_list_index(list_index);
		self.ui.set_modal_required(required);
		self.ui.set_modal_show_in_list(show_in_list);
		self.ui.set_modal_error("".into());
		self.ui
			.set_modal_height(if ty.needs_list() { 365.0 } else { 310.0 });
		self.ui.set_modal_open(true);
	}

	fn field_save(&self) {
		let label = self.ui.get_modal_text().to_string().trim().to_string();
		let ty = type_from_index(self.ui.get_modal_type_index());
		let list_index = self.ui.get_modal_list_index().max(0) as usize;
		let required = self.ui.get_modal_required();
		let show_in_list = self.ui.get_modal_show_in_list();
		let editing = self.modal_editing.borrow().clone();

		if label.is_empty() {
			self.ui.set_modal_error("Give the field a name.".into());
			return;
		}

		let list_id = if ty.needs_list() {
			let lists: Vec<String> = {
				let data = self.data.borrow();
				data.lists
					.iter()
					.filter(|l| !l.archived)
					.map(|l| l.id.clone())
					.collect()
			};
			match lists.get(list_index) {
				Some(id) => Some(id.clone()),
				None => {
					self.ui.set_modal_error(
						"Create a dropdown list first (Settings \u{2192} Lists).".into(),
					);
					return;
				}
			}
		} else {
			None
		};

		let mut remove_values = false;
		if let Some(field_id) = editing {
			let locked = {
				let data = self.data.borrow();
				data.tasks.iter().any(|t| {
					t.values
						.get(&field_id)
						.map(|v| !values::is_empty_value(v))
						.unwrap_or(false)
				})
			};
			let type_changed = {
				let data = self.data.borrow();
				data.fields
					.iter()
					.find(|f| f.id == field_id)
					.map(|f| f.ty != ty)
					.unwrap_or(false)
			};
			if type_changed && locked {
				self.ui.set_modal_error(
					"This field already has values, so its type cannot change.".into(),
				);
				return;
			}
			remove_values = type_changed;
			{
				let mut data = self.data.borrow_mut();
				let Some(field) = data.fields.iter_mut().find(|f| f.id == field_id) else {
					return;
				};
				field.label = label;
				field.ty = ty;
				field.list_id = list_id;
				field.required = required;
				field.show_in_list = show_in_list;
			}
			if remove_values {
				let mut data = self.data.borrow_mut();
				for task in data.tasks.iter_mut() {
					task.values.remove(&field_id);
				}
			}
		} else {
			let field = FieldDef {
				id: model::new_id(),
				label,
				ty,
				required,
				default_value: None,
				list_id,
				help: None,
				show_in_list,
				archived: false,
			};
			self.data.borrow_mut().fields.push(field);
		}

		self.mark_dirty(|d| {
			d.fields = true;
			if remove_values {
				d.tasks = true;
			}
		});
		self.ui.set_modal_open(false);
		self.rebuild_settings_fields();
		self.rebuild_columns();
		self.rebuild_rows();
		self.rebuild_filters();
		self.rebuild_editor();
	}

	fn field_move(&self, field_id: String, direction: i32) {
		{
			let mut data = self.data.borrow_mut();
			let Some(index) = data.fields.iter().position(|f| f.id == field_id) else {
				return;
			};
			let target = index as i32 + direction;
			if target < 0 || target as usize >= data.fields.len() {
				return;
			}
			data.fields.swap(index, target as usize);
		}
		self.mark_dirty(|d| d.fields = true);
		self.rebuild_settings_fields();
		self.rebuild_columns();
		self.rebuild_rows();
		self.rebuild_editor();
	}

	fn field_archived(&self, field_id: String, archived: bool) {
		{
			let mut data = self.data.borrow_mut();
			let Some(field) = data.fields.iter_mut().find(|f| f.id == field_id) else {
				return;
			};
			field.archived = archived;
		}
		self.mark_dirty(|d| d.fields = true);
		self.rebuild_settings_fields();
		self.rebuild_columns();
		self.rebuild_rows();
		self.rebuild_filters();
		self.rebuild_editor();
	}

	fn field_toggle(&self, field_id: String, value: bool, required: bool) {
		{
			let mut data = self.data.borrow_mut();
			let Some(field) = data.fields.iter_mut().find(|f| f.id == field_id) else {
				return;
			};
			if required {
				field.required = value;
			} else {
				field.show_in_list = value;
			}
		}
		self.mark_dirty(|d| d.fields = true);
		self.rebuild_settings_fields();
		if !required {
			self.rebuild_columns();
			self.rebuild_rows();
		}
		self.rebuild_editor();
	}

	fn ask_delete_field(&self, field_id: String) {
		let label = {
			let data = self.data.borrow();
			data.fields
				.iter()
				.find(|f| f.id == field_id)
				.map(|f| f.label.clone())
				.unwrap_or_default()
		};
		*self.pending_confirm.borrow_mut() = Some(ConfirmAction::DeleteField(field_id));
		self.show_confirm(
			"Delete field",
			&format!(
				"\u{201C}{label}\u{201D} and all values stored for it will be deleted permanently."
			),
			"Delete",
		);
	}

	// -------------------------------------------------------- lists settings

	fn list_add(&self, name: String) {
		let name = name.trim().to_string();
		if name.is_empty() {
			return;
		}
		self.data.borrow_mut().lists.push(ListDef {
			id: model::new_id(),
			name,
			archived: false,
			options: Vec::new(),
		});
		self.mark_dirty(|d| d.lists = true);
		self.rebuild_lists();
	}

	fn list_rename(&self, list_id: String, name: String) {
		if name.trim().is_empty() {
			return;
		}
		{
			let mut data = self.data.borrow_mut();
			let Some(list) = data.lists.iter_mut().find(|l| l.id == list_id) else {
				return;
			};
			list.name = name;
		}
		self.mark_dirty(|d| d.lists = true);
		self.rebuild_settings_fields();
	}

	fn list_move(&self, list_id: String, direction: i32) {
		{
			let mut data = self.data.borrow_mut();
			let Some(index) = data.lists.iter().position(|l| l.id == list_id) else {
				return;
			};
			let target = index as i32 + direction;
			if target < 0 || target as usize >= data.lists.len() {
				return;
			}
			data.lists.swap(index, target as usize);
		}
		self.mark_dirty(|d| d.lists = true);
		self.rebuild_lists();
	}

	fn list_archived(&self, list_id: String, archived: bool) {
		{
			let mut data = self.data.borrow_mut();
			let Some(list) = data.lists.iter_mut().find(|l| l.id == list_id) else {
				return;
			};
			list.archived = archived;
		}
		self.mark_dirty(|d| d.lists = true);
		self.rebuild_lists();
	}

	fn ask_delete_list(&self, list_id: String) {
		let (name, used_by) = {
			let data = self.data.borrow();
			let name = data
				.lists
				.iter()
				.find(|l| l.id == list_id)
				.map(|l| l.name.clone())
				.unwrap_or_default();
			let used_by = data
				.fields
				.iter()
				.filter(|f| f.list_id.as_deref() == Some(list_id.as_str()))
				.count();
			(name, used_by)
		};
		let text = if used_by > 0 {
			format!(
				"\u{201C}{name}\u{201D} is used by {used_by} field(s); they will keep their values but lose the list."
			)
		} else {
			format!("\u{201C}{name}\u{201D} will be deleted permanently.")
		};
		*self.pending_confirm.borrow_mut() = Some(ConfirmAction::DeleteList(list_id));
		self.show_confirm("Delete list", &text, "Delete");
	}

	fn option_add(&self, list_id: String, label: String) {
		let label = label.trim().to_string();
		if label.is_empty() {
			return;
		}
		{
			let mut data = self.data.borrow_mut();
			let Some(list) = data.lists.iter_mut().find(|l| l.id == list_id) else {
				return;
			};
			let color = OPTION_COLORS[list.options.len() % OPTION_COLORS.len()].to_string();
			list.options.push(OptionDef {
				id: model::new_id(),
				label,
				color: Some(color),
				archived: false,
			});
		}
		self.mark_dirty(|d| d.lists = true);
		self.rebuild_lists();
	}

	fn option_rename(&self, option_id: String, label: String) {
		if label.trim().is_empty() {
			return;
		}
		{
			let mut data = self.data.borrow_mut();
			let Some(option) = data
				.lists
				.iter_mut()
				.flat_map(|l| l.options.iter_mut())
				.find(|o| o.id == option_id)
			else {
				return;
			};
			option.label = label;
		}
		self.mark_dirty(|d| d.lists = true);
	}

	fn option_color(&self, option_id: String, hex: String) {
		let hex = hex.trim().to_string();
		let normalized = if hex.starts_with('#') {
			hex.clone()
		} else {
			format!("#{hex}")
		};
		if hex_color(&normalized).is_none() {
			return;
		}
		{
			let mut data = self.data.borrow_mut();
			let Some(option) = data
				.lists
				.iter_mut()
				.flat_map(|l| l.options.iter_mut())
				.find(|o| o.id == option_id)
			else {
				return;
			};
			option.color = Some(normalized);
		}
		self.mark_dirty(|d| d.lists = true);
		self.rebuild_lists();
		self.rebuild_editor();
	}

	fn option_move(&self, option_id: String, direction: i32) {
		{
			let mut data = self.data.borrow_mut();
			for list in data.lists.iter_mut() {
				if let Some(index) = list.options.iter().position(|o| o.id == option_id) {
					let target = index as i32 + direction;
					if target < 0 || target as usize >= list.options.len() {
						return;
					}
					list.options.swap(index, target as usize);
					break;
				}
			}
		}
		self.mark_dirty(|d| d.lists = true);
		self.rebuild_lists();
		self.rebuild_editor();
	}

	fn option_archived(&self, option_id: String, archived: bool) {
		{
			let mut data = self.data.borrow_mut();
			let Some(option) = data
				.lists
				.iter_mut()
				.flat_map(|l| l.options.iter_mut())
				.find(|o| o.id == option_id)
			else {
				return;
			};
			option.archived = archived;
		}
		self.mark_dirty(|d| d.lists = true);
		self.rebuild_lists();
		self.rebuild_editor();
	}

	fn ask_delete_option(&self, option_id: String) {
		let label = {
			let data = self.data.borrow();
			data.lists
				.iter()
				.flat_map(|l| l.options.iter())
				.find(|o| o.id == option_id)
				.map(|o| o.label.clone())
				.unwrap_or_default()
		};
		*self.pending_confirm.borrow_mut() = Some(ConfirmAction::DeleteOption(option_id));
		self.show_confirm(
			"Delete option",
			&format!("\u{201C}{label}\u{201D} will be deleted permanently."),
			"Delete",
		);
	}

	// ----------------------------------------------------------- data screen

	fn show_confirm(&self, title: &str, text: &str, confirm_label: &str) {
		self.ui.set_modal_kind(2);
		self.ui.set_modal_title(title.into());
		self.ui.set_modal_text(text.into());
		self.ui.set_modal_confirm_label(confirm_label.into());
		self.ui.set_modal_height(220.0);
		self.ui.set_modal_open(true);
	}

	fn confirm_yes(&self) {
		let Some(action) = self.pending_confirm.borrow_mut().take() else {
			return;
		};
		match action {
			ConfirmAction::DeleteTask(id) => {
				{
					let mut data = self.data.borrow_mut();
					data.tasks.retain(|t| t.id != id);
				}
				if self.selected.borrow().as_deref() == Some(id.as_str()) {
					self.close_editor();
				}
				self.mark_dirty(|d| d.tasks = true);
				self.rebuild_rows();
			}
			ConfirmAction::DeleteField(id) => {
				{
					let mut data = self.data.borrow_mut();
					data.fields.retain(|f| f.id != id);
					for task in data.tasks.iter_mut() {
						task.values.remove(&id);
					}
				}
				self.mark_dirty(|d| {
					d.fields = true;
					d.tasks = true;
				});
				self.rebuild_settings_fields();
				self.rebuild_columns();
				self.rebuild_rows();
				self.rebuild_filters();
				self.rebuild_editor();
			}
			ConfirmAction::DeleteList(id) => {
				{
					let mut data = self.data.borrow_mut();
					data.lists.retain(|l| l.id != id);
					for field in data.fields.iter_mut() {
						if field.list_id.as_deref() == Some(id.as_str()) {
							field.list_id = None;
						}
					}
				}
				self.mark_dirty(|d| {
					d.lists = true;
					d.fields = true;
				});
				self.rebuild_lists();
				self.rebuild_settings_fields();
				self.rebuild_editor();
			}
			ConfirmAction::DeleteOption(option_id) => {
				{
					let mut data = self.data.borrow_mut();
					for list in data.lists.iter_mut() {
						list.options.retain(|o| o.id != option_id);
					}
					cleanup_option_values(&mut data, &option_id);
				}
				self.mark_dirty(|d| {
					d.lists = true;
					d.tasks = true;
				});
				self.rebuild_lists();
				self.rebuild_rows();
				self.rebuild_filters();
				self.rebuild_editor();
			}
			ConfirmAction::ImportFrom(dir) => {
				self.do_import(dir);
			}
		}
	}

	fn open_folder(&self, path: PathBuf) {
		let _ = std::fs::create_dir_all(&path);
		let _ = std::process::Command::new("explorer")
			.arg(path.as_os_str())
			.spawn();
	}

	fn export_data(&self) {
		let Some(dir) = rfd::FileDialog::new()
			.set_title("Choose a folder for the export")
			.pick_folder()
		else {
			return;
		};
		match self.store.export_to(&dir) {
			Ok(()) => self
				.ui
				.set_status_text(format!("Exported to {}", dir.display()).into()),
			Err(err) => self.ui.set_status_text(format!("Export failed: {err}").into()),
		}
	}

	fn ask_import(&self) {
		let Some(dir) = rfd::FileDialog::new()
			.set_title("Choose a FocusBoard export folder")
			.pick_folder()
		else {
			return;
		};
		*self.pending_confirm.borrow_mut() = Some(ConfirmAction::ImportFrom(dir.clone()));
		self.show_confirm(
			"Import data",
			&format!(
				"Replace the current data with the export in\n{}. A backup of the current data is kept.",
				dir.display()
			),
			"Import",
		);
	}

	fn do_import(&self, dir: PathBuf) {
		if let Err(err) = self.store.import_from(&dir) {
			self.ui
				.set_status_text(format!("Import failed: {err}").into());
			return;
		}
		match self.store.load() {
			Ok(loaded) => {
				{
					let mut data = self.data.borrow_mut();
					*data = loaded.data;
				}
				*self.selected.borrow_mut() = None;
				self.ui.set_has_selection(false);
				self.ui.set_selected_id("".into());
				self.filters.borrow_mut().clear();
				self.rebuild_all();
				self.ui.set_status_text("Imported".into());
			}
			Err(err) => self
				.ui
				.set_status_text(format!("Import failed: {err}").into()),
		}
	}

	fn set_theme(&self, index: i32) {
		let theme = theme_from_index(index);
		self.data.borrow_mut().settings.theme = theme;
		self.mark_dirty(|d| d.settings = true);
		self.apply_theme(theme);
	}

	fn set_sidebar_collapsed(&self, collapsed: bool) {
		self.data.borrow_mut().settings.sidebar_collapsed = collapsed;
		self.mark_dirty(|d| d.settings = true);
	}
}

fn option_color_of(field: &FieldDef, value: &Value, data: &DataRoot) -> Option<Color> {
	let list = field
		.list_id
		.as_ref()
		.and_then(|id| data.lists.iter().find(|l| &l.id == id))?;
	let id = match value {
		Value::String(s) => Some(s.as_str()),
		Value::Array(items) if items.len() == 1 => items[0].as_str(),
		_ => None,
	}?;
	let option = list.option(id)?;
	Some(color_of(option.color.as_ref()))
}

fn summary_of_options(field: &FieldDef, selected: &[String], data: &DataRoot) -> String {
	if selected.is_empty() {
		return String::new();
	}
	let list = field
		.list_id
		.as_ref()
		.and_then(|id| data.lists.iter().find(|l| &l.id == id));
	let labels: Vec<String> = selected
		.iter()
		.map(|id| {
			list.and_then(|l| l.option(id))
				.map(|o| o.label.clone())
				.unwrap_or_else(|| "(missing)".to_string())
		})
		.collect();
	if labels.len() > 3 {
		format!("{} +{} more", labels[..3].join(", "), labels.len() - 3)
	} else {
		labels.join(", ")
	}
}

fn field_picker_options(field: &FieldDef, value: &Value, data: &DataRoot) -> Vec<PickOption> {
	let Some(list) = field
		.list_id
		.as_ref()
		.and_then(|id| data.lists.iter().find(|l| &l.id == id))
	else {
		return Vec::new();
	};
	let selected: Vec<String> = match value {
		Value::String(s) => vec![s.clone()],
		Value::Array(items) => items
			.iter()
			.filter_map(|v| v.as_str().map(|s| s.to_string()))
			.collect(),
		_ => Vec::new(),
	};
	list.options
		.iter()
		.filter(|o| !o.archived || selected.contains(&o.id))
		.map(|o| PickOption {
			id: o.id.clone().into(),
			label: o.label.clone().into(),
			swatch: color_of(o.color.as_ref()),
			selected: selected.contains(&o.id),
			archived: o.archived,
		})
		.collect()
}

fn list_picker_options(field: &FieldDef, selected: &[String], data: &DataRoot) -> Vec<PickOption> {
	let Some(list) = field
		.list_id
		.as_ref()
		.and_then(|id| data.lists.iter().find(|l| &l.id == id))
	else {
		return Vec::new();
	};
	list.options
		.iter()
		.filter(|o| !o.archived || selected.contains(&o.id))
		.map(|o| PickOption {
			id: o.id.clone().into(),
			label: o.label.clone().into(),
			swatch: color_of(o.color.as_ref()),
			selected: selected.contains(&o.id),
			archived: o.archived,
		})
		.collect()
}

/// Remove a deleted option from every task value that references it.
fn cleanup_option_values(data: &mut DataRoot, option_id: &str) {
	for task in data.tasks.iter_mut() {
		for field in data.fields.iter() {
			if !field.ty.needs_list() {
				continue;
			}
			if let Some(value) = task.values.get_mut(&field.id) {
				match value {
					Value::String(s) if s == option_id => {
						task.values.remove(&field.id);
					}
					Value::Array(items) => {
						items.retain(|v| v.as_str() != Some(option_id));
					}
					_ => {}
				}
			}
		}
	}
}

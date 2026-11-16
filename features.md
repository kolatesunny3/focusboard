# FocusBoard — MVP features

A lightweight Windows task planner where the *shape of a task is user-defined*.
Offline, local-only, no account.

## Core concepts

| Concept | Meaning |
|---|---|
| Task | id + title + description + values (one per field) + timestamps + archived |
| Field | a user-defined attribute: label, type, required, default, help text, show-in-list, order, archived |
| Dropdown master | reusable list of options; one master can feed many fields |
| Option | label + color + position + archived flag inside a master |
| Value | what a task holds for a field; encoding depends on the field type |

## Field types

| Type | Stored as | Notes |
|---|---|---|
| Single-line text | string | |
| Long text | string | multiline editor |
| Number | number | numeric only |
| Date | `"YYYY-MM-DD"` | no timezone shift |
| Date & time | ISO 8601 UTC string | displayed in local time |
| Checkbox | true / false | |
| Dropdown | option id | archived/missing options shown as "(archived)"/"(missing)" |
| Multi-select | array of option ids | picker with checkboxes |

## Field rules

- id is immutable; label is editable.
- Type can only be changed while no task has a value for the field.
- Order = position in the list; changed with up/down buttons (Settings lists keep
  buttons; the tasks grid is dragged).
- Archive hides the field from the task editor and column chooser; values survive.
  Restore at any time.
- Delete is only available for archived fields, with confirmation; it permanently
  removes that field's values from all tasks.
- required is enforced while editing. Adding a required field later does not
  rewrite existing tasks — the editor flags the missing value.
- default value applies to new tasks only.
- show-in-list adds a column; max 5 custom columns in MVP.
- select/multiselect must reference a master; changing the master is blocked once
  any value exists.

## Dropdown masters

- Defined once in `lists.json`, referenced by any number of fields.
- Renaming an option updates every task automatically (values store option ids).
- Archiving an option hides it from pickers but existing tasks still render its
  label with an "(archived)" marker.
- Deleting an option is allowed only when no task uses it.
- Deleting a master is blocked while any field (active or archived) references it.
- Each option can carry a color from a preset palette; shown as a dot/chip in
  tables and pickers.

## Tasks

- Fields: title (required), description, values, createdAt, updatedAt, archivedAt.
- Quick add: type a title, press Enter — task is created, focus stays for the next one.
- Editor: "New task" or clicking a task opens a **modal form** with the title,
  description and all fields in configured order. Save creates or updates the
  task; Cancel (or Close) discards the draft. Nothing is stored until Save, and an
  empty title is rejected with an inline message.
- Single-select fields (Status, Priority) use an inline dropdown in the form.
  Multi-select keeps its checkbox picker; date fields keep the calendar picker.
- Autosave: ~700 ms after Save (or any other change), flushed when the app
  closes. A status indicator shows Saving / Saved.
- Invalid input shows an inline error and is not persisted.
- Archive and restore; permanent delete requires confirmation.

## List view

- Search: title + description.
- Columns: title always visible; up to 5 custom fields via show-in-list.
- Sort: clicking a header cycles ascending → descending → manual. Manual is the
  default: it shows the tasks in stored order, i.e. the order they were dragged
  into. Sortable: title, created, updated, text, number, date, and dropdown (by
  option order). Checkbox and multi-select are not sortable in MVP.
- Manual order (priority): drag a row towards where it belongs. The dragged row
  dims, a blue line marks the slot it would land in, and dropping switches the list
  to manual order — a column sort in effect is dropped, because the arrangement the
  user just made is the one they want to keep. The order is saved in `tasks.json`
  (the order of the tasks in the file) and survives a restart. Dragging works with a
  search or filter active: only the rows on screen move, hidden tasks keep their
  place. Archived rows always stay below the active ones.
- Column widths: title 5 parts, each custom column 2, computed from one shared
  formula for the header and every row (`Theme.grid-x`/`grid-width`). A long title
  or value is elided with "…" rather than pushing the following columns to the
  right, so values always line up under their headers. Dropping values are drawn as
  rounded chips tinted with the option colour.
- Filters, combined with AND:
  - text / number: contains, equals, empty
  - date: before, after, empty
  - dropdown: any of selected options
  - multi-select: contains option, empty
  - checkbox: any / on / off
- "Show archived" toggle; archived rows are listed after the active ones and
  carry an "archived" marker.
- The counter reads "shown / total": shown = rows on screen, total = every task
  including archived ones.

## Window and theme

- Window size/position are remembered between runs; both are clamped to the
  screen's work area, so the window always reopens on-screen, and a first run is
  centred. A maximized window reopens maximized; the last restored (non-maximized)
  size is kept for un-maximizing.
- Minimum window: 830 × 520 logical pixels (the table and the date-picker popup
  need that much room); the window cannot be resized smaller.
- Theme: follow system / light / dark, applied immediately and persisted.
- The left sidebar collapses to an icon rail with the «/» button (full width
  200 px → 68 px); the collapsed state is remembered between runs.
- Appearance: the FocusBoard logo (`fb.png`) is the window/taskbar/exe icon and
  appears in the sidebar; indigo accent on soft neutral surfaces — sidebar with
  pill navigation, white cards with hairline borders, uppercase muted table headers,
  hairline row separators, accent-tinted selected row with a left marker, coloured
  chips for dropdown values, primary/quiet/danger buttons. Every colour token has a
  light and a dark value; "follow system" is resolved from Windows at startup and
  whenever the theme is changed.

## Seeded on first run (fully editable, like any user data)

- Master "Status": To Do, In Progress, Done → field "Status" (dropdown, default
  "To Do", shown in list).
- Master "Priority": Low, Medium, High → field "Priority" (dropdown, shown in list).
- Field "Due date" (date, shown in list).

## Screens

1. **Tasks** — toolbar (search, filter popover, archived toggle, New task button),
   quick-add row, table with configurable columns, and a modal task form for
   create/edit. An empty table explains itself: "no tasks yet" on a fresh board,
   "no tasks match the current search or filters" when a filter hides everything.
2. **Settings → Fields** — list of fields with type, required, show-in-list,
   archived; add/edit dialog; up/down reorder; archive/restore/delete.
3. **Settings → Lists** — masters with option editors (label, color, order,
   archive/delete); add master, add option.
4. **Settings → Data** — data folder path, open data folder, open backups,
   export copy, import/restore, theme (system / light / dark).

## Storage (summary)

Four JSON files in `%LOCALAPPDATA%\FocusBoard\`: `lists.json`, `fields.json`,
`tasks.json`, `settings.json` — atomic writes, a `.bak` per file, daily backups
(keep 7), `schemaVersion` per file, single instance. Details and the rules that
must not be broken: `agents.md`.

## Acceptance checklist

Checked items were exercised on 2026-10-19 with a mouse-driven smoke test (create
task, quick-add typing, option picker, date picker, filters, archive/restore,
delete confirmation, theme switch, window restore, drag & drop reorder, long-title
column alignment) plus `cargo test`/`clippy`. The restyle was re-checked on
2026-10-21 in both light and dark themes (grid alignment measured from captures,
drag & drop re-verified against a byte-identical `tasks.json` afterwards), and the
modal task form was re-checked on 2026-11-16 (create, validation, dropdown choice,
edit prefill, Cancel, Save, delete), with `tasks.json` restored byte-identically.
The collapsible sidebar, its persistence and the icon were also checked on
2026-11-16, including the icon extracted from the shipping exe.

- [x] Fresh launch creates the four JSON files.
- [x] Create a task, restart the app — data persists.
- [x] "New task" opens a form and creates nothing until Save; an empty title is
      rejected and the form stays open.
- [x] Clicking a row opens the same form pre-filled; Cancel discards, Save persists.
- [x] Status / Priority are inline dropdowns in the form (not a separate popup).
- [x] Sidebar collapses to an icon rail and stays collapsed after a restart; the
      FocusBoard logo shows in the sidebar, title bar, taskbar and Explorer.
- [ ] Add a master "Client" with options; add a field "Client" using it; set a
      value; show it as a column; filter by it.
- [ ] Add a date field, sort ascending and descending by it.
- [x] Drag a row to another position — the drop slot is marked, the order sticks
      after a restart and the header sort arrow clears (manual order).
- [x] A long title in one row — the columns after it, and their headers, stay
      aligned (header and rows share `Theme.grid-x`); archived rows line up too.
- [ ] Rename an option — every task shows the new label.
- [ ] Archive a field: gone from the editor, values preserved; restore brings it back.
- [ ] Type change is blocked once values exist.
- [x] Kill the process after a save — data intact on restart.
- [ ] Corrupt `tasks.json` — app falls back to `tasks.json.bak` with a message.
- [ ] Import an invalid file — rejected, existing data untouched.
- [ ] Start a second instance — it does not run concurrently.
- [ ] Export folder → wipe → import folder: full round-trip.
- [~] Release exe ≤ 8 MB; cold start ≤ 200 ms; idle RAM ≤ 80 MB.
      Cold start (170 ms median) and RAM (31 MB) pass; the exe is 9.75 MB
      (see `agents.md` → Budget results).

## Out of scope for MVP

Cloud sync, accounts, mobile/web, Kanban or calendar views, reminders and
notifications, recurring tasks, subtasks, attachments, bulk edit, saved filter
views, CSV import/export, collaboration, formulas, auto-update, tray icon or
global hotkey, i18n.

## Roadmap (after MVP)

1. Kanban board grouped by any dropdown field.
2. Saved filter views.
3. Bulk edit with multi-select rows.
4. CSV import; single-file JSON export.
5. Reminders (Windows notifications) and tray icon.
6. Undo/redo.
7. Portable mode (data beside the exe).
8. Accessibility pass (Slint AccessKit).

## Assumptions

- Single user, personal machine, Windows 10/11.
- English-only UI.
- Slint licensing must be resolved before any public, closed-source distribution.

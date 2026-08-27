//! Interactive `/config` panel.
//!
//! Lists the same allow-list that backs the model-callable `Config` tool
//! ([`agent_code_lib::config::supported_settings`]) and lets the user edit
//! each value with arrow keys. Edits are written through the same
//! read-modify-write path the tool uses (one dotted key into the scope's
//! TOML file), so unrelated sections, comments, and — critically — any
//! secrets elsewhere in the file are left untouched.

use std::path::{Path, PathBuf};

use crossterm::event::{self, KeyCode, KeyEventKind};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};
use toml::Value;

use agent_code_lib::config::Config;
use agent_code_lib::config::atomic::atomic_write_secret;
use agent_code_lib::config::supported_settings::{
    SUPPORTED_SETTINGS, Scope, SettingKind, SupportedSetting,
};
use agent_code_lib::error::ConfigError;
use agent_code_lib::query::QueryEngine;

use crate::ui::modern::colors::palette;
use crate::ui::modern::render::{draw_modal_box, key_hint_line};

/// A single editable setting row.
struct ConfigItem {
    setting: &'static SupportedSetting,
    /// Current value from the live config, or `None` when the key is
    /// absent (every allow-list key has a default, so this is rare).
    value: Option<Value>,
}

/// Plain (unstyled) rendered row produced by [`ConfigPanel::rows`].
struct RowView<'a> {
    selected: bool,
    marker: &'static str,
    key: &'a str,
    key_pad: usize,
    value_shown: String,
    value_pad: usize,
    /// Which scope group this row belongs to, so the renderer can inject a
    /// single group-header line at scope transitions instead of repeating a
    /// `[scope]` tag on every row (saves a column).
    scope: Scope,
}

/// Outcome of a key press handled by the [`ValuePicker`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum PickerResult {
    /// Still open; key consumed, keep rendering.
    StillOpen,
    /// A value was confirmed.
    Chosen(String),
    /// Cancelled (Esc / `q` / Ctrl+C).
    Cancelled,
}

/// A nested ratatui modal that lets the user pick one of a setting's allowed
/// `enum` values (or toggle among them). Mirrors the model/provider pickers:
/// a centered bordered box, `❯` marker, accent highlight, and a sticky footer.
struct ValuePicker {
    options: Vec<String>,
    selected: usize,
    /// Value currently shown in the parent row, so a `✔` marks it like the
    /// model/provider pickers mark the active entry.
    current: String,
    /// Index of the `ConfigItem` this picker edits, so the run loop can
    /// commit the chosen value back to the right row.
    source_idx: usize,
}

impl ValuePicker {
    fn new(allowed: &[&'static str], current: String, source_idx: usize) -> Self {
        let options: Vec<String> = allowed.iter().map(|s| s.to_string()).collect();
        let selected = options.iter().position(|o| *o == current).unwrap_or(0);
        Self {
            options,
            selected,
            current,
            source_idx,
        }
    }

    /// Handle one key; returns [`PickerResult`] describing what happened.
    fn on_key(&mut self, code: KeyCode) -> PickerResult {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                PickerResult::StillOpen
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < self.options.len() {
                    self.selected += 1;
                }
                PickerResult::StillOpen
            }
            KeyCode::Enter => PickerResult::Chosen(self.options[self.selected].clone()),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('\u{3}') => PickerResult::Cancelled,
            _ => PickerResult::StillOpen,
        }
    }

    /// Draw the picker into a ratatui frame, reusing the shared modal box
    /// (same centered border + sticky footer the model/provider pickers use).
    fn render(&self, frame: &mut Frame<'_>, area: Rect) {
        let t = palette();
        let mut lines: Vec<Line<'static>> = Vec::new();
        for (i, opt) in self.options.iter().enumerate() {
            let is_sel = i == self.selected;
            let marker = if is_sel { "❯" } else { " " };
            let style = if is_sel {
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.inactive)
            };
            let cur = if *opt == self.current { " ✔" } else { "" };
            lines.push(Line::from(Span::styled(
                format!("{marker} {opt}{cur}"),
                style,
            )));
        }
        draw_modal_box(
            frame,
            area,
            lines,
            " value ",
            t.accent,
            Some(key_hint_line("[↑↓] move   [Enter] choose   [Esc/q] cancel")),
        );
    }
}

impl ConfigItem {
    fn new(setting: &'static SupportedSetting, config: &Config) -> Self {
        Self {
            setting,
            value: Self::read(setting, config),
        }
    }

    /// Walk the dotted key inside the live config (serialized to TOML).
    fn read(setting: &'static SupportedSetting, config: &Config) -> Option<Value> {
        let root = toml::Value::try_from(config).ok()?;
        let mut cur = &root;
        for part in setting.key.split('.') {
            cur = cur.get(part)?;
        }
        Some(cur.clone())
    }

    fn display(&self) -> String {
        match &self.value {
            Some(Value::Boolean(b)) => b.to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => "(unset)".to_string(),
        }
    }
}

/// Full-screen interactive configuration panel.
pub struct ConfigPanel {
    items: Vec<ConfigItem>,
    cursor: usize,
    /// When editing an `enum` setting, the in-progress value picker overlay
    /// (its own ratatui modal, like the model/provider pickers).
    value_picker: Option<ValuePicker>,
}

/// Human label for a [`Scope`], used by the group headers.
fn scope_label(scope: Scope) -> &'static str {
    match scope {
        Scope::User => "user",
        Scope::Project => "project",
    }
}
impl ConfigPanel {
    pub fn new(engine: &QueryEngine) -> Self {
        let config = &engine.state().config;
        let items = SUPPORTED_SETTINGS
            .iter()
            .map(|s| ConfigItem::new(s, config))
            .collect();
        Self {
            items,
            cursor: 0,
            value_picker: None,
        }
    }

    pub fn run(mut self, engine: &mut QueryEngine) -> Result<(), ConfigError> {
        enable_raw();
        let mut terminal = match Terminal::new(CrosstermBackend::new(std::io::stdout())) {
            Ok(t) => t,
            Err(e) => {
                disable_raw();
                return Err(ConfigError::FileError(format!("init terminal: {e}")));
            }
        };
        loop {
            if let Err(e) = terminal.draw(|f| self.render(f, f.area())) {
                disable_raw();
                return Err(ConfigError::FileError(format!("render: {e}")));
            }
            let key = match event::read() {
                Ok(crossterm::event::Event::Key(key))
                    if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat =>
                {
                    key
                }
                Ok(_) => continue,
                Err(e) => {
                    disable_raw();
                    return Err(ConfigError::FileError(format!("input read: {e}")));
                }
            };
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if self.cursor > 0 {
                        self.cursor -= 1;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.cursor + 1 < self.items.len() {
                        self.cursor += 1;
                    }
                }
                KeyCode::Home => self.cursor = 0,
                KeyCode::End => {
                    self.cursor = self.items.len().saturating_sub(1);
                }
                KeyCode::PageUp => {
                    self.cursor = self.cursor.saturating_sub(10);
                }
                KeyCode::PageDown => {
                    self.cursor = (self.cursor + 10).min(self.items.len().saturating_sub(1));
                }
                KeyCode::Esc | KeyCode::Char('q') => break,
                KeyCode::Enter => {
                    // `open_edit` may open a nested value picker; drive that
                    // sub-loop inline so the panel and the picker never share
                    // a single `event::read()`.
                    self.open_edit(engine)?;
                    while self.value_picker.is_some() {
                        if let Err(e) = terminal
                            .draw(|f| self.value_picker.as_ref().unwrap().render(f, f.area()))
                        {
                            disable_raw();
                            return Err(ConfigError::FileError(format!("render: {e}")));
                        }
                        let Some(choice) = self.handle_value_picker_key() else {
                            continue;
                        };
                        let idx = self.value_picker.as_ref().unwrap().source_idx;
                        self.value_picker = None;
                        enable_raw();
                        if let Some(v) = choice {
                            self.commit(idx, Value::String(v), engine)?;
                        }
                    }
                }
                _ => {}
            }
        }
        let _ = terminal.clear();
        disable_raw();
        Ok(())
    }

    /// Plain (unstyled) layout for every row at the given terminal width.
    /// Shared by [`Self::render`] (which layers color on top) and the tests
    /// (which assert the alignment / ellipsis math without a TTY).
    ///
    /// One compact line per setting: `❯ key = value`. The `value` is the
    /// editable target and is the only field ellipsized on a narrow terminal;
    /// the key column stays aligned via a shared gutter, and on a wide
    /// terminal the value column is right-padded to the widest value so every
    /// row lines up. The scope is rendered once as a group header instead of
    /// repeating a `[scope]` tag on every row, which keeps the value column
    /// from competing with the scope tag for horizontal space.
    fn rows(&self, cols: usize) -> Vec<RowView<'_>> {
        // Gutter: align the `key` column to the widest key + 1.
        let key_gutter = self
            .items
            .iter()
            .map(|i| i.setting.key.chars().count())
            .max()
            .unwrap_or(20)
            + 1;
        // On wide terminals, pad every value to the widest one so the value
        // column lines up (the scope is shown once as a group header, not
        // per-row, so it costs no columns). On narrow terminals the value is
        // ellipsized instead, so this gutter is only used when it fits.
        let value_gutter = self
            .items
            .iter()
            .map(|i| i.display().chars().count())
            .max()
            .unwrap_or(0)
            + 1;
        self.items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let selected = i == self.cursor;
                let scope = item.setting.scope;
                let key = item.setting.key;
                let value = item.display();

                let key_pad = key_gutter.saturating_sub(key.chars().count());
                // Budget for the value + optional alignment padding:
                // marker(2) + key + key_pad + " = "(3). The scope is no longer
                // rendered per-row (it gets a single group header instead), so
                // it costs no columns here.
                let overhead = 2 + key.chars().count() + key_pad + 3;
                let budget = cols.saturating_sub(overhead);
                // On a wide terminal there is room to pad every value to the
                // widest one so the value column lines up. Otherwise pad
                // nothing and ellipsize so the row still fits.
                let align = value_gutter <= budget;
                let (shown_value, value_pad) = if align && value.chars().count() <= budget {
                    (
                        value.clone(),
                        value_gutter.saturating_sub(value.chars().count()),
                    )
                } else if value.chars().count() <= budget {
                    (value.clone(), 0)
                } else {
                    let mut s: String = value.chars().take(budget.saturating_sub(1)).collect();
                    s.push('…');
                    (s, 0)
                };

                RowView {
                    selected,
                    marker: if selected { "❯ " } else { "  " },
                    key,
                    key_pad,
                    value_shown: shown_value,
                    value_pad,
                    scope,
                }
            })
            .collect()
    }

    /// Count the scope group headers that will be rendered for the current
    /// (unsliced) item list, so the scroll window can reserve space for them.
    /// Every contiguous run of settings sharing a scope emits exactly one
    /// header; an empty list emits none.
    fn scope_header_count(&self) -> usize {
        let mut count = 0usize;
        let mut prev: Option<Scope> = None;
        for item in &self.items {
            if prev != Some(item.setting.scope) {
                count += 1;
                prev = Some(item.setting.scope);
            }
        }
        count
    }

    /// Render the panel into a ratatui frame, reusing the same centered
    /// bordered modal box every other picker draws (`draw_modal_box`) so the
    /// look matches the model/provider pickers. The `value` column is the
    /// editable target: only it is accent + bold on the selected row.
    fn render(&self, frame: &mut Frame<'_>, area: Rect) {
        let t = palette();
        // Match `draw_modal_box`'s inner content width (box width minus the
        // two borders) so values/descriptions fit the box, not the whole
        // frame — the box centers and caps its width independently of `area`.
        let cols = (area.width.saturating_sub(6).clamp(40, 76).saturating_sub(2)) as usize;
        let max_rows = (area.height.saturating_sub(10) as usize).clamp(3, 12);
        let total = self.items.len();
        // Recompute the scroll window from the *rendered* (post-group-header)
        // line count, so the highlight and the last item always stay inside the
        // visible box. `draw_modal_box` sizes the body from the rows the text
        // will actually occupy once wrapped, which includes the scope group
        // headers — so `max_rows` here must count those headers too, otherwise
        // the window over-counts visible rows and the last item scrolls out of
        // view with the first one clipped off the top.
        let header_count = self.scope_header_count();
        // Reserve one row per group header. On a pathologically short terminal
        // this could eat the whole box, so never drop below one visible item.
        let visible_budget = max_rows.saturating_sub(header_count).max(1);
        let mut top = self.cursor.saturating_sub(visible_budget.saturating_sub(1));
        top = top.min(total.saturating_sub(visible_budget));
        top = top.min(self.cursor);
        let start = top;
        let end = (start + visible_budget).min(total);

        // Settings are grouped by scope; a single dim header (e.g. `■ user`)
        // marks each group instead of repeating a `[scope]` tag on every row,
        // which keeps the value column from competing with the scope tag for
        // horizontal space on narrow terminals.
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut prev_scope: Option<Scope> = None;
        for row in self
            .rows(cols)
            .into_iter()
            .skip(start)
            .take(end.saturating_sub(start))
        {
            if prev_scope != Some(row.scope) {
                let label = scope_label(row.scope);
                lines.push(Line::from(Span::styled(
                    format!("■ {label}"),
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )));
                prev_scope = Some(row.scope);
            }
            // Selected row: `❯` + key accent/bold; value accent/bold (the
            // dropdown target). One compact line per setting.
            let (marker_style, key_style, eq_style, val_style) = if row.selected {
                (
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                    Style::default().fg(t.text),
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )
            } else {
                (
                    Style::default().fg(t.muted),
                    Style::default().fg(t.muted),
                    Style::default().fg(t.inactive),
                    Style::default().fg(t.text),
                )
            };
            let kpad = " ".repeat(row.key_pad);
            let vpad = " ".repeat(row.value_pad);
            lines.push(Line::from(vec![
                Span::styled(row.marker.to_string(), marker_style),
                Span::styled(row.key.to_string(), key_style),
                Span::styled(kpad, Style::default()),
                Span::styled(" = ".to_string(), eq_style),
                Span::styled(row.value_shown.clone(), val_style),
                Span::styled(vpad, Style::default()),
            ]));
        }
        if total > max_rows {
            lines.push(Line::from(Span::styled(
                format!("  … {} total", total),
                Style::default().fg(t.muted),
            )));
        }

        draw_modal_box(
            frame,
            area,
            lines,
            " configuration ",
            t.accent,
            Some(key_hint_line(
                "[↑/k↓/j] move   [PgUp/PgDn/Hm/End] jump   [Enter] edit   [Esc/q] exit",
            )),
        );
    }

    /// Open the appropriate editor for the row under the cursor. Bool flips
    /// in place; Enum opens a nested ratatui value picker overlay;
    /// String/Int/Float read a line from the terminal (temporarily leaving
    /// raw mode). Validated edits are committed to both the live engine
    /// config and the scope file.
    fn open_edit(&mut self, engine: &mut QueryEngine) -> Result<(), ConfigError> {
        let idx = self.cursor;
        let setting = self.items[idx].setting;
        let new_value: Option<Value> = match setting.kind {
            SettingKind::Bool => Some(Value::Boolean(
                !self.items[idx]
                    .value
                    .as_ref()
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            )),
            SettingKind::Enum(allowed) => {
                // Hand off to a nested modal instead of the standalone
                // `selector`. The run loop renders and resolves it.
                let current = self.items[idx].display();
                self.value_picker = Some(ValuePicker::new(allowed, current, idx));
                None
            }
            SettingKind::String => {
                disable_raw();
                println!(
                    "\nNew value for {} (current: {}):",
                    setting.key,
                    self.items[idx].display()
                );
                let mut buf = String::new();
                let _ = std::io::stdin().read_line(&mut buf);
                enable_raw();
                let s = buf.trim();
                if s.is_empty() {
                    None
                } else {
                    Some(Value::String(s.to_string()))
                }
            }
            SettingKind::Int => {
                disable_raw();
                println!(
                    "\nNew integer value for {} (current: {}):",
                    setting.key,
                    self.items[idx].display()
                );
                let mut buf = String::new();
                let _ = std::io::stdin().read_line(&mut buf);
                enable_raw();
                match buf.trim().parse::<i64>() {
                    Ok(n) => Some(Value::Integer(n)),
                    Err(_) => {
                        eprintln!("config: not an integer, edit skipped");
                        None
                    }
                }
            }
            SettingKind::Float => {
                disable_raw();
                println!(
                    "\nNew number value for {} (current: {}):",
                    setting.key,
                    self.items[idx].display()
                );
                let mut buf = String::new();
                let _ = std::io::stdin().read_line(&mut buf);
                enable_raw();
                match buf.trim().parse::<f64>() {
                    Ok(n) => Some(Value::Float(n)),
                    Err(_) => {
                        eprintln!("config: not a number, edit skipped");
                        None
                    }
                }
            }
        };

        if let Some(v) = new_value {
            self.commit(idx, v, engine)?;
        }
        Ok(())
    }

    /// Route one key event to the active value picker and return its result:
    /// `Some(Some(value))` when a choice was confirmed, `Some(None)` when it
    /// was cancelled, or `None` if the picker is still open (key consumed).
    fn handle_value_picker_key(&mut self) -> Option<Option<String>> {
        let picker = self.value_picker.as_mut()?;
        match event::read() {
            Ok(crossterm::event::Event::Key(key))
                if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat =>
            {
                let result = picker.on_key(key.code);
                match result {
                    PickerResult::StillOpen => None,
                    PickerResult::Chosen(v) => Some(Some(v)),
                    PickerResult::Cancelled => Some(None),
                }
            }
            Ok(_) => None,
            Err(_) => Some(None),
        }
    }

    /// Persist an edited value: update the live engine config (so the
    /// change takes effect this session) and rewrite just the affected
    /// key into its scope's TOML file.
    fn commit(
        &mut self,
        idx: usize,
        value: Value,
        engine: &mut QueryEngine,
    ) -> Result<(), ConfigError> {
        let key = self.items[idx].setting.key;
        self.items[idx].value = Some(value.clone());

        // Live engine config — round-trip keeps every other field intact.
        let doc = toml::Value::try_from(&engine.state().config)
            .map_err(|e| ConfigError::InvalidValue(format!("serialize config: {e}")))?;
        let doc = set_dotted(doc, key, value.clone())?;
        let updated: Config = doc
            .try_into()
            .map_err(|e: toml::de::Error| ConfigError::InvalidValue(e.to_string()))?;
        engine.state_mut().config = updated;

        // On-disk scope file — read-modify-write just this key.
        let cwd = PathBuf::from(engine.state().cwd.clone());
        write_scope_key(self.items[idx].setting, &cwd, value)
    }
}

fn enable_raw() {
    let _ = crossterm::terminal::enable_raw_mode();
}

fn disable_raw() {
    let _ = crossterm::terminal::disable_raw_mode();
}

/// Insert `value` at the dotted `key` path inside a TOML document,
/// creating any missing tables along the way. Refuses to clobber a
/// non-table value sitting in the path (mirrors the model tool's guard).
fn set_dotted(mut doc: Value, key: &str, value: Value) -> Result<Value, ConfigError> {
    let segments: Vec<&str> = key.split('.').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(ConfigError::InvalidValue(
            "empty setting key segment".into(),
        ));
    }
    let mut cursor = &mut doc;
    for seg in &segments[..segments.len() - 1] {
        let table = cursor
            .as_table_mut()
            .ok_or_else(|| ConfigError::InvalidValue(format!("non-table at '{seg}'")))?;
        let entry = table
            .entry((*seg).to_string())
            .or_insert_with(|| Value::Table(toml::value::Table::new()));
        if !entry.is_table() {
            return Err(ConfigError::InvalidValue(format!(
                "segment '{seg}' is not a table; refusing to overwrite"
            )));
        }
        cursor = entry;
    }
    let leaf = segments.last().unwrap();
    let table = cursor
        .as_table_mut()
        .ok_or_else(|| ConfigError::InvalidValue("leaf parent is not a table".into()))?;
    table.insert((*leaf).to_string(), value);
    Ok(doc)
}

/// Write `value` into the TOML file that owns `setting`'s scope,
/// preserving the rest of the file. The user-scope file is
/// `~/.config/agent-code/config.toml`; the project-scope file is the
/// nearest `.agent/settings.toml` (created on demand if none exists).
fn write_scope_key(
    setting: &SupportedSetting,
    cwd: &Path,
    value: Value,
) -> Result<(), ConfigError> {
    let path = match setting.scope {
        Scope::User => agent_code_lib::config::user_config_path()
            .ok_or_else(|| ConfigError::FileError("could not resolve user config path".into()))?,
        Scope::Project => match agent_code_lib::config::find_project_config_from(cwd) {
            Some(p) => p,
            None => cwd.join(".agent").join("settings.toml"),
        },
    };

    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| ConfigError::FileError(format!("create {parent:?}: {e}")))?;
    }

    let mut doc: Value = if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| ConfigError::FileError(format!("read {path:?}: {e}")))?;
        toml::from_str(&raw).map_err(|e| ConfigError::FileError(format!("parse {path:?}: {e}")))?
    } else {
        Value::Table(toml::value::Table::new())
    };

    doc = set_dotted(doc, setting.key, value)?;

    let serialized = toml::to_string_pretty(&doc)
        .map_err(|e| ConfigError::FileError(format!("serialize: {e}")))?;
    atomic_write_secret(&path, serialized.as_bytes())
        .map_err(|e| ConfigError::FileError(format!("write {path:?}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_dotted_creates_nested_tables() {
        let doc = Value::Table(toml::value::Table::new());
        let doc = set_dotted(doc, "api.max_turns", Value::Integer(10)).unwrap();
        // Round-trips into a Config-shaped doc would need the full schema;
        // here we just assert the dotted path was written.
        let api = doc.get("api").expect("api table created");
        assert_eq!(api.get("max_turns"), Some(&Value::Integer(10)));
    }

    #[test]
    fn set_dotted_refuses_to_clobber_scalar_with_table() {
        let mut root = toml::value::Table::new();
        root.insert("api".into(), Value::String("nope".into()));
        let doc = Value::Table(root);
        let err = set_dotted(doc, "api.model", Value::String("x".into())).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidValue(_)));
    }

    #[test]
    fn set_dotted_rejects_empty_key() {
        let doc = Value::Table(toml::value::Table::new());
        assert!(set_dotted(doc, "", Value::Boolean(true)).is_err());
    }

    /// Build a minimal panel from hand-written items to assert the
    /// plain-row layout used by `draw` (alignment + ellipsis). The live
    /// `ConfigPanel::new` needs a full engine; constructing `ConfigItem`
    /// directly keeps this hermetic and fast.
    fn test_panel(items: Vec<ConfigItem>) -> ConfigPanel {
        ConfigPanel {
            items,
            cursor: 0,
            value_picker: None,
        }
    }

    /// Build a `ConfigItem` for an allow-list `key`, overriding its
    /// description so the test controls wrapping. The cloned `SupportedSetting`
    /// is leaked (test-only, runs once) so it can back a `&'static` reference.
    fn item(key: &'static str, value: &str, desc: &'static str) -> ConfigItem {
        let setting = SUPPORTED_SETTINGS
            .iter()
            .find(|s| s.key == key)
            .expect("allow-list key must exist for the test fixture");
        let mut setting = setting.clone();
        setting.description = desc;
        ConfigItem {
            setting: Box::leak(Box::new(setting)),
            value: Some(Value::String(value.to_string())),
        }
    }

    #[test]
    fn rows_align_keys_and_ellipsize_value() {
        // Two keys of different widths so the gutter has work to do.
        let mut panel = test_panel(vec![
            item("ui.theme", "midnight", "short desc"),
            item(
                "api.model",
                "nvidia/nemotron-3-super-120b-a12b",
                "long value here",
            ),
        ]);
        // Select the second row so `rows` marks it; cursor is read by `rows`.
        panel.cursor = 1;
        let cols = 40;
        let rows = panel.rows(cols);
        assert_eq!(rows.len(), 2);

        // Both markers are exactly 2 visible columns, so the key column
        // starts at column 2 for every row regardless of selection.
        let key_col = 2;
        assert_eq!(rows[0].marker.chars().count(), key_col);
        assert_eq!(rows[1].marker.chars().count(), key_col);
        // The value (after the gutter) aligns: gutter is widest key + 1.
        let gutter = "ui.theme".len().max("api.model".len()) + 1;
        let ui_val_col = 2 + "ui.theme".len() + (gutter - "ui.theme".len()) + 3;
        let api_val_col = 2 + "api.model".len() + (gutter - "api.model".len()) + 3;
        assert_eq!(ui_val_col, api_val_col);

        // Long value is ellipsized within `cols`. Compute the visible row
        // length (the scope tag is now a group header, not per-row, so it
        // adds no columns here).
        let row = &rows[1];
        let visible = 2 + "api.model".len() + row.key_pad + 3 + row.value_shown.chars().count();
        assert!(visible <= cols, "row overflowed: {visible} > {cols}");
        assert!(
            row.value_shown.ends_with('…'),
            "long value should ellipsize"
        );

        // Short value is shown in full, not ellipsized.
        assert_eq!(rows[0].value_shown, "midnight");
    }

    #[test]
    fn rows_pad_value_to_align_when_wide() {
        // On a wide terminal every value is padded to the widest one, so the
        // rows line up in their value column (scopes are grouped separately).
        let panel = test_panel(vec![
            item("ui.theme", "dark", "short desc"),
            item(
                "api.model",
                "nvidia/nemotron-3-super-120b-a12b",
                "long value",
            ),
        ]);
        let rows = panel.rows(120);
        assert!(rows[0].value_pad > 0, "value should be padded to align");
        // Value column start = marker + key + pad + " = ".
        let col = |r: &RowView<'_>| 2 + r.key.chars().count() + r.key_pad + 3;
        assert_eq!(
            col(&rows[0]) + rows[0].value_shown.chars().count() + rows[0].value_pad,
            col(&rows[1]) + rows[1].value_shown.chars().count() + rows[1].value_pad
        );
    }
}

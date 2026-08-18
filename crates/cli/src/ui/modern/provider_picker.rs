//! In-TUI provider picker (Ctrl+/ `/provider`).
//!
//! Lists providers with a default base URL, filterable by name. When a
//! provider is selected, the base URL and a default model are applied
//! automatically (mirrors the `/model` picker UX).

use super::app::{App, PendingProviderAction};

/// Overlay state for the provider picker.
#[derive(Debug, Clone)]
pub struct ProviderPicker {
    /// Filter text over provider names / descriptions.
    pub query: String,
    /// Highlighted row into the filtered list.
    pub selected: usize,
    /// First visible row of the filtered list (scroll offset).
    pub top: usize,
    /// Full catalog: (name, description).
    pub entries: Vec<(String, String)>,
    /// Provider active when the picker opened.
    pub current: String,
}

impl ProviderPicker {
    /// Filtered provider rows matching `query`.
    pub fn filtered(&self) -> Vec<(usize, &str, &str)> {
        let q = self.query.to_ascii_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, (id, desc))| {
                if q.is_empty() {
                    return true;
                }
                id.to_ascii_lowercase().contains(&q) || desc.to_ascii_lowercase().contains(&q)
            })
            .map(|(i, (id, desc))| (i, id.as_str(), desc.as_str()))
            .collect()
    }
}

/// Largest viewport the picker can show; the actual rows are clamped to the
/// terminal height at draw time, but move-scrolling uses this so the window
/// advances in the user's scroll direction before the height is known.
const SCROLL_WINDOW: usize = 12;

impl App {
    /// Request opening the provider picker (run loop fills catalog via pending Show).
    pub fn request_provider_picker(&mut self) {
        if self.front_modal().is_some() {
            return;
        }
        self.work.stage_provider(PendingProviderAction::Show);
        self.dirty = true;
    }

    pub fn provider_picker_move(&mut self, delta: i32) {
        let Some(p) = self.provider_picker.as_mut() else {
            return;
        };
        let n = p.filtered().len() as i32;
        if n == 0 {
            p.selected = 0;
            p.top = 0;
            self.dirty = true;
            return;
        }
        let cur = p.selected as i32;
        let new_sel = (cur + delta).rem_euclid(n) as usize;
        // Advance the scroll offset in the direction the user moved so the
        // highlighted row stays pinned to the edge it approached: scrolling
        // down grows `top` until the selection sits at the window bottom,
        // scrolling up shrinks `top` until it sits at the window top.
        if delta > 0 && new_sel >= p.top.saturating_add(SCROLL_WINDOW) {
            p.top = new_sel + 1 - SCROLL_WINDOW;
        } else if delta < 0 && new_sel < p.top {
            p.top = new_sel;
        }
        p.selected = new_sel;
        self.dirty = true;
    }

    pub fn provider_picker_insert_char(&mut self, c: char) {
        let Some(p) = self.provider_picker.as_mut() else {
            return;
        };
        if c.is_control() {
            return;
        }
        p.query.push(c);
        p.selected = 0;
        p.top = 0;
        self.dirty = true;
    }

    pub fn provider_picker_backspace(&mut self) {
        let Some(p) = self.provider_picker.as_mut() else {
            return;
        };
        p.query.pop();
        p.selected = 0;
        p.top = 0;
        self.dirty = true;
    }

    /// Accept the current picker selection: stage a `Set` for the chosen
    /// provider so the run loop applies base URL + default model.
    pub fn provider_picker_accept(&mut self) {
        let Some(p) = self.provider_picker.clone() else {
            return;
        };
        let filtered = p.filtered();
        let Some((_, name, _)) = filtered.get(p.selected).copied() else {
            self.close_provider_picker();
            return;
        };
        let Some(kind) = agent_code_lib::llm::provider::ProviderKind::from_name(name) else {
            self.close_provider_picker();
            return;
        };
        self.close_provider_picker();
        self.work
            .stage_provider(PendingProviderAction::Set { provider: kind });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::modern::app::App;

    #[test]
    fn open_filter_and_accept() {
        let mut app = App::new("p", "/tmp", "s");
        app.open_provider_picker(
            "openai",
            vec![
                ("openai".into(), "https://api.openai.com/v1".into()),
                ("anthropic".into(), "https://api.anthropic.com".into()),
            ],
        );
        assert!(app.provider_picker_open());
        app.provider_picker_insert_char('a');
        app.provider_picker_insert_char('n');
        let filtered = app.provider_picker.as_ref().unwrap().filtered();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].1, "anthropic");
        app.provider_picker_accept();
        assert!(!app.provider_picker_open());
        assert_eq!(
            app.work.provider_staged(),
            Some(&PendingProviderAction::Set {
                provider: agent_code_lib::llm::provider::ProviderKind::from_name("anthropic")
                    .unwrap()
            })
        );
    }
}

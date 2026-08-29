//! In-TUI model picker (Ctrl+M / `/model`).
//!
//! Lists provider catalog entries with filter + optional effort sub-menu
//! (Grok Build product motion: pick model, Tab for reasoning effort).

use super::app::{App, EFFORT_LEVELS, PendingModelAction};
use crossterm::terminal;

/// Overlay state for the model picker.
#[derive(Debug, Clone)]
pub struct ModelPicker {
    /// Filter text over model ids / descriptions.
    pub query: String,
    /// Highlighted row into the filtered list (model phase) or effort list.
    pub selected: usize,
    /// First visible row in the filtered list (scroll offset). Keeps the
    /// selection cursor visible when the list exceeds the terminal height.
    pub top: usize,
    /// Full catalog: (id, description).
    pub entries: Vec<(String, String)>,
    /// Model active when the picker opened.
    pub current: String,
    /// When true, the list shows effort levels for the highlighted model.
    pub effort_phase: bool,
    /// Selected effort row.
    pub effort_selected: usize,
}

impl ModelPicker {
    /// Filtered model rows matching `query`.
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

impl App {
    /// Request opening the model picker (run loop fills catalog via pending Show).
    pub fn request_model_picker(&mut self) {
        if self.front_modal().is_some() {
            return;
        }
        self.work.stage_model(PendingModelAction::Show);
        self.dirty = true;
    }

    pub fn model_picker_move(&mut self, delta: i32) {
        let Some(p) = self.model_picker.as_mut() else {
            return;
        };
        if p.effort_phase {
            let n = EFFORT_LEVELS.len() as i32;
            let cur = p.effort_selected as i32;
            p.effort_selected = (cur + delta).rem_euclid(n) as usize;
        } else {
            let n = p.filtered().len() as i32;
            if n == 0 {
                p.selected = 0;
                p.top = 0;
            } else {
                let cur = p.selected as i32;
                p.selected = (cur + delta).rem_euclid(n) as usize;
                let (_w, term_h) = terminal::size().unwrap_or((80, 24));
                let header_footer = 6usize; // header + footer + borders
                let max_rows: usize = (term_h as usize).saturating_sub(header_footer).max(3);
                let _last_visible = (p.top + max_rows - 1).min(p.entries.len().saturating_sub(1));
                if p.selected.saturating_sub(p.top) >= max_rows.saturating_sub(1)
                    && (p.selected + 1) < (n as usize)
                {
                    // Scroll window so selected is at bottom (visible)
                    p.top = p.selected.saturating_sub(max_rows.saturating_sub(1));
                    p.top = p.top.min(p.entries.len().saturating_sub(1));
                    // Ensure selected stays visible after scroll by keeping
                    // the selection within the new window.
                } else if p.selected < p.top {
                    p.top = p.selected;
                }
            }
        }
        self.dirty = true;
    }

    pub fn model_picker_insert_char(&mut self, c: char) {
        let Some(p) = self.model_picker.as_mut() else {
            return;
        };
        if p.effort_phase || c.is_control() {
            return;
        }
        p.query.push(c);
        p.selected = 0;
        p.top = 0;
        p.top = p.selected.saturating_sub(6);
        self.dirty = true;
    }

    pub fn model_picker_backspace(&mut self) {
        let Some(p) = self.model_picker.as_mut() else {
            return;
        };
        if p.effort_phase {
            p.effort_phase = false;
            self.dirty = true;
            return;
        }
        if p.query.pop().is_none() {
            return;
        }
        if p.query.is_empty() {
            p.selected = 0;
            p.top = 0;
        }
        p.top = p.selected.saturating_sub(6);
        self.dirty = true;
    }

    pub fn model_picker_enter_effort(&mut self) {
        let Some(p) = self.model_picker.as_mut() else {
            return;
        };
        if p.filtered().is_empty() {
            return;
        }
        p.effort_selected = EFFORT_LEVELS
            .iter()
            .position(|l| Some(*l) == self.effort.as_deref())
            .unwrap_or(0);
        p.effort_phase = true;
        self.dirty = true;
    }

    /// Accept the current model selection: stage a `Set` for the run loop.
    pub fn model_picker_accept(&mut self) {
        let Some(p) = self.model_picker.clone() else {
            return;
        };
        let filtered = p.filtered();
        let Some((_, id, _)) = filtered.get(p.selected).copied() else {
            self.close_model_picker();
            return;
        };
        let effort = if p.effort_phase {
            EFFORT_LEVELS.get(p.effort_selected).map(|s| s.to_string())
        } else {
            None
        };
        self.close_model_picker();
        self.work.stage_model(PendingModelAction::Set {
            model: id.into(),
            effort,
        });
    }
}

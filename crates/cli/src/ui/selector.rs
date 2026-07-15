//! Arrow-key interactive selector for terminal menus.
//!
//! Renders a list of options with a highlighted cursor that moves
//! with up/down arrow keys. Enter confirms the selection.
//! Supports optional live preview that updates as the cursor moves.

use std::io::Write;

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent},
    style::Stylize,
    terminal,
};

/// A single option in the selector.
pub struct SelectOption {
    pub label: String,
    pub description: String,
    pub value: String,
    /// Optional preview content shown below the options when this item is focused.
    pub preview: Option<String>,
}

/// Show an interactive selector and return the chosen value.
///
/// Esc/`q` cancel by returning the currently-highlighted value (legacy
/// behavior kept for non-security callers). For prompts where cancel must NOT
/// fall through to a default action (e.g. permission modals), use
/// [`select_cancellable`] instead.
pub fn select(options: &[SelectOption]) -> String {
    if options.is_empty() {
        return String::new();
    }
    let (index, _cancelled) = select_index(options);
    print_choice("→", &options[index].label);
    options[index].value.clone()
}

/// Like [`select`], but returns `None` when the user cancels with Esc/`q`
/// instead of falling through to the highlighted option. Callers that gate a
/// side effect on the result (permission prompts) must use this so a dismissed
/// modal cannot silently pick the default.
pub fn select_cancellable(options: &[SelectOption]) -> Option<String> {
    if options.is_empty() {
        return None;
    }
    let (index, cancelled) = select_index(options);
    if cancelled {
        print_choice("✕", "cancelled");
        None
    } else {
        print_choice("→", &options[index].label);
        Some(options[index].value.clone())
    }
}

/// Print the confirmed/cancelled line under a dismissed selector.
fn print_choice(marker: &str, label: &str) {
    let t = super::theme::current();
    println!(
        "    {} {}\r",
        marker.with(t.accent),
        label.to_string().bold()
    );
}

/// Core selector loop. Returns `(selected_index, cancelled)` where `cancelled`
/// is true only when dismissed with Esc/`q` (as opposed to Enter or a letter
/// hotkey).
fn select_index(options: &[SelectOption]) -> (usize, bool) {
    let has_preview = options.iter().any(|o| o.preview.is_some());
    let mut selected = 0usize;
    let mut cancelled = false;
    let mut filter = String::new();
    let mut filtered_indices: Vec<usize> = (0..options.len()).collect();

    terminal::enable_raw_mode().expect("failed to enable raw mode");

    render_all_filtered(options, &filtered_indices, selected, has_preview, &filter);

    loop {
        if let Ok(Event::Key(KeyEvent {
            code, modifiers, ..
        })) = event::read()
        {
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = if selected > 0 {
                        selected - 1
                    } else {
                        filtered_indices.len() - 1
                    };
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = if selected < filtered_indices.len() - 1 {
                        selected + 1
                    } else {
                        0
                    };
                }
                KeyCode::Enter => break,
                KeyCode::Char('q') | KeyCode::Esc => {
                    if filter.is_empty() {
                        cancelled = true;
                        break;
                    } else {
                        // Clear filter on Esc when filter is active.
                        filter.clear();
                        filtered_indices = (0..options.len()).collect();
                        selected = 0;
                    }
                }
                KeyCode::Backspace => {
                    if !filter.is_empty() {
                        filter.pop();
                        update_filter(options, &filter, &mut filtered_indices, &mut selected);
                    }
                }
                KeyCode::Char(c) => {
                    // Ctrl+C to cancel.
                    if modifiers.contains(crossterm::event::KeyModifiers::CONTROL) && c == 'c' {
                        cancelled = true;
                        break;
                    }
                    // 'f' is a shortcut for the third option ("Allow always")
                    // in permission prompts.
                    if c == 'f' && filter.is_empty() && options.len() > 2 {
                        selected = 2;
                        break;
                    }
                    // If filter is empty and single char, try direct selection.
                    if filter.is_empty() {
                        let idx = c.to_ascii_lowercase() as usize - 'a' as usize;
                        if idx < options.len() {
                            selected = idx;
                            break;
                        }
                    }
                    // Add to filter.
                    filter.push(c);
                    update_filter(options, &filter, &mut filtered_indices, &mut selected);
                }
                _ => {}
            }

            clear_all_filtered(filtered_indices.len(), has_preview);
            render_all_filtered(options, &filtered_indices, selected, has_preview, &filter);
        }
    }

    terminal::disable_raw_mode().expect("failed to disable raw mode");

    clear_all_filtered(filtered_indices.len(), has_preview);

    // Map filtered index back to original index.
    let original_idx = if cancelled {
        0
    } else if selected < filtered_indices.len() {
        filtered_indices[selected]
    } else {
        0
    };

    (original_idx, cancelled)
}

/// Update filtered indices based on filter string.
fn update_filter(
    options: &[SelectOption],
    filter: &str,
    filtered_indices: &mut Vec<usize>,
    selected: &mut usize,
) {
    let filter_lower = filter.to_lowercase();
    *filtered_indices = options
        .iter()
        .enumerate()
        .filter(|(_, opt)| {
            let label_lower = opt.label.to_lowercase();
            let value_lower = opt.value.to_lowercase();
            label_lower.contains(&filter_lower) || value_lower.contains(&filter_lower)
        })
        .map(|(i, _)| i)
        .collect();
    *selected = 0;
}

/// Preview lines count (fixed height so the UI doesn't jump).
const PREVIEW_LINES: usize = 6;

fn render_all(options: &[SelectOption], selected: usize, has_preview: bool) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    // Blank separator line so options aren't glued to the banner above.
    write!(out, "\r\n").ok();

    // Render options.
    for (i, opt) in options.iter().enumerate() {
        let letter = (b'A' + i as u8) as char;
        let t = super::theme::current();
        if i == selected {
            write!(
                out,
                "  {} {} {}\r\n",
                format!("❯ {letter})").with(t.accent).bold(),
                opt.label.clone().with(t.text).bold(),
                opt.description.clone().with(t.muted),
            )
            .ok();
        } else {
            write!(
                out,
                "    {}) {} {}\r\n",
                letter,
                opt.label,
                opt.description.clone().with(t.muted),
            )
            .ok();
        }
    }

    // Render preview block if any option has preview content.
    if has_preview {
        write!(out, "\r\n").ok(); // Blank separator line.
        let preview_text = options[selected].preview.as_deref().unwrap_or("");

        let lines: Vec<&str> = preview_text.lines().collect();
        for i in 0..PREVIEW_LINES {
            if i < lines.len() {
                write!(out, "    {}\r\n", lines[i]).ok();
            } else {
                write!(out, "    \r\n").ok();
            }
        }
    }

    out.flush().ok();
}

fn clear_all(option_count: usize, has_preview: bool) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let total = option_count + if has_preview { PREVIEW_LINES + 1 } else { 0 };
    for _ in 0..total {
        write!(out, "\x1b[A\x1b[2K").ok();
    }
    out.flush().ok();
}

/// Render filtered options with filter input displayed.
fn render_all_filtered(
    options: &[SelectOption],
    filtered_indices: &[usize],
    selected: usize,
    has_preview: bool,
    filter: &str,
) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let t = super::theme::current();

    // Show filter input.
    write!(out, "\r\n").ok();
    write!(
        out,
        "  {}{}\r\n",
        "> ".with(t.accent).bold(),
        filter.with(t.text)
    )
    .ok();

    // Render filtered options.
    for (display_idx, &orig_idx) in filtered_indices.iter().enumerate() {
        let opt = &options[orig_idx];
        let letter = (b'a' + display_idx as u8) as char;
        if display_idx == selected {
            write!(
                out,
                "  {} {} {}\r\n",
                format!("❯ {letter})").with(t.accent).bold(),
                opt.label.clone().with(t.text).bold(),
                opt.description.clone().with(t.muted),
            )
            .ok();
        } else {
            write!(
                out,
                "    {}) {} {}\r\n",
                letter,
                opt.label,
                opt.description.clone().with(t.muted),
            )
            .ok();
        }
    }

    // Render preview block if any option has preview content.
    if has_preview && !filtered_indices.is_empty() {
        write!(out, "\r\n").ok();
        let preview_idx = filtered_indices[selected];
        let preview_text = options[preview_idx].preview.as_deref().unwrap_or("");

        let lines: Vec<&str> = preview_text.lines().collect();
        for i in 0..PREVIEW_LINES {
            if i < lines.len() {
                write!(out, "    {}\r\n", lines[i]).ok();
            } else {
                write!(out, "    \r\n").ok();
            }
        }
    }

    out.flush().ok();
}

/// Clear filtered rendering.
fn clear_all_filtered(option_count: usize, has_preview: bool) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    // +1 for filter input line.
    let total = 1 + option_count + if has_preview { PREVIEW_LINES + 1 } else { 0 };
    for _ in 0..total {
        write!(out, "\x1b[A\x1b[2K").ok();
    }
    out.flush().ok();
}

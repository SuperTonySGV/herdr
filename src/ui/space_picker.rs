use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::{
    text::{display_width_u16, middle_elide, truncate_end},
    widgets::render_panel_shell,
};
use crate::app::space_picker::SpacePickerRow;
use crate::app::state::AppState;

/// The narrowest width at which a path column earns its space. Below it the
/// name alone has to carry the row, which a vertical split reaches easily.
const PATH_COLUMN_MIN_WIDTH: u16 = 44;

pub(super) fn render_space_picker_overlay(app: &AppState, frame: &mut Frame) {
    let popup = app.space_picker_popup_rect();
    let Some(inner) = render_panel_shell(frame, popup, app.palette.accent, app.palette.panel_bg)
    else {
        return;
    };

    render_search(app, frame, Rect::new(inner.x, inner.y, inner.width, 1));

    let body_y = inner.y.saturating_add(2);
    let body_height = inner.height.saturating_sub(3);
    if body_height == 0 {
        return;
    }
    let body = Rect::new(inner.x, body_y, inner.width, body_height);

    if let Some(error) = &app.space_picker.store_error {
        render_store_error(app, error, frame, body);
    } else {
        render_rows(app, frame, body);
    }

    let footer_y = inner.y.saturating_add(inner.height.saturating_sub(1));
    render_footer(app, frame, Rect::new(inner.x, footer_y, inner.width, 1));
}

fn render_search(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let mut spans = vec![Span::styled(
        " / ",
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
    )];
    if app.space_picker.query.is_empty() {
        spans.push(Span::styled(
            "filter places",
            Style::default().fg(p.overlay0),
        ));
    } else {
        spans.push(Span::styled(
            app.space_picker.query.clone(),
            Style::default().fg(p.text),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Say what is wrong and where, rather than showing an empty list.
///
/// An empty picker for a store we could not read is indistinguishable from
/// having lost every place in it.
fn render_store_error(app: &AppState, error: &str, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let lines = vec![
        Line::from(Span::styled(
            " places could not be read",
            Style::default().fg(p.red).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!(
                " {}",
                truncate_end(error, area.width.saturating_sub(1) as usize)
            ),
            Style::default().fg(p.overlay0),
        )),
        Line::from(Span::styled(
            " the file was left untouched",
            Style::default().fg(p.overlay0),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_rows(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let visible = app.space_picker.visible();
    let capacity = area.height as usize;
    // Keep the selection on screen without a separate scroll model: the list is
    // short enough that a window around the cursor is the whole story.
    let start = app
        .space_picker
        .selected
        .saturating_sub(capacity.saturating_sub(1))
        .min(visible.len().saturating_sub(capacity.min(visible.len())));

    let show_path = area.width >= PATH_COLUMN_MIN_WIDTH;
    let name_width = if show_path {
        area.width / 3
    } else {
        area.width.saturating_sub(2)
    };

    let mut lines = Vec::new();
    let mut previous_was_place = false;
    for (offset, row_idx) in visible.iter().skip(start).take(capacity).enumerate() {
        let row = &app.space_picker.rows[*row_idx];
        let selected = start + offset == app.space_picker.selected;

        // One blank-ish divider where places give way to recents, so the two
        // kinds of row are not silently interleaved.
        let is_recent = matches!(row, SpacePickerRow::Recent { .. });
        if is_recent && previous_was_place && lines.len() < capacity {
            lines.push(Line::from(Span::styled(
                " recent",
                Style::default().fg(p.overlay0),
            )));
        }
        previous_was_place = matches!(row, SpacePickerRow::Place { .. });

        let marker = if selected { ">" } else { " " };
        let name_style = if row.missing() {
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM)
        } else if selected {
            Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.text)
        };

        let mut spans = vec![
            Span::styled(format!("{marker} "), name_style),
            Span::styled(truncate_end(row.label(), name_width as usize), name_style),
        ];

        if show_path {
            let used = display_width_u16(row.label()).saturating_add(2);
            let room = area.width.saturating_sub(used).saturating_sub(2);
            if room > 4 {
                spans.push(Span::styled(
                    format!(
                        "  {}",
                        middle_elide(&row.path().display().to_string(), room as usize)
                    ),
                    Style::default().fg(p.overlay0),
                ));
            }
        }
        if row.missing() {
            spans.push(Span::styled("  missing", Style::default().fg(p.red)));
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_footer(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " enter open   esc cancel",
            Style::default().fg(p.overlay0),
        ))),
        area,
    );
}

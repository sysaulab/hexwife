use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};

use crate::constants::SCROLLBAR_WIDTH;
use crate::search::format_eta;
use crate::search::format_throughput;
use crate::util::read_bytes_at;
use crate::viewer::{HexViewer, SearchState};

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------
fn _highlight_hex(
    hex_str: &str,
    byte_index: usize,
    group_size: usize,
    hl_style: Style,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut group_idx = 0;
    let bytes = hex_str.as_bytes();
    let len = bytes.len();
    let mut group_start = 0;

    while group_start < len {
        let group_len = (group_size * 2).min(len - group_start);
        let group_str =
            std::str::from_utf8(&bytes[group_start..group_start + group_len]).unwrap();

        if group_idx == byte_index / group_size {
            let byte_in_group = byte_index % group_size;
            let start = byte_in_group * 2;
            if start + 2 <= group_str.len() {
                let before = group_str[..start].to_string();
                let hl = group_str[start..start + 2].to_string();
                let after = group_str[start + 2..].to_string();
                if !before.is_empty() {
                    spans.push(Span::raw(before));
                }
                spans.push(Span::styled(hl, hl_style));
                if !after.is_empty() {
                    spans.push(Span::raw(after));
                }
            } else {
                spans.push(Span::raw(group_str.to_string()));
            }
        } else {
            spans.push(Span::raw(group_str.to_string()));
        }

        if group_start + group_len < len {
            spans.push(Span::raw(" ".to_string()));
        }

        group_start += group_len;
        if group_start < len && bytes[group_start] == b' ' {
            group_start += 1;
        }
        group_idx += 1;
    }
    spans
}

fn build_highlighted_hex(
    hex_str: &str,
    group_size: usize,
    highlights: Vec<(usize, Style)>,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let bytes = hex_str.as_bytes();
    let len = bytes.len();
    let mut group_idx = 0;
    let mut group_start = 0;

    while group_start < len {
        let group_len = (group_size * 2).min(len - group_start);
        let group_str =
            std::str::from_utf8(&bytes[group_start..group_start + group_len]).unwrap();

        let style = highlights
            .iter()
            .find(|(idx, _)| *idx == group_idx)
            .map(|(_, s)| *s);

        if let Some(st) = style {
            spans.push(Span::styled(group_str.to_string(), st));
        } else {
            spans.push(Span::raw(group_str.to_string()));
        }

        if group_start + group_len < len {
            spans.push(Span::raw(" ".to_string()));
        }

        group_start += group_len;
        if group_start < len && bytes[group_start] == b' ' {
            group_start += 1;
        }
        group_idx += 1;
    }
    spans
}

fn format_line(
    bytes: &[u8],
    offset: u128,
    addr_width: usize,
    group_size: usize,
    bytes_per_line: usize,
    cursor_offset: u128,
    hl_style: Style,
    scrollbar_char: char,
    search_matches: &[u128],
) -> Line<'static> {
    let mut spans = Vec::new();

    let addr_str = format!("{:0>width$X}", offset, width = addr_width);
    spans.push(Span::raw(addr_str));
    spans.push(Span::raw("  ".to_string()));

    let mut hex_parts = Vec::new();
    for chunk in bytes.chunks(group_size) {
        let part: String = chunk.iter().map(|b| format!("{:02X}", b)).collect();
        hex_parts.push(part);
    }
    let total_groups = bytes_per_line / group_size;
    while hex_parts.len() < total_groups {
        hex_parts.push(" ".repeat(group_size * 2));
    }
    let hex_str = hex_parts.join(" ");

    let cursor_in_line = if offset <= cursor_offset
        && (cursor_offset - offset) < bytes_per_line as u128
    {
        Some((cursor_offset - offset) as usize)
    } else {
        None
    };

    let match_start_in_line: Option<usize> = search_matches.iter().find_map(|&m| {
        if m >= offset && m < offset + bytes_per_line as u128 {
            Some((m - offset) as usize)
        } else {
            None
        }
    });

    let match_style = Style::default().bg(Color::Green).add_modifier(Modifier::BOLD);

    let mut highlighted_groups: Vec<(usize, Style)> = Vec::new();
    if let Some(byte_idx) = cursor_in_line {
        highlighted_groups.push((byte_idx / group_size, hl_style));
    }
    if let Some(byte_idx) = match_start_in_line {
        highlighted_groups.push((byte_idx / group_size, match_style));
    }

    if !highlighted_groups.is_empty() {
        spans.append(&mut build_highlighted_hex(
            &hex_str,
            group_size,
            highlighted_groups,
        ));
    } else {
        spans.push(Span::raw(hex_str));
    }

    spans.push(Span::raw("  ".to_string()));

    let ascii: String = bytes
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '.'
            }
        })
        .collect();
    let ascii_padded = format!("{:>width$}", ascii, width = bytes_per_line);

    if let Some(byte_idx) = cursor_in_line {
        let (before, rest) = ascii_padded.split_at(byte_idx);
        let (hl_char, after) = rest.split_at(1.min(rest.len()));
        spans.push(Span::raw(before.to_string()));
        spans.push(Span::styled(hl_char.to_string(), hl_style));
        spans.push(Span::raw(after.to_string()));
    } else {
        spans.push(Span::raw(ascii_padded));
    }

    spans.push(Span::raw(format!(" {}", scrollbar_char)));

    Line::from(spans)
}

fn layout_normal(area: Rect, total_rows: usize) -> (Rect, Option<Rect>, Rect) {
    if total_rows >= 3 {
        let chunks = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
        (chunks[0], Some(chunks[1]), chunks[2])
    } else if total_rows == 2 {
        let chunks =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
        (chunks[0], None, chunks[1])
    } else {
        let chunks = Layout::vertical([Constraint::Length(1)]).split(area);
        (chunks[0], None, chunks[0])
    }
}

pub fn render_hex(
    f: &mut Frame,
    viewer: &mut HexViewer,
    hl_style: Style,
    vis_rows: usize,
    area: Rect,
) {
    let vis_start = viewer.scroll_line * viewer.bytes_per_line as u128;
    let needed_len = vis_rows * viewer.bytes_per_line;
    let file_bytes = read_bytes_at(&mut viewer.file, vis_start, needed_len).unwrap_or_default();

    let total_logical_lines = viewer.total_lines();
    let thumb_height = if total_logical_lines == 0 {
        0.0
    } else {
        (vis_rows as f64 / total_logical_lines as f64) * vis_rows as f64
    };
    let thumb_height = thumb_height.max(1.0) as usize;
    let thumb_top = if total_logical_lines == 0 {
        0.0
    } else {
        (viewer.scroll_line as f64 / total_logical_lines as f64) * vis_rows as f64
    };
    let thumb_top = thumb_top as usize;

    let mut lines: Vec<Line> = Vec::new();
    for row in 0..vis_rows {
        let offset = vis_start + row as u128 * viewer.bytes_per_line as u128;
        let scrollbar_char = if row >= thumb_top && row < thumb_top + thumb_height {
            '█'
        } else {
            '│'
        };

        if offset > viewer.file_size {
            let empty: &[u8] = &[];
            lines.push(format_line(
                empty,
                offset,
                viewer.address_width,
                viewer.grouping as usize,
                viewer.bytes_per_line,
                viewer.cursor,
                hl_style,
                scrollbar_char,
                &viewer.search_matches,
            ));
            continue;
        }

        let start_idx = row * viewer.bytes_per_line;
        let end_idx = (start_idx + viewer.bytes_per_line).min(file_bytes.len());
        let slice = &file_bytes[start_idx..end_idx];

        lines.push(format_line(
            slice,
            offset,
            viewer.address_width,
            viewer.grouping as usize,
            viewer.bytes_per_line,
            viewer.cursor,
            hl_style,
            scrollbar_char,
            &viewer.search_matches,
        ));
    }

    let hex_block = Block::default();
    f.render_widget(Paragraph::new(lines).block(hex_block), area);
}

pub fn render_search_panel(
    f: &mut Frame,
    viewer: &HexViewer,
    input: &str,
    area: Rect,
) {
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);
    let prompt_area = chunks[0];
    let help_area = chunks[1];

    let mut prompt_spans = vec![Span::raw("/")];
    prompt_spans.push(Span::raw(input.to_string()));
    if let Some(ref err) = viewer.search_error {
        prompt_spans.push(Span::styled(
            format!(" {}", err),
            Style::default().fg(Color::Red),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(prompt_spans)), prompt_area);

    let help_text = "Hex: 48 65 6C 6C 6F   ASCII: \"Hello\"   Regex: /[Hh]ex/";
    f.render_widget(Paragraph::new(help_text), help_area);
}

pub fn format_status(viewer: &HexViewer) -> Line<'static> {
    let mut spans = Vec::new();

    if viewer.edit_mode {
        spans.push(Span::styled(
            "EDIT MODE, NO UNDO. Edit with care. ",
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let line = if viewer.bytes_per_line > 0 {
        viewer.cursor / viewer.bytes_per_line as u128
    } else {
        0
    };
    let total = viewer.total_lines();
    let pct = if viewer.file_size > 0 {
        (viewer.cursor * 100 / viewer.file_size) as u64
    } else {
        100
    };
    let rest = format!(
        " {}  0x{:X}  L{}/{}  {}%  [{}b]{}",
        viewer.filename.display(),
        viewer.cursor,
        line + 1,
        total,
        pct,
        viewer.grouping,
        match viewer.search {
            SearchState::MatchFound => " - Match found",
            SearchState::NotFound => " - Not found",
            _ => "",
        }
    );
    spans.push(Span::raw(rest));
    Line::from(spans)
}

pub fn word_interpretation(viewer: &HexViewer, buffer: &[u8], vis_start: u128) -> String {
    let cursor = viewer.cursor;
    let grouping = viewer.grouping as u128;
    if cursor >= viewer.file_size || grouping == 0 {
        return String::new();
    }

    let aligned = cursor - (cursor % grouping);
    if aligned + grouping > viewer.file_size {
        return String::new();
    }

    if aligned < vis_start {
        return String::new();
    }
    let pos_in_buf = (aligned - vis_start) as usize;
    if pos_in_buf + (grouping as usize) > buffer.len() {
        return String::new();
    }

    let data = &buffer[pos_in_buf..pos_in_buf + grouping as usize];

    match grouping as usize {
        1 => {
            let v = data[0];
            format!("u8 {}", v)
        }
        2 => {
            let le = u16::from_le_bytes([data[0], data[1]]);
            let be = u16::from_be_bytes([data[0], data[1]]);
            format!("u16 LE:{} BE:{}", le, be)
        }
        4 => {
            let le = u32::from_le_bytes(data.try_into().unwrap());
            let be = u32::from_be_bytes(data.try_into().unwrap());
            format!("u32 LE:{} BE:{}", le, be)
        }
        8 => {
            let le = u64::from_le_bytes(data.try_into().unwrap());
            let be = u64::from_be_bytes(data.try_into().unwrap());
            format!("u64 LE:{} BE:{}", le, be)
        }
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Main draw function
// ---------------------------------------------------------------------------
pub fn draw_ui(f: &mut Frame, viewer: &mut HexViewer, hl_style: Style) {
    let area = f.area();
    let total_rows = area.height as usize;

    let min_width = viewer.address_width
        + 4
        + viewer.grouping as usize * 2
        + viewer.grouping as usize
        + SCROLLBAR_WIDTH;
    if area.width < min_width as u16 {
        let msg = format!(
            "Enlarge terminal to at least {} columns (grouping {}).",
            min_width, viewer.grouping
        );
        let p = Paragraph::new(msg).alignment(Alignment::Center);
        f.render_widget(p, area);
        return;
    }

    // Extract search prompt input early to avoid borrow conflict
    let search_input = if let SearchState::Prompt(ref input) = viewer.search {
        Some(input.clone())
    } else {
        None
    };

    if let Some(input) = search_input {
        if total_rows < 4 {
            let msg = "Terminal too small for search. Enlarge to at least 4 rows.";
            f.render_widget(Paragraph::new(msg).alignment(Alignment::Center), area);
            return;
        }

        let chunks = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .split(area);
        let hex_area = chunks[0];
        let search_panel_area = chunks[1];
        let status_area = chunks[2];

        let vis_rows = hex_area.height as usize;
        render_hex(f, viewer, hl_style, vis_rows, hex_area);
        render_search_panel(f, viewer, &input, search_panel_area);
        let status = format_status(viewer);
        f.render_widget(Paragraph::new(status).alignment(Alignment::Left), status_area);
        return;
    }

    // Normal mode
    let vis_rows = viewer.visible_rows();
    let (hex_area, info_area_opt, status_area) = layout_normal(area, total_rows);
    render_hex(f, viewer, hl_style, vis_rows, hex_area);
    if let Some(info_area) = info_area_opt {
        if viewer.search == SearchState::Scanning {
            let progress = if viewer.file_size > 0 {
                let pct = (viewer.search_progress * 100) as f64 / viewer.file_size as f64;
                let (eta, throughput) = if let Some(start) = viewer.search_start {
                    let elapsed = start.elapsed();
                    let elapsed_secs = elapsed.as_secs_f64();
                    let remaining = viewer.file_size - viewer.search_offset;
                    let eta_str = format_eta(elapsed, viewer.search_progress, remaining);
                    let throughput_str = if elapsed_secs > 0.0 {
                        let rate = viewer.search_progress as f64 / elapsed_secs;
                        format_throughput(rate)
                    } else {
                        String::new()
                    };
                    (eta_str, throughput_str)
                } else {
                    (String::new(), String::new())
                };
                if !throughput.is_empty() {
                    format!("Searching... {:.1}%  {}  {}", pct, throughput, eta)
                } else {
                    format!("Searching... {:.1}%  {}", pct, eta)
                }
            } else {
                "Searching...".to_string()
            };
            f.render_widget(Paragraph::new(progress).alignment(Alignment::Left), info_area);
        } else if viewer.search == SearchState::MatchFound {
            let msg = if let Some(first) = viewer.search_matches.first() {
                format!(
                    "Match found at offset {:X} ({} matches total)",
                    first,
                    viewer.search_matches.len()
                )
            } else {
                "Match found".to_string()
            };
            f.render_widget(Paragraph::new(msg).alignment(Alignment::Left), info_area);
        } else {
            let vis_start = viewer.scroll_line * viewer.bytes_per_line as u128;
            let needed_len = vis_rows * viewer.bytes_per_line;
            let file_bytes =
                read_bytes_at(&mut viewer.file, vis_start, needed_len).unwrap_or_default();
            let word_info = word_interpretation(viewer, &file_bytes, vis_start);
            f.render_widget(Paragraph::new(word_info).alignment(Alignment::Left), info_area);
        }
    }
    let status = format_status(viewer);
    f.render_widget(Paragraph::new(status).alignment(Alignment::Left), status_area);
}
// hx – terminal hex viewer with configurable grouping, mouse wheel, scrollbar.
// Build: cargo add ratatui crossterm

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::PathBuf,
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, MouseEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::Alignment,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Terminal,
};

// ---------------------------------------------------------------------------
// 128‑bit helpers
// ---------------------------------------------------------------------------
fn u128_to_u64_safe(x: u128) -> Option<u64> {
    if x <= u64::MAX as u128 {
        Some(x as u64)
    } else {
        None
    }
}

fn read_bytes_at(file: &mut File, start: u128, len: usize) -> io::Result<Vec<u8>> {
    let offset = match u128_to_u64_safe(start) {
        Some(off) => off,
        None => return Ok(vec![]),
    };
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len];
    let n = file.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

// ---------------------------------------------------------------------------
// HexViewer state
// ---------------------------------------------------------------------------
const SCROLLBAR_WIDTH: usize = 2; // space + scrollbar char

struct HexViewer {
    file: File,
    file_size: u128,
    filename: PathBuf,
    cursor: u128,          // absolute byte offset (0..=file_size)
    scroll_line: u128,     // first visible line index (0‑based)
    grouping: u8,          // 1,2,4,8
    term_cols: u16,
    term_rows: u16,
    bytes_per_line: usize,
    groups_per_line: usize,
    address_width: usize,
}

impl HexViewer {
    fn open(path: PathBuf) -> io::Result<Self> {
        let file = File::open(&path)?;
        let metadata = file.metadata()?;
        let file_size = if metadata.is_file() {
            metadata.len() as u128
        } else {
            0
        };
        Ok(Self {
            file,
            file_size,
            filename: path,
            cursor: 0,
            scroll_line: 0,
            grouping: 1,
            term_cols: 80,
            term_rows: 24,
            bytes_per_line: 16,
            groups_per_line: 16,
            address_width: 1,
        })
    }

    fn recalc_layout(&mut self) {
        let max_off = self.file_size.saturating_sub(1);
        self.address_width = if max_off == 0 {
            2
        } else {
            (max_off.ilog(16) + 1 + 1) as usize
        };

        let group_size = self.grouping as usize;
        let addr_len = self.address_width;
        let min_line_width = addr_len + 4 + group_size * 2 + group_size + SCROLLBAR_WIDTH;

        let avail_width = self.term_cols.saturating_sub(SCROLLBAR_WIDTH as u16) as usize;

        if avail_width < min_line_width {
            self.bytes_per_line = group_size;
            self.groups_per_line = 1;
            return;
        }

        let constant = addr_len + 4;
        let per_group = 3 * group_size + 1;
        let remaining = avail_width.saturating_sub(constant);
        let mut max_groups = remaining / per_group;
        if max_groups < 1 {
            max_groups = 1;
        }
        self.groups_per_line = max_groups;
        self.bytes_per_line = max_groups * group_size;
    }

    fn total_lines(&self) -> u128 {
        if self.bytes_per_line == 0 {
            return 0;
        }
        (self.file_size + self.bytes_per_line as u128 - 1) / self.bytes_per_line as u128
    }

    fn max_scroll_line(&self) -> u128 {
        self.total_lines().saturating_sub(1)
    }

    fn visible_rows(&self) -> usize {
        if self.term_rows < 3 {
            1
        } else {
            self.term_rows as usize - 2
        }
    }

    fn ensure_cursor_visible(&mut self, visible_rows: usize) {
        let line_of_cursor = if self.bytes_per_line == 0 {
            0
        } else {
            self.cursor / self.bytes_per_line as u128
        };
        let vis_start = self.scroll_line;
        let vis_end = self.scroll_line + visible_rows.saturating_sub(1) as u128;
        if line_of_cursor < vis_start {
            self.scroll_line = line_of_cursor;
        } else if line_of_cursor > vis_end {
            self.scroll_line = line_of_cursor.saturating_sub(visible_rows.saturating_sub(1) as u128);
        }
        self.scroll_line = self.scroll_line.min(self.max_scroll_line());
    }

    fn move_cursor(&mut self, delta: i128, visible_rows: usize) {
        let new = (self.cursor as i128 + delta).max(0).min(self.file_size as i128) as u128;
        self.cursor = new;
        self.ensure_cursor_visible(visible_rows);
    }

    fn page_up(&mut self, visible_rows: usize) {
        let delta = (visible_rows * self.bytes_per_line) as i128;
        self.move_cursor(-delta, visible_rows);
    }

    fn page_down(&mut self, visible_rows: usize) {
        let delta = (visible_rows * self.bytes_per_line) as i128;
        self.move_cursor(delta, visible_rows);
    }

    fn go_home(&mut self, visible_rows: usize) {
        self.cursor = 0;
        self.ensure_cursor_visible(visible_rows);
    }

    fn go_end(&mut self, visible_rows: usize) {
        self.cursor = self.file_size;
        self.ensure_cursor_visible(visible_rows);
    }

    fn scroll_view(&mut self, delta_lines: i128) {
        let visible = self.visible_rows();
        let new_scroll = self.scroll_line as i128 + delta_lines;
        self.scroll_line = new_scroll.max(0) as u128;
        self.scroll_line = self.scroll_line.min(self.max_scroll_line());
        self.ensure_cursor_visible(visible);
    }
}

// ---------------------------------------------------------------------------
// Drawing helpers
// ---------------------------------------------------------------------------
fn highlight_hex(hex_str: &str, byte_index: usize, group_size: usize, hl_style: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut group_idx = 0;
    let bytes = hex_str.as_bytes();
    let len = bytes.len();
    let mut group_start = 0;

    while group_start < len {
        let group_len = (group_size * 2).min(len - group_start);
        let group_str = std::str::from_utf8(&bytes[group_start..group_start + group_len]).unwrap();

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

fn format_line(
    bytes: &[u8],
    offset: u128,
    addr_width: usize,
    group_size: usize,
    bytes_per_line: usize,
    cursor_offset: u128,
    hl_style: Style,
    scrollbar_char: char,
) -> Line<'static> {
    let mut spans = Vec::new();

    // Address
    let addr_str = format!("{:0>width$X}", offset, width = addr_width);
    spans.push(Span::raw(addr_str));
    spans.push(Span::raw("  ".to_string()));

    // Hex groups
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

    let cursor_in_line = if offset <= cursor_offset && (cursor_offset - offset) < bytes_per_line as u128 {
        Some((cursor_offset - offset) as usize)
    } else {
        None
    };

    if let Some(byte_idx) = cursor_in_line {
        spans.extend(highlight_hex(&hex_str, byte_idx, group_size, hl_style));
    } else {
        spans.push(Span::raw(hex_str));
    }

    spans.push(Span::raw("  ".to_string()));

    // ASCII column (right-aligned block, left-aligned content)
    let ascii: String = bytes
        .iter()
        .map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' })
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

    // Scrollbar
    spans.push(Span::raw(format!(" {}", scrollbar_char)));

    Line::from(spans)
}

// ---------------------------------------------------------------------------
// UI rendering
// ---------------------------------------------------------------------------
fn draw_ui(f: &mut ratatui::Frame, viewer: &mut HexViewer, hl_style: Style) {
    let area = f.area();
    let total_rows = area.height as usize;

    let min_width = viewer.address_width + 4 + viewer.grouping as usize * 2 + viewer.grouping as usize + SCROLLBAR_WIDTH;
    if area.width < min_width as u16 {
        let msg = format!(
            "Enlarge terminal to at least {} columns (grouping {}).",
            min_width,
            viewer.grouping
        );
        let p = Paragraph::new(msg).alignment(Alignment::Center);
        f.render_widget(p, area);
        return;
    }

    let vis_rows = viewer.visible_rows();
    let vis_start = viewer.scroll_line * viewer.bytes_per_line as u128;
    let needed_len = vis_rows * viewer.bytes_per_line;
    let file_bytes = read_bytes_at(&mut viewer.file, vis_start, needed_len).unwrap_or_default();

    // Scrollbar calculations
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
            // Empty line past EOF
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
        ));
    }

    // Layout
    let (hex_area, cursor_info_area, status_area) = if total_rows >= 3 {
        let chunks = ratatui::layout::Layout::vertical([
            ratatui::layout::Constraint::Min(1),
            ratatui::layout::Constraint::Length(1),
            ratatui::layout::Constraint::Length(1),
        ])
        .split(area);
        (chunks[0], Some(chunks[1]), chunks[2])
    } else if total_rows == 2 {
        let chunks = ratatui::layout::Layout::vertical([
            ratatui::layout::Constraint::Min(1),
            ratatui::layout::Constraint::Length(1),
        ])
        .split(area);
        (chunks[0], None, chunks[1])
    } else {
        let chunks = ratatui::layout::Layout::vertical([ratatui::layout::Constraint::Length(1)])
            .split(area);
        let status = format_status(viewer);
        f.render_widget(Paragraph::new(status), chunks[0]);
        return;
    };

    let hex_block = Block::default();
    f.render_widget(Paragraph::new(lines).block(hex_block), hex_area);

    if let Some(info_area) = cursor_info_area {
        let word_info = word_interpretation(viewer, &file_bytes, vis_start);
        f.render_widget(Paragraph::new(word_info).alignment(Alignment::Left), info_area);
    }

    let status = format_status(viewer);
    f.render_widget(Paragraph::new(status).alignment(Alignment::Left), status_area);
}

fn format_status(viewer: &HexViewer) -> String {
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
    format!(
        " {}  {:X}  L{}/{}  {}%  [{}b]",
        viewer.filename.display(),
        viewer.cursor,
        line + 1,
        total,
        pct,
        viewer.grouping
    )
}

fn word_interpretation(viewer: &HexViewer, buffer: &[u8], vis_start: u128) -> String {
    let cursor = viewer.cursor;
    let grouping = viewer.grouping as u128;
    if cursor >= viewer.file_size || grouping == 0 {
        return String::new();
    }

    // Align cursor to the start of the group it belongs to
    let aligned = cursor - (cursor % grouping);
    if aligned + grouping > viewer.file_size {
        return String::new(); // not enough bytes for a complete group
    }

    // Check if the aligned group is inside the currently visible buffer
    if aligned < vis_start {
        return String::new(); // group starts before visible window (shouldn't happen if cursor visible)
    }
    let pos_in_buf = (aligned - vis_start) as usize;
    if pos_in_buf + (grouping as usize) > buffer.len() {
        return String::new(); // group not fully in buffer (shouldn't happen)
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
// Main loop
// ---------------------------------------------------------------------------
fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: hx <file>");
        std::process::exit(1);
    }
    let path = PathBuf::from(&args[1]);
    let mut viewer = HexViewer::open(path)?;

    enable_raw_mode()?;
    execute!(io::stdout(), crossterm::event::EnableMouseCapture)?;

    let (cols, rows) = crossterm::terminal::size()?;
    viewer.term_cols = cols;
    viewer.term_rows = rows;
    viewer.recalc_layout();

    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let hl_style = Style::default()
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD);

    loop {
        if event::poll(std::time::Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat => {
                    match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Up => viewer.move_cursor(-(viewer.bytes_per_line as i128), viewer.visible_rows()),
                        KeyCode::Down => viewer.move_cursor(viewer.bytes_per_line as i128, viewer.visible_rows()),
                        KeyCode::Left => viewer.move_cursor(-1, viewer.visible_rows()),
                        KeyCode::Right => viewer.move_cursor(1, viewer.visible_rows()),

                        // Grouping
                        KeyCode::Char('1') => {
                            viewer.grouping = 1;
                            viewer.recalc_layout();
                            viewer.scroll_line = viewer.cursor / viewer.bytes_per_line as u128;
                            viewer.ensure_cursor_visible(viewer.visible_rows());
                        }
                        KeyCode::Char('2') => {
                            viewer.grouping = 2;
                            viewer.recalc_layout();
                            viewer.scroll_line = viewer.cursor / viewer.bytes_per_line as u128;
                            viewer.ensure_cursor_visible(viewer.visible_rows());
                        }
                        KeyCode::Char('3') => {
                            viewer.grouping = 4;
                            viewer.recalc_layout();
                            viewer.scroll_line = viewer.cursor / viewer.bytes_per_line as u128;
                            viewer.ensure_cursor_visible(viewer.visible_rows());
                        }
                        KeyCode::Char('4') => {
                            viewer.grouping = 8;
                            viewer.recalc_layout();
                            viewer.scroll_line = viewer.cursor / viewer.bytes_per_line as u128;
                            viewer.ensure_cursor_visible(viewer.visible_rows());
                        }

                        // Navigation remapped
                        KeyCode::Char('s') => viewer.go_home(viewer.visible_rows()),     // start
                        KeyCode::Char('e') => viewer.go_end(viewer.visible_rows()),      // end
                        KeyCode::Char('u') => viewer.page_up(viewer.visible_rows()),     // page up
                        KeyCode::Char('d') => viewer.page_down(viewer.visible_rows()),   // page down

                        _ => {}
                    }
                }
                Event::Mouse(mouse) => {
                    match mouse.kind {
                        MouseEventKind::ScrollDown => viewer.scroll_view(1),
                        MouseEventKind::ScrollUp => viewer.scroll_view(-1),
                        _ => {}
                    }
                }
                Event::Resize(cols, rows) => {
                    viewer.term_cols = cols;
                    viewer.term_rows = rows;
                    viewer.recalc_layout();
                    viewer.ensure_cursor_visible(viewer.visible_rows());
                }
                _ => {}
            }
        }

        terminal.draw(|f| draw_ui(f, &mut viewer, hl_style))?;
    }

    disable_raw_mode()?;
    execute!(io::stdout(), crossterm::event::DisableMouseCapture)?;
    Ok(())
}
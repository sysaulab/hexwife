// hexwife 0.2.0 – terminal hex viewer with regex search
// Build: cargo add ratatui crossterm memchr regex

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::PathBuf,
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, MouseEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use memchr::memmem;
use regex::bytes::Regex;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Layout, Rect},
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
const SCROLLBAR_WIDTH: usize = 2;
const BLOCK_SIZE: usize = 16 * 1024 * 1024; // 16 MiB search block
const MAX_REGEX_OVERLAP: usize = 4096; // conservative overlap for regex matching

#[derive(PartialEq)]
enum SearchState {
    Inactive,
    Prompt(String),
    Scanning,
    MatchFound,
    NotFound,
}

enum SearchPattern {
    Exact(Vec<u8>),
    Regex(Regex),
}

struct HexViewer {
    file: File,
    file_size: u128,
    filename: PathBuf,
    cursor: u128,
    scroll_line: u128,
    grouping: u8,
    term_cols: u16,
    term_rows: u16,
    bytes_per_line: usize,
    groups_per_line: usize,
    address_width: usize,
    search: SearchState,
    pattern: Option<SearchPattern>,
    search_matches: Vec<u128>,
    search_offset: u128,
    search_progress: u128,
    search_tail: Vec<u8>,
    search_start: Option<Instant>,
    search_error: Option<String>,
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
            search: SearchState::Inactive,
            pattern: None,
            search_matches: Vec::new(),
            search_offset: 0,
            search_progress: 0,
            search_tail: Vec::new(),
            search_start: None,
            search_error: None,
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
            self.scroll_line =
                line_of_cursor.saturating_sub(visible_rows.saturating_sub(1) as u128);
        }
        self.scroll_line = self.scroll_line.min(self.max_scroll_line());
    }

    fn move_cursor(&mut self, delta: i128, visible_rows: usize) {
        let new = (self.cursor as i128 + delta)
            .max(0)
            .min(self.file_size as i128) as u128;
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
}

// ---------------------------------------------------------------------------
// Drawing helpers
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

    // ASCII column (right-aligned)
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

    // Normal mode (including scanning, match found, etc.)
    let vis_rows = viewer.visible_rows();
    let (hex_area, info_area_opt, status_area) = layout_normal(area, total_rows);
    render_hex(f, viewer, hl_style, vis_rows, hex_area);
    if let Some(info_area) = info_area_opt {
        if viewer.search == SearchState::Scanning {
            let progress = if viewer.file_size > 0 {
                let pct = (viewer.search_progress * 100) as f64 / viewer.file_size as f64;
                let eta = if let Some(start) = viewer.search_start {
                    let elapsed = start.elapsed();
                    let remaining = viewer.file_size - viewer.search_offset;
                    format_eta(elapsed, viewer.search_progress, remaining)
                } else {
                    String::new()
                };
                format!("Searching... {:.1}%  {}", pct, eta)
            } else {
                "Searching...".to_string()
            };
            f.render_widget(Paragraph::new(progress).alignment(Alignment::Left), info_area);
        } else if viewer.search == SearchState::MatchFound {
            let msg = if let Some(first) = viewer.search_matches.first() {
                format!("Match found at offset {:X} ({} matches total)", first, viewer.search_matches.len())
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

fn render_hex(
    f: &mut ratatui::Frame,
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

fn render_search_panel(
    f: &mut ratatui::Frame,
    viewer: &HexViewer,
    input: &str,
    area: Rect,
) {
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);
    let prompt_area = chunks[0];
    let help_area = chunks[1];

    // Prompt line: "/" + input + optional error
    let mut prompt_spans = vec![Span::raw("/")];
    prompt_spans.push(Span::raw(input.to_string()));
    if let Some(ref err) = viewer.search_error {
        prompt_spans.push(Span::styled(
            format!(" {}", err),
            Style::default().fg(Color::Red),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(prompt_spans)), prompt_area);

    // Help line (now includes regex)
    let help_text = "Hex: 48 65 6C 6C 6F   ASCII: \"Hello\"   Regex: /[Hh]ex/";
    f.render_widget(Paragraph::new(help_text), help_area);
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
        " {}  {:X}  L{}/{}  {}%  [{}b]{}",
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
    )
}

fn word_interpretation(viewer: &HexViewer, buffer: &[u8], vis_start: u128) -> String {
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
// Search helpers
// ---------------------------------------------------------------------------
fn parse_search_pattern(input: &str) -> Result<SearchPattern, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("empty pattern".to_string());
    }
    // Regex: /pattern/
    if input.starts_with('/') && input.ends_with('/') && input.len() >= 2 {
        let regex_str = &input[1..input.len() - 1];
        match Regex::new(regex_str) {
            Ok(re) => return Ok(SearchPattern::Regex(re)),
            Err(e) => return Err(format!("regex error: {}", e)),
        }
    }
    // ASCII: "text"
    if input.starts_with('"') && input.ends_with('"') && input.len() >= 2 {
        let ascii = &input[1..input.len() - 1];
        if ascii.is_ascii() {
            return Ok(SearchPattern::Exact(ascii.as_bytes().to_vec()));
        } else {
            return Err("ASCII string contains non-ASCII characters".to_string());
        }
    }
    // Hex: whitespace‑separated hex bytes
    let hex_str: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    if hex_str.len() % 2 != 0 {
        return Err("hex string must have an even number of digits".to_string());
    }
    let mut bytes = Vec::with_capacity(hex_str.len() / 2);
    for i in (0..hex_str.len()).step_by(2) {
        match u8::from_str_radix(&hex_str[i..i + 2], 16) {
            Ok(b) => bytes.push(b),
            Err(_) => {
                return Err(format!(
                    "invalid hex digit '{}'",
                    hex_str.chars().nth(i).unwrap()
                ));
            }
        }
    }
    Ok(SearchPattern::Exact(bytes))
}

fn search_step(viewer: &mut HexViewer) -> bool {
    if viewer.search_offset >= viewer.file_size {
        viewer.search = SearchState::NotFound;
        return false;
    }

    // Determine overlap size: for exact patterns use pattern length, for regex use constant
    let overlap = match viewer.pattern {
        Some(SearchPattern::Exact(ref pat)) => {
            if pat.is_empty() { 0 } else { pat.len() - 1 }
        }
        Some(SearchPattern::Regex(_)) => MAX_REGEX_OVERLAP,
        None => return false, // should not happen
    };

    let block_len = BLOCK_SIZE.min((viewer.file_size - viewer.search_offset) as usize);

    let bytes = match read_bytes_at(&mut viewer.file, viewer.search_offset, block_len) {
        Ok(b) => b,
        Err(_) => {
            viewer.search = SearchState::NotFound;
            return false;
        }
    };
    let bytes_len = bytes.len();
    if bytes_len == 0 {
        viewer.search = SearchState::NotFound;
        return false;
    }

    // Build haystack: previous tail + new block
    let mut haystack = viewer.search_tail.clone();
    haystack.extend_from_slice(&bytes);

    let matches: Vec<u128> = match viewer.pattern {
        Some(SearchPattern::Exact(ref pat)) => {
            let finder = memmem::Finder::new(pat);
            finder
                .find_iter(&haystack)
                .map(|pos| viewer.search_offset - viewer.search_tail.len() as u128 + pos as u128)
                .collect()
        }
        Some(SearchPattern::Regex(ref re)) => {
            re.find_iter(&haystack)
                .map(|m| viewer.search_offset - viewer.search_tail.len() as u128 + m.start() as u128)
                .collect()
        }
        None => Vec::new(),
    };

    viewer.search_progress += bytes_len as u128;

    // Update tail for next block
    if bytes_len >= overlap {
        viewer.search_tail = bytes[bytes_len - overlap..].to_vec();
    } else {
        viewer.search_tail.extend_from_slice(&bytes);
        let tail_len = viewer.search_tail.len().min(overlap);
        viewer.search_tail = viewer.search_tail[viewer.search_tail.len() - tail_len..].to_vec();
    }

    viewer.search_offset += bytes_len as u128;

    if !matches.is_empty() {
        viewer.cursor = matches[0];
        viewer.search_matches = matches;
        viewer.search = SearchState::MatchFound;
        viewer.ensure_cursor_visible(viewer.visible_rows());
        return true;
    }

    if viewer.search_offset >= viewer.file_size {
        viewer.search = SearchState::NotFound;
        return false;
    }
    true
}

fn format_eta(elapsed: Duration, bytes_scanned: u128, remaining: u128) -> String {
    if remaining == 0 {
        return "ETA: done".to_string();
    }
    let elapsed_secs = elapsed.as_secs_f64();
    if elapsed_secs == 0.0 || bytes_scanned == 0 {
        return "ETA: calculating...".to_string();
    }
    let rate = bytes_scanned as f64 / elapsed_secs;
    let eta_secs = remaining as f64 / rate;
    let total_secs = eta_secs as u64;

    let years = total_secs / (365 * 24 * 3600);
    let days = (total_secs % (365 * 24 * 3600)) / (24 * 3600);
    let hours = (total_secs % (24 * 3600)) / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    let mut parts = Vec::new();
    if years > 0 { parts.push(format!("{}y", years)); }
    if days > 0 { parts.push(format!("{}d", days)); }
    if hours > 0 { parts.push(format!("{}h", hours)); }
    if minutes > 0 { parts.push(format!("{}m", minutes)); }
    parts.push(format!("{}s", seconds));
    format!("ETA: {}", parts.join(" "))
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
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat => {
                    match viewer.search {
                        SearchState::Prompt(ref mut input) => {
                            match key.code {
                                KeyCode::Esc => {
                                    viewer.search = SearchState::Inactive;
                                    viewer.search_error = None;
                                }
                                KeyCode::Enter => {
                                    match parse_search_pattern(input) {
                                        Ok(pattern) => {
                                            viewer.pattern = Some(pattern);
                                            viewer.search_offset = viewer.cursor + 1;
                                            viewer.search_progress = 0;
                                            viewer.search_matches.clear();
                                            viewer.search_tail.clear();
                                            viewer.search_start = Some(Instant::now());
                                            viewer.search_error = None;
                                            viewer.search = SearchState::Scanning;
                                        }
                                        Err(err) => {
                                            viewer.search_error = Some(err);
                                        }
                                    }
                                }
                                KeyCode::Char(c) => {
                                    input.push(c);
                                    viewer.search_error = None;
                                }
                                KeyCode::Backspace => {
                                    input.pop();
                                    viewer.search_error = None;
                                }
                                _ => {}
                            }
                        }
                        SearchState::Scanning => {
                            if key.code == KeyCode::Esc {
                                viewer.search = SearchState::Inactive;
                                viewer.search_start = None;
                            }
                        }
                        SearchState::MatchFound | SearchState::NotFound => {
                            viewer.search = SearchState::Inactive;
                            viewer.search_matches.clear();
                            viewer.search_start = None;
                            viewer.search_error = None;
                        }
                        SearchState::Inactive => {
                            match key.code {
                                KeyCode::Char('q') => break,
                                KeyCode::Up => viewer.move_cursor(
                                    -(viewer.bytes_per_line as i128),
                                    viewer.visible_rows(),
                                ),
                                KeyCode::Down => viewer.move_cursor(
                                    viewer.bytes_per_line as i128,
                                    viewer.visible_rows(),
                                ),
                                KeyCode::Left => viewer.move_cursor(-1, viewer.visible_rows()),
                                KeyCode::Right => viewer.move_cursor(1, viewer.visible_rows()),
                                KeyCode::Char('s') => viewer.go_home(viewer.visible_rows()),
                                KeyCode::Char('e') => viewer.go_end(viewer.visible_rows()),
                                KeyCode::Char('u') => viewer.page_up(viewer.visible_rows()),
                                KeyCode::Char('d') => viewer.page_down(viewer.visible_rows()),
                                KeyCode::Char('1') => {
                                    viewer.grouping = 1;
                                    viewer.recalc_layout();
                                    viewer.scroll_line =
                                        viewer.cursor / viewer.bytes_per_line as u128;
                                    viewer.ensure_cursor_visible(viewer.visible_rows());
                                }
                                KeyCode::Char('2') => {
                                    viewer.grouping = 2;
                                    viewer.recalc_layout();
                                    viewer.scroll_line =
                                        viewer.cursor / viewer.bytes_per_line as u128;
                                    viewer.ensure_cursor_visible(viewer.visible_rows());
                                }
                                KeyCode::Char('3') => {
                                    viewer.grouping = 4;
                                    viewer.recalc_layout();
                                    viewer.scroll_line =
                                        viewer.cursor / viewer.bytes_per_line as u128;
                                    viewer.ensure_cursor_visible(viewer.visible_rows());
                                }
                                KeyCode::Char('4') => {
                                    viewer.grouping = 8;
                                    viewer.recalc_layout();
                                    viewer.scroll_line =
                                        viewer.cursor / viewer.bytes_per_line as u128;
                                    viewer.ensure_cursor_visible(viewer.visible_rows());
                                }
                                KeyCode::Char('/') => {
                                    viewer.search = SearchState::Prompt(String::new());
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Event::Mouse(mouse) => {
                    if viewer.search == SearchState::Inactive {
                        match mouse.kind {
                            MouseEventKind::ScrollDown => viewer.move_cursor(
                                viewer.bytes_per_line as i128,
                                viewer.visible_rows(),
                            ),
                            MouseEventKind::ScrollUp => viewer.move_cursor(
                                -(viewer.bytes_per_line as i128),
                                viewer.visible_rows(),
                            ),
                            _ => {}
                        }
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

        if viewer.search == SearchState::Scanning {
            if !search_step(&mut viewer) {
                viewer.search_start = None;
            }
        }

        terminal.draw(|f| draw_ui(f, &mut viewer, hl_style))?;
    }

    disable_raw_mode()?;
    execute!(io::stdout(), crossterm::event::DisableMouseCapture)?;
    Ok(())
}
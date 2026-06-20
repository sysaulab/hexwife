use std::{
    fs::{File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    path::PathBuf,
    time::Instant,
};

use crate::constants::SCROLLBAR_WIDTH;
use crate::search::SearchPattern;
use crate::util::u128_to_u64_safe;

#[derive(PartialEq)]
pub enum SearchState {
    Inactive,
    Prompt(String),
    Scanning,
    MatchFound,
    NotFound,
}

pub struct HexViewer {
    pub file: File,
    pub file_size: u128,
    pub filename: PathBuf,
    pub cursor: u128,
    pub scroll_line: u128,
    pub grouping: u8,
    pub term_cols: u16,
    pub term_rows: u16,
    pub bytes_per_line: usize,
    pub groups_per_line: usize,
    pub address_width: usize,
    pub search: SearchState,
    pub pattern: Option<SearchPattern>,
    pub search_matches: Vec<u128>,
    pub search_offset: u128,
    pub search_progress: u128,
    pub search_tail: Vec<u8>,
    pub search_start: Option<Instant>,
    pub search_error: Option<String>,

    // Edit mode
    pub edit_mode: bool,
    pub nibble: u8,        // high nibble already entered (0-15) if nibble_count == 1
    pub nibble_count: u8,  // 0 or 1
}

impl HexViewer {
    pub fn open(path: PathBuf) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)?;
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
            edit_mode: false,
            nibble: 0,
            nibble_count: 0,
        })
    }

    pub fn write_byte_at(&mut self, byte: u8) -> io::Result<()> {
        if self.cursor == self.file_size {
            // Append to the end
            self.file.seek(SeekFrom::End(0))?;
            self.file.write_all(&[byte])?;
            self.file.sync_data()?;
            self.file_size += 1;
        } else {
            let offset = u128_to_u64_safe(self.cursor).ok_or_else(|| {
                io::Error::new(io::ErrorKind::Other, "offset beyond u64")
            })?;
            self.file.seek(SeekFrom::Start(offset))?;
            self.file.write_all(&[byte])?;
            self.file.sync_data()?;
        }
        Ok(())
    }

    pub fn recalc_layout(&mut self) {
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

    pub fn total_lines(&self) -> u128 {
        if self.bytes_per_line == 0 {
            return 0;
        }
        (self.file_size + self.bytes_per_line as u128 - 1) / self.bytes_per_line as u128
    }

    pub fn max_scroll_line(&self) -> u128 {
        self.total_lines().saturating_sub(1)
    }

    pub fn visible_rows(&self) -> usize {
        if self.term_rows < 3 {
            1
        } else {
            self.term_rows as usize - 2
        }
    }

    pub fn ensure_cursor_visible(&mut self, visible_rows: usize) {
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

    pub fn move_cursor(&mut self, delta: i128, visible_rows: usize) {
        let new = (self.cursor as i128 + delta)
            .max(0)
            .min(self.file_size as i128) as u128;
        self.cursor = new;
        self.ensure_cursor_visible(visible_rows);
    }

    pub fn page_up(&mut self, visible_rows: usize) {
        let delta = (visible_rows * self.bytes_per_line) as i128;
        self.move_cursor(-delta, visible_rows);
    }

    pub fn page_down(&mut self, visible_rows: usize) {
        let delta = (visible_rows * self.bytes_per_line) as i128;
        self.move_cursor(delta, visible_rows);
    }

    pub fn go_home(&mut self, visible_rows: usize) {
        self.cursor = 0;
        self.ensure_cursor_visible(visible_rows);
    }

    pub fn go_end(&mut self, visible_rows: usize) {
        self.cursor = self.file_size;
        self.ensure_cursor_visible(visible_rows);
    }
}
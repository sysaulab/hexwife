use std::time::{Duration};

use memchr::memmem;
use regex::bytes::Regex;

use crate::constants::{BLOCK_SIZE, MAX_REGEX_OVERLAP};
use crate::util::read_bytes_at;
use crate::viewer::{HexViewer, SearchState};

pub enum SearchPattern {
    Exact(Vec<u8>),
    Regex(Regex),
}

pub fn parse_search_pattern(input: &str) -> Result<SearchPattern, String> {
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

pub fn search_step(viewer: &mut HexViewer) -> bool {
    if viewer.search_offset >= viewer.file_size {
        viewer.search = SearchState::NotFound;
        viewer.search_start = None; // stop clock
        return false;
    }

    let overlap = match viewer.pattern {
        Some(SearchPattern::Exact(ref pat)) => {
            if pat.is_empty() {
                0
            } else {
                pat.len() - 1
            }
        }
        Some(SearchPattern::Regex(_)) => MAX_REGEX_OVERLAP,
        None => return false,
    };

    let block_len = BLOCK_SIZE.min((viewer.file_size - viewer.search_offset) as usize);

    let bytes = match read_bytes_at(&mut viewer.file, viewer.search_offset, block_len) {
        Ok(b) => b,
        Err(_) => {
            viewer.search = SearchState::NotFound;
            viewer.search_start = None;
            return false;
        }
    };
    let bytes_len = bytes.len();
    if bytes_len == 0 {
        viewer.search = SearchState::NotFound;
        viewer.search_start = None;
        return false;
    }

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
        Some(SearchPattern::Regex(ref re)) => re
            .find_iter(&haystack)
            .map(|m| viewer.search_offset - viewer.search_tail.len() as u128 + m.start() as u128)
            .collect(),
        None => Vec::new(),
    };

    viewer.search_progress += bytes_len as u128;

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
        viewer.search_start = None; // stop clock
        viewer.ensure_cursor_visible(viewer.visible_rows());
        return true;
    }

    if viewer.search_offset >= viewer.file_size {
        viewer.search = SearchState::NotFound;
        viewer.search_start = None;
        return false;
    }
    true
}

pub fn format_eta(elapsed: Duration, bytes_scanned: u128, remaining: u128) -> String {
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
    if years > 0 {
        parts.push(format!("{}y", years));
    }
    if days > 0 {
        parts.push(format!("{}d", days));
    }
    if hours > 0 {
        parts.push(format!("{}h", hours));
    }
    if minutes > 0 {
        parts.push(format!("{}m", minutes));
    }
    parts.push(format!("{}s", seconds));
    format!("ETA: {}", parts.join(" "))
}

pub fn format_throughput(bytes_per_sec: f64) -> String {
    if bytes_per_sec <= 0.0 {
        return String::new();
    }
    let units = ["B/s", "KiB/s", "MiB/s", "GiB/s", "TiB/s", "PiB/s", "EiB/s"];
    let mut value = bytes_per_sec;
    let mut unit_idx = 0;
    while value >= 1024.0 && unit_idx < units.len() - 1 {
        value /= 1024.0;
        unit_idx += 1;
    }
    format!("{:.1} {}", value, units[unit_idx])
}
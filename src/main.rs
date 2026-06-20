mod constants;
mod draw;
mod search;
mod util;
mod viewer;
mod config;

use std::{
    io,
    path::PathBuf,
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    backend::CrosstermBackend,
    style::{Color, Modifier, Style},
    Terminal,
};

use draw::draw_ui;
use search::{parse_search_pattern, search_step};
use viewer::{HexViewer, SearchState};

const HELP_TEXT: &str = r#"
hexwife - a 128-bit hex viewer for theoretical OS limits

USAGE:
    hexwife <file>

KEYBINDINGS (normal mode):
    q              Quit
    Space          Enter edit mode
    /              Search forward
    s, Home        Go to start of file
    e, End         Go to end of file
    u, PageUp      Page up
    d, PageDown    Page down
    1-4            Set grouping (1=byte, 2=short, 3=int, 4=long)
    Arrow keys     Move cursor
    Shift+Left     Go to start (same as Home)
    Shift+Right    Go to end (same as End)
    Shift+Up       Page up (same as u/PageUp)
    Shift+Down     Page down (same as d/PageDown)
    Shift+Scroll   Page up/down
    Mouse wheel    Move cursor one line

SEARCH MODE:
    /pattern       Enter search prompt
    Enter          Start search
    Esc            Cancel search / leave search mode
    Patterns:      Hex: 48 65 6C 6C 6F
                   ASCII: "Hello"
                   Regex: /[Hh]ex/

EDIT MODE:
    Space          Toggle edit mode on/off
    Hex digits     0-9 a-f A-F (two digits per byte)
    Each byte is written immediately to disk. NO UNDO.

CONFIGURATION:
    ~/.hexwife.toml   Persistent settings (grouping)
"#;

fn handle_edit_key(viewer: &mut HexViewer, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Char(c) if c.is_ascii_hexdigit() => {
            let digit = c.to_digit(16).unwrap() as u8;
            if viewer.nibble_count == 0 {
                viewer.nibble = digit << 4;  // high nibble
                viewer.nibble_count = 1;
            } else {
                let byte = viewer.nibble | digit;  // low nibble
                if let Err(_e) = viewer.write_byte_at(byte) {
                    // Silently ignore for now; could set an error field
                }
                viewer.cursor = (viewer.cursor + 1).min(viewer.file_size);
                viewer.ensure_cursor_visible(viewer.visible_rows());
                viewer.nibble_count = 0;
                viewer.nibble = 0;
            }
        }
        KeyCode::Char(' ') => {
            viewer.edit_mode = false;
            viewer.nibble_count = 0;
        }
        KeyCode::Esc => {
            viewer.edit_mode = false;
            viewer.nibble_count = 0;
        }
        _ => {} // ignore all other keys
    }
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 || args[1] == "--help" || args[1] == "-h" {
        eprintln!("{}", HELP_TEXT);
        std::process::exit(if args.len() == 2 { 0 } else { 1 });
    }

    let path = PathBuf::from(&args[1]);
    let mut viewer = HexViewer::open(path)?;

    // Apply user config
    if let Some(cfg) = config::load_config() {
        if let Some(disp) = cfg.display {
            if let Some(g) = disp.grouping {
                if matches!(g, 1 | 2 | 4 | 8) {
                    viewer.grouping = g;
                    viewer.recalc_layout();
                }
            }
        }
    }


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
                            let vis = viewer.visible_rows();
                            // Edit mode intercepts most keys
                            if viewer.edit_mode {
                                handle_edit_key(&mut viewer, key);
                                continue; // skip normal key processing
                            }

                            match key.code {
                                KeyCode::Char('q') => break,
                                KeyCode::Char(' ') => {
                                    viewer.edit_mode = true;
                                    viewer.nibble_count = 0;
                                }
                                KeyCode::Up => {
                                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                                        viewer.page_up(vis);
                                    } else {
                                        viewer.move_cursor(-(viewer.bytes_per_line as i128), vis);
                                    }
                                }
                                KeyCode::Down => {
                                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                                        viewer.page_down(vis);
                                    } else {
                                        viewer.move_cursor(viewer.bytes_per_line as i128, vis);
                                    }
                                }
                                KeyCode::Left => {
                                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                                        viewer.go_home(vis);
                                    } else {
                                        viewer.move_cursor(-1, vis);
                                    }
                                }
                                KeyCode::Right => {
                                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                                        viewer.go_end(vis);
                                    } else {
                                        viewer.move_cursor(1, vis);
                                    }
                                }
                                KeyCode::Home => viewer.go_home(vis),
                                KeyCode::End => viewer.go_end(vis),
                                KeyCode::PageUp => viewer.page_up(vis),
                                KeyCode::PageDown => viewer.page_down(vis),
                                KeyCode::Char('s') => viewer.go_home(vis),
                                KeyCode::Char('e') => viewer.go_end(vis),
                                KeyCode::Char('u') => viewer.page_up(vis),
                                KeyCode::Char('d') => viewer.page_down(vis),
                                KeyCode::Char('1') => {
                                    viewer.grouping = 1;
                                    config::save_config(1);
                                    viewer.recalc_layout();
                                    viewer.scroll_line = viewer.cursor / viewer.bytes_per_line as u128;
                                    viewer.ensure_cursor_visible(vis);
                                }
                                KeyCode::Char('2') => {
                                    viewer.grouping = 2;
                                    config::save_config(2);
                                    viewer.recalc_layout();
                                    viewer.scroll_line = viewer.cursor / viewer.bytes_per_line as u128;
                                    viewer.ensure_cursor_visible(vis);
                                }
                                KeyCode::Char('3') => {
                                    viewer.grouping = 4;
                                    config::save_config(4);
                                    viewer.recalc_layout();
                                    viewer.scroll_line = viewer.cursor / viewer.bytes_per_line as u128;
                                    viewer.ensure_cursor_visible(vis);
                                }
                                KeyCode::Char('4') => {
                                    viewer.grouping = 8;
                                    config::save_config(8);
                                    viewer.recalc_layout();
                                    viewer.scroll_line = viewer.cursor / viewer.bytes_per_line as u128;
                                    viewer.ensure_cursor_visible(vis);
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
                        let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
                        let vis = viewer.visible_rows();
                        match mouse.kind {
                            MouseEventKind::ScrollDown => {
                                if shift {
                                    viewer.page_down(vis);
                                } else {
                                    viewer.move_cursor(viewer.bytes_per_line as i128, vis);
                                }
                            }
                            MouseEventKind::ScrollUp => {
                                if shift {
                                    viewer.page_up(vis);
                                } else {
                                    viewer.move_cursor(-(viewer.bytes_per_line as i128), vis);
                                }
                            }
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
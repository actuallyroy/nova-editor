// Integrated terminal: a ConPTY-backed shell rendered into a cell grid.
//
// portable-pty spawns the shell and gives us a ConPTY whose output is a VT (ANSI)
// byte stream. A background thread streams those bytes to the main loop, which
// feeds them through a `vte` parser into a rows×cols grid of colored cells with a
// cursor + scrollback (the `Perform` impl below). The renderer draws the grid; key
// presses are translated to bytes and written back to the PTY.

use std::io::{Read, Write};
use std::sync::mpsc::{channel, Receiver};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use vte::{Params, Parser, Perform};

/// One terminal cell: a glyph + foreground colour (background omitted for now).
#[derive(Clone, Copy)]
pub struct Cell {
    pub ch: char,
    pub fg: [f32; 4],
}

const DEFAULT_FG: [f32; 4] = [0.83, 0.83, 0.83, 1.0];

impl Cell {
    fn blank() -> Self {
        Cell { ch: ' ', fg: DEFAULT_FG }
    }
}

/// Standard ANSI 16-colour palette (sRGB), indexed 0..16.
const ANSI: [[f32; 4]; 16] = [
    [0.0, 0.0, 0.0, 1.0],       // 0 black
    [0.80, 0.16, 0.13, 1.0],    // 1 red
    [0.13, 0.69, 0.30, 1.0],    // 2 green
    [0.79, 0.62, 0.13, 1.0],    // 3 yellow
    [0.22, 0.45, 0.84, 1.0],    // 4 blue
    [0.74, 0.31, 0.74, 1.0],    // 5 magenta
    [0.20, 0.68, 0.74, 1.0],    // 6 cyan
    [0.80, 0.80, 0.80, 1.0],    // 7 white
    [0.50, 0.50, 0.50, 1.0],    // 8 bright black
    [0.94, 0.35, 0.33, 1.0],    // 9 bright red
    [0.31, 0.84, 0.46, 1.0],    // 10 bright green
    [0.93, 0.79, 0.31, 1.0],    // 11 bright yellow
    [0.40, 0.61, 0.94, 1.0],    // 12 bright blue
    [0.87, 0.50, 0.87, 1.0],    // 13 bright magenta
    [0.36, 0.83, 0.90, 1.0],    // 14 bright cyan
    [0.96, 0.96, 0.96, 1.0],    // 15 bright white
];

/// The screen grid + cursor + scrollback. Implements `vte::Perform` so the parser
/// drives it directly.
/// Saved state for the alternate screen (TUI apps swap to this and back).
struct AltScreen {
    cells: Vec<Vec<Cell>>,
    cur_row: usize,
    cur_col: usize,
    scroll_top: usize,
    scroll_bottom: usize,
}

pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<Vec<Cell>>, // rows × cols (current screen)
    pub scrollback: Vec<Vec<Cell>>,
    pub cur_row: usize,
    pub cur_col: usize,
    cur_fg: [f32; 4],
    scroll_top: usize,    // scroll region top row (inclusive)
    scroll_bottom: usize, // scroll region bottom row (inclusive)
    saved_cursor: (usize, usize),
    alt: Option<AltScreen>,
}

const MAX_SCROLLBACK: usize = 5000;

impl Grid {
    fn new(rows: usize, cols: usize) -> Self {
        Self {
            cols,
            rows,
            cells: vec![vec![Cell::blank(); cols]; rows],
            scrollback: Vec::new(),
            cur_row: 0,
            cur_col: 0,
            cur_fg: DEFAULT_FG,
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            saved_cursor: (0, 0),
            alt: None,
        }
    }

    fn resize(&mut self, rows: usize, cols: usize) {
        if rows == self.rows && cols == self.cols {
            return;
        }
        for row in &mut self.cells {
            row.resize(cols, Cell::blank());
        }
        self.cells.resize(rows, vec![Cell::blank(); cols]);
        self.rows = rows;
        self.cols = cols;
        self.scroll_top = 0;
        self.scroll_bottom = rows.saturating_sub(1);
        self.cur_row = self.cur_row.min(rows.saturating_sub(1));
        self.cur_col = self.cur_col.min(cols.saturating_sub(1));
    }

    fn enter_alt(&mut self) {
        if self.alt.is_some() {
            return;
        }
        self.alt = Some(AltScreen {
            cells: std::mem::replace(&mut self.cells, vec![vec![Cell::blank(); self.cols]; self.rows]),
            cur_row: self.cur_row,
            cur_col: self.cur_col,
            scroll_top: self.scroll_top,
            scroll_bottom: self.scroll_bottom,
        });
        self.cur_row = 0;
        self.cur_col = 0;
        self.scroll_top = 0;
        self.scroll_bottom = self.rows.saturating_sub(1);
    }

    fn leave_alt(&mut self) {
        if let Some(a) = self.alt.take() {
            self.cells = a.cells;
            for row in &mut self.cells {
                row.resize(self.cols, Cell::blank());
            }
            self.cells.resize(self.rows, vec![Cell::blank(); self.cols]);
            self.cur_row = a.cur_row.min(self.rows.saturating_sub(1));
            self.cur_col = a.cur_col.min(self.cols.saturating_sub(1));
            self.scroll_top = a.scroll_top.min(self.rows.saturating_sub(1));
            self.scroll_bottom = a.scroll_bottom.min(self.rows.saturating_sub(1));
        }
    }

    fn newline(&mut self) {
        if self.cur_row == self.scroll_bottom {
            self.scroll_up_region();
        } else if self.cur_row + 1 < self.rows {
            self.cur_row += 1;
        }
    }

    /// Scroll the active region up by one line, blanking the bottom. When the region
    /// starts at the top of the screen, the displaced line goes to scrollback.
    fn scroll_up_region(&mut self) {
        let (top, bot) = (self.scroll_top, self.scroll_bottom.min(self.rows - 1));
        if bot <= top {
            return;
        }
        if top == 0 {
            let line = self.cells[top].clone();
            self.scrollback.push(line);
            if self.scrollback.len() > MAX_SCROLLBACK {
                self.scrollback.remove(0);
            }
        }
        self.cells[top..=bot].rotate_left(1);
        self.cells[bot] = vec![Cell::blank(); self.cols];
    }

    /// Scroll the active region down by one line, blanking the top (reverse index).
    fn scroll_down_region(&mut self) {
        let (top, bot) = (self.scroll_top, self.scroll_bottom.min(self.rows - 1));
        if bot <= top {
            return;
        }
        self.cells[top..=bot].rotate_right(1);
        self.cells[top] = vec![Cell::blank(); self.cols];
    }

    /// Insert `n` blank lines at the cursor (within the scroll region).
    fn insert_lines(&mut self, n: usize) {
        let (top, bot) = (self.cur_row, self.scroll_bottom.min(self.rows - 1));
        if self.cur_row < self.scroll_top || self.cur_row > bot {
            return;
        }
        for _ in 0..n.min(bot - top + 1) {
            self.cells[top..=bot].rotate_right(1);
            self.cells[top] = vec![Cell::blank(); self.cols];
        }
    }

    /// Delete `n` lines at the cursor (within the scroll region).
    fn delete_lines(&mut self, n: usize) {
        let (top, bot) = (self.cur_row, self.scroll_bottom.min(self.rows - 1));
        if self.cur_row < self.scroll_top || self.cur_row > bot {
            return;
        }
        for _ in 0..n.min(bot - top + 1) {
            self.cells[top..=bot].rotate_left(1);
            self.cells[bot] = vec![Cell::blank(); self.cols];
        }
    }

    fn erase_in_line(&mut self, mode: u16) {
        let (a, b) = match mode {
            1 => (0, self.cur_col + 1),       // start..=cursor
            2 => (0, self.cols),              // whole line
            _ => (self.cur_col, self.cols),   // cursor..end
        };
        if let Some(row) = self.cells.get_mut(self.cur_row) {
            for c in row.iter_mut().take(b.min(self.cols)).skip(a) {
                *c = Cell::blank();
            }
        }
    }

    fn erase_in_display(&mut self, mode: u16) {
        match mode {
            2 | 3 => {
                for row in &mut self.cells {
                    for c in row.iter_mut() {
                        *c = Cell::blank();
                    }
                }
                self.cur_row = 0;
                self.cur_col = 0;
            }
            1 => {
                for r in 0..=self.cur_row.min(self.rows - 1) {
                    for c in self.cells[r].iter_mut() {
                        *c = Cell::blank();
                    }
                }
            }
            _ => {
                self.erase_in_line(0);
                for r in (self.cur_row + 1)..self.rows {
                    for c in self.cells[r].iter_mut() {
                        *c = Cell::blank();
                    }
                }
            }
        }
    }

    /// Apply an SGR (Select Graphic Rendition) sequence — colors only for now.
    fn sgr(&mut self, params: &Params) {
        let flat: Vec<u16> = params.iter().map(|p| p.first().copied().unwrap_or(0)).collect();
        if flat.is_empty() {
            self.cur_fg = DEFAULT_FG;
        }
        let mut i = 0;
        while i < flat.len() {
            match flat[i] {
                0 => self.cur_fg = DEFAULT_FG,
                30..=37 => self.cur_fg = ANSI[(flat[i] - 30) as usize],
                90..=97 => self.cur_fg = ANSI[(flat[i] - 90 + 8) as usize],
                39 => self.cur_fg = DEFAULT_FG,
                38 => {
                    // 38;5;n (256) or 38;2;r;g;b (truecolor) — approximate.
                    if flat.get(i + 1) == Some(&5) {
                        if let Some(&n) = flat.get(i + 2) {
                            self.cur_fg = xterm256(n as u8);
                        }
                        i += 2;
                    } else if flat.get(i + 1) == Some(&2) {
                        if let (Some(&r), Some(&g), Some(&b)) =
                            (flat.get(i + 2), flat.get(i + 3), flat.get(i + 4))
                        {
                            self.cur_fg = [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0];
                        }
                        i += 4;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}

/// Approximate an xterm-256 color index as RGB.
fn xterm256(n: u8) -> [f32; 4] {
    match n {
        0..=15 => ANSI[n as usize],
        16..=231 => {
            let n = n - 16;
            let r = n / 36;
            let g = (n % 36) / 6;
            let b = n % 6;
            let lv = |v: u8| if v == 0 { 0.0 } else { (55.0 + v as f32 * 40.0) / 255.0 };
            [lv(r), lv(g), lv(b), 1.0]
        }
        _ => {
            let v = (8 + (n - 232) as u32 * 10) as f32 / 255.0;
            [v, v, v, 1.0]
        }
    }
}

impl Perform for Grid {
    fn print(&mut self, c: char) {
        if self.cur_col >= self.cols {
            self.cur_col = 0;
            self.newline();
        }
        if let Some(cell) = self.cells.get_mut(self.cur_row).and_then(|r| r.get_mut(self.cur_col)) {
            *cell = Cell { ch: c, fg: self.cur_fg };
        }
        self.cur_col += 1;
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.newline(),
            b'\r' => self.cur_col = 0,
            0x08 => self.cur_col = self.cur_col.saturating_sub(1),
            b'\t' => self.cur_col = ((self.cur_col / 8) + 1) * 8,
            _ => {}
        }
        if self.cur_col >= self.cols {
            self.cur_col = self.cols - 1;
        }
    }

    fn csi_dispatch(&mut self, params: &Params, inter: &[u8], _ignore: bool, action: char) {
        let p1 = params.iter().next().and_then(|p| p.first().copied()).unwrap_or(0);
        let n = p1.max(1) as usize;
        // Private modes (CSI ? Pm h/l) — alternate screen buffer.
        if inter.contains(&b'?') {
            if action == 'h' || action == 'l' {
                let set = action == 'h';
                for p in params.iter() {
                    match p.first().copied().unwrap_or(0) {
                        47 | 1047 | 1049 => {
                            if set {
                                self.enter_alt();
                            } else {
                                self.leave_alt();
                            }
                        }
                        _ => {} // 25 (cursor vis), 2004 (bracketed paste), mouse modes — ignored
                    }
                }
            }
            return;
        }
        match action {
            'm' => self.sgr(params),
            'H' | 'f' => {
                let mut it = params.iter();
                let r = it.next().and_then(|p| p.first().copied()).unwrap_or(1).max(1) as usize;
                let c = it.next().and_then(|p| p.first().copied()).unwrap_or(1).max(1) as usize;
                self.cur_row = (r - 1).min(self.rows - 1);
                self.cur_col = (c - 1).min(self.cols - 1);
            }
            'A' => self.cur_row = self.cur_row.saturating_sub(n),
            'B' => self.cur_row = (self.cur_row + n).min(self.rows - 1),
            'C' => self.cur_col = (self.cur_col + n).min(self.cols - 1),
            'D' => self.cur_col = self.cur_col.saturating_sub(n),
            'G' => self.cur_col = (n - 1).min(self.cols - 1),
            'd' => self.cur_row = (n - 1).min(self.rows - 1),
            'J' => self.erase_in_display(p1),
            'K' => self.erase_in_line(p1),
            'L' => self.insert_lines(n),
            'M' => self.delete_lines(n),
            'S' => {
                for _ in 0..n {
                    self.scroll_up_region();
                }
            }
            'T' => {
                for _ in 0..n {
                    self.scroll_down_region();
                }
            }
            'X' => {
                // Erase n chars from the cursor.
                if let Some(row) = self.cells.get_mut(self.cur_row) {
                    for c in row.iter_mut().skip(self.cur_col).take(n) {
                        *c = Cell::blank();
                    }
                }
            }
            'r' => {
                // DECSTBM: set scroll region (1-based top;bottom).
                let mut it = params.iter();
                let top = it.next().and_then(|p| p.first().copied()).unwrap_or(1).max(1) as usize - 1;
                let bot = it
                    .next()
                    .and_then(|p| p.first().copied())
                    .map(|v| v as usize - 1)
                    .unwrap_or(self.rows - 1);
                self.scroll_top = top.min(self.rows - 1);
                self.scroll_bottom = bot.min(self.rows - 1).max(self.scroll_top);
                self.cur_row = 0;
                self.cur_col = 0;
            }
            's' => self.saved_cursor = (self.cur_row, self.cur_col),
            'u' => {
                self.cur_row = self.saved_cursor.0.min(self.rows - 1);
                self.cur_col = self.saved_cursor.1.min(self.cols - 1);
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _inter: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'7' => self.saved_cursor = (self.cur_row, self.cur_col), // DECSC
            b'8' => {
                self.cur_row = self.saved_cursor.0.min(self.rows - 1);
                self.cur_col = self.saved_cursor.1.min(self.cols - 1);
            }
            b'D' => self.newline(),          // index
            b'M' => {
                // Reverse index: up, scrolling the region down at the top.
                if self.cur_row == self.scroll_top {
                    self.scroll_down_region();
                } else {
                    self.cur_row = self.cur_row.saturating_sub(1);
                }
            }
            _ => {}
        }
    }
    fn hook(&mut self, _: &Params, _: &[u8], _: bool, _: char) {}
    fn put(&mut self, _: u8) {}
    fn unhook(&mut self) {}
    fn osc_dispatch(&mut self, _: &[&[u8]], _: bool) {}
}

pub struct Terminal {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    _child: Box<dyn Child + Send + Sync>,
    rx: Receiver<Vec<u8>>,
    parser: Parser,
    pub grid: Grid,
}

impl Terminal {
    /// Spawn the platform shell in a ConPTY sized to `rows`×`cols`.
    pub fn spawn(rows: usize, cols: usize) -> Option<Self> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows: rows as u16, cols: cols as u16, pixel_width: 0, pixel_height: 0 })
            .ok()?;
        let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        let cmd = CommandBuilder::new(shell);
        let child = pair.slave.spawn_command(cmd).ok()?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().ok()?;
        let writer = pair.master.take_writer().ok()?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Some(Self {
            master: pair.master,
            writer,
            _child: child,
            rx,
            parser: Parser::new(),
            grid: Grid::new(rows, cols),
        })
    }

    /// Drain any pending shell output into the grid. Returns true if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(chunk) = self.rx.try_recv() {
            for b in chunk {
                self.parser.advance(&mut self.grid, b);
            }
            changed = true;
        }
        changed
    }

    /// Send raw bytes (translated key input) to the shell.
    pub fn write(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// (cols, rows) of the current grid.
    pub fn dims(&self) -> (usize, usize) {
        (self.grid.cols, self.grid.rows)
    }

    /// Cursor cell position (col, row) within the visible grid.
    pub fn cursor(&self) -> (usize, usize) {
        (self.grid.cur_col, self.grid.cur_row)
    }

    /// Rich spans for the visible grid: per row, runs of same-colored cells, rows
    /// joined by '\n'. Trailing blanks per row are dropped to keep it compact.
    pub fn visual_spans(&self) -> Vec<(String, [f32; 4])> {
        let mut out: Vec<(String, [f32; 4])> = Vec::new();
        for (ri, row) in self.grid.cells.iter().enumerate() {
            // Find last non-blank cell so we don't emit trailing spaces.
            let last = row.iter().rposition(|c| c.ch != ' ').map(|i| i + 1).unwrap_or(0);
            let mut col = 0;
            while col < last {
                let fg = row[col].fg;
                let start = col;
                while col < last && row[col].fg == fg {
                    col += 1;
                }
                let text: String = row[start..col].iter().map(|c| c.ch).collect();
                out.push((text, fg));
            }
            if ri + 1 < self.grid.cells.len() {
                out.push(("\n".to_string(), DEFAULT_FG));
            }
        }
        out
    }

    pub fn resize(&mut self, rows: usize, cols: usize) {
        let _ = self.master.resize(PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: 0,
            pixel_height: 0,
        });
        self.grid.resize(rows, cols);
    }
}

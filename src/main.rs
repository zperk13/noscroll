use clap::Parser;
use crossterm::{cursor, execute, queue};
use droppable_process::prelude::*;
use std::ffi::OsStr;
use std::io::{Read, StdoutLock, Write};
use std::sync::{Arc, Mutex};
use tinyvec::ArrayVec;
use unicode_width::UnicodeWidthChar;

const BUFFER_SIZE: usize = 4096;

fn stdout_title(s: impl std::fmt::Display) -> Box<str> {
    format!("OUT: {s}").into()
}

fn stderr_title(s: impl std::fmt::Display) -> Box<str> {
    format!("ERR: {s}").into()
}

#[derive(Parser, Debug)]
#[command()]
struct Args {
    #[arg(long, short)]
    commands: Vec<Arc<OsStr>>,

    #[arg(long, short = 'r')]
    max_height: Option<u16>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RenderType {
    Content,
    Everything,
    EverythingExceptContent,
}

impl RenderType {
    fn should_draw_titles(&self) -> bool {
        match self {
            RenderType::Content => false,
            RenderType::Everything => true,
            RenderType::EverythingExceptContent => true,
        }
    }
    fn should_draw_borders(&self) -> bool {
        match self {
            RenderType::Content => false,
            RenderType::Everything => true,
            RenderType::EverythingExceptContent => true,
        }
    }
    fn should_draw_content(&self) -> bool {
        match self {
            RenderType::Content => true,
            RenderType::Everything => true,
            RenderType::EverythingExceptContent => false,
        }
    }
}

struct TerminalManager<'a> {
    stdout: StdoutLock<'a>,
    titles: Arc<Mutex<Vec<Box<str>>>>,
    size_info: SizeInfo,
    pane_count: usize,
}

impl<'a> TerminalManager<'a> {
    fn new(
        mut stdout: StdoutLock<'a>,
        titles: Arc<Mutex<Vec<Box<str>>>>,
        size_info: SizeInfo,
        pane_count: usize,
    ) -> Self {
        queue!(
            stdout,
            crossterm::cursor::SavePosition,
            crossterm::cursor::Hide,
        )
        .unwrap();

        crossterm::terminal::enable_raw_mode().unwrap();
        Self {
            stdout,
            titles,
            size_info,
            pane_count, // title,
                        // lines: Vec::with_capacity(inner_height.into()),
        }
    }

    fn render(&mut self, parser_pairs: &[ParserPair], render_type: RenderType) {
        if render_type.should_draw_titles() {
            queue!(self.stdout, crossterm::cursor::RestorePosition).unwrap();
            let mut to_write = String::with_capacity(self.size_info.terminal_columns as usize);
            to_write.push('╭');
            let titles = self.titles.lock().unwrap();
            for (index, title) in titles.iter().enumerate() {
                let mut remaining_straight_lines_to_print =
                    self.size_info.pane_inner_columns as usize;
                for ch in title.chars() {
                    let ch_width = ch.width().unwrap();
                    if ch_width > remaining_straight_lines_to_print {
                        break;
                    }
                    to_write.push(ch);
                    remaining_straight_lines_to_print -= ch_width;
                }
                for _ in 0..remaining_straight_lines_to_print {
                    to_write.push('─');
                }
                if index == titles.len() - 1 {
                    to_write.push('╮');
                } else {
                    to_write.push('┬');
                }
            }
            write!(self.stdout, "{}", to_write).unwrap();
        }
        if render_type.should_draw_borders() {
            for _ in 0..self.size_info.pane_inner_rows {
                write!(self.stdout, "\n").unwrap();
                queue!(self.stdout, crossterm::cursor::MoveToColumn(0)).unwrap();
                for _ in 0..self.pane_count + 1 {
                    write!(self.stdout, "│").unwrap();
                    queue!(
                        self.stdout,
                        crossterm::cursor::MoveRight(self.size_info.pane_inner_columns)
                    )
                    .unwrap();
                }
            }
            queue!(self.stdout, crossterm::cursor::MoveToColumn(0),).unwrap();
            let mut to_write = String::with_capacity(self.size_info.terminal_columns as usize);
            to_write.push('╰');
            for i in 0..self.pane_count {
                for _ in 0..self.size_info.pane_inner_columns {
                    to_write.push('─');
                }
                if i == self.pane_count - 1 {
                    to_write.push('╯');
                } else {
                    to_write.push('┴');
                }
            }
            write!(self.stdout, "\n{}", to_write).unwrap();
        }

        if render_type.should_draw_content() {
            for (parser_index, parser) in parser_pairs
                .iter()
                .flat_map(ParserPair::as_array)
                .enumerate()
            {
                for (row_index, row) in parser
                    .screen()
                    .rows_formatted(0, self.size_info.pane_inner_columns)
                    .enumerate()
                {
                    queue!(
                        self.stdout,
                        crossterm::cursor::RestorePosition,
                        crossterm::cursor::MoveUp(
                            self.size_info.pane_inner_rows - row_index as u16
                        ),
                        crossterm::cursor::MoveRight(
                            1 + parser_index as u16 * (self.size_info.pane_inner_columns + 1)
                        )
                    )
                    .unwrap();
                    // write!(self.stdout, "{:?}", row).unwrap();
                    self.stdout.write_all(&row).unwrap();
                }
            }
            self.stdout.flush().unwrap();
        }
    }
}

impl Drop for TerminalManager<'_> {
    fn drop(&mut self) {
        let _ = execute!(
            self.stdout,
            cursor::MoveToColumn(0),
            cursor::MoveDown(self.size_info.terminal_rows)
        );
        println!();
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = execute!(self.stdout, cursor::Show);
    }
}

#[derive(Debug, Copy, Clone)]
enum OutputChannel {
    Stdout,
    Stderr,
}

#[derive(Debug)]
struct Message {
    process_index: usize,
    output_channel: OutputChannel,
    bytes: ArrayVec<[u8; BUFFER_SIZE]>,
}

struct ParserPair {
    stdout: vt100::Parser,
    stderr: vt100::Parser,
}

impl ParserPair {
    fn new(rows: u16, cols: u16, scrollback_len: usize) -> ParserPair {
        ParserPair {
            stdout: vt100::Parser::new(rows, cols, scrollback_len),
            stderr: vt100::Parser::new(rows, cols, scrollback_len),
        }
    }

    fn get_mut(&mut self, output_channel: OutputChannel) -> &mut vt100::Parser {
        match output_channel {
            OutputChannel::Stdout => &mut self.stdout,
            OutputChannel::Stderr => &mut self.stderr,
        }
    }

    fn as_array(&self) -> [&vt100::Parser; 2] {
        [&self.stdout, &self.stderr]
    }
}

#[derive(Clone, Copy)]
struct SizeInfo {
    terminal_columns: u16,
    terminal_rows: u16,
    pane_outer_rows: u16,
    pane_inner_columns: u16,
    pane_inner_rows: u16,
}

fn calculate_size(
    terminal_columns: u16,
    terminal_rows: u16,
    pane_count: u16,
    max_height: Option<u16>,
) -> SizeInfo {
    let terminal_rows = terminal_rows.min(max_height.unwrap_or(u16::MAX));
    let pane_inner_columns = (terminal_columns - 1) / (pane_count) - 1;
    SizeInfo {
        terminal_columns,
        terminal_rows,
        pane_outer_rows: terminal_rows,
        pane_inner_columns,
        pane_inner_rows: terminal_rows - 2,
    }
}

fn main() {
    let args = Args::parse();

    if args.commands.is_empty() {
        eprintln!("Requires at least 1 command");
        return;
    }

    let (tx, rx) = std::sync::mpsc::channel();

    let (terminal_columns, terminal_rows) = crossterm::terminal::size().unwrap();

    let pane_count = args.commands.len() * 2;

    let titles: Arc<Mutex<Vec<Box<str>>>> = Arc::new(Mutex::new(
        args.commands
            .iter()
            .flat_map(|command| {
                let s = command.to_string_lossy();
                [stdout_title(&s), stderr_title(s)]
            })
            .collect(),
    ));

    let size_info = calculate_size(
        terminal_columns,
        terminal_rows,
        pane_count as u16,
        args.max_height,
    );

    let mut terminal_manager = TerminalManager::new(
        std::io::stdout().lock(),
        titles.clone(),
        size_info,
        pane_count,
    );

    let mut parsers = Vec::with_capacity(args.commands.len());
    for _ in 0..args.commands.len() {
        parsers.push(ParserPair::new(
            size_info.pane_inner_rows,
            size_info.pane_inner_columns,
            0,
        ));
    }

    terminal_manager.render(&parsers, RenderType::EverythingExceptContent);

    let threads: Vec<std::thread::JoinHandle<()>> = args
        .commands
        .into_iter()
        .enumerate()
        .map(|(process_index, command)| {
            let tx = tx.clone();
            std::thread::spawn(move || {
                use std::process::Stdio;
                let mut process = DroppableProcess(std::process::Command::new("bash")
                    .arg("-c")
                    .arg(command)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap());
                let mut stdout = process.0.stdout.take().unwrap();
                // let mut buf = Vec::new();
                // println!("to end: {:?}", stdout.read_to_end(&mut buf));
                let mut stderr = process.0.stderr.take().unwrap();

                macro_rules! spawn_thread {
                    ($child_channel:ident, $channel_type:expr, $tx:ident) => {
                        std::thread::spawn(move || {
                            loop {
                                // Halved so we can fit as many \r as we need
                                let mut buffer = [0u8; BUFFER_SIZE / 2];

                                let read_amount = $child_channel.read(&mut buffer).unwrap();
                                if read_amount == 0 {
                                    break;
                                }
                                let mut message_bytes = ArrayVec::new();
                                for byte in &buffer[..read_amount] {
                                    message_bytes.push(*byte);
                                    if *byte == b'\n' {
                                        message_bytes.push(b'\r');
                                    }
                                }
                                let msg = Message {
                                    process_index,
                                    output_channel: $channel_type,
                                    bytes: message_bytes,
                                };
                                if $tx.send(msg).is_err() {
                                    break;
                                };
                            }
                        });
                    };
                }

                let stdout_tx = tx.clone();
                let stderr_tx = tx;
                spawn_thread!(stdout, OutputChannel::Stdout, stdout_tx);
                spawn_thread!(stderr, OutputChannel::Stderr, stderr_tx);

                process.0.wait().unwrap();
            })
        })
        .collect();
    drop(tx);

    loop {
        let mut time_to_end = false;
        let mut render_type = match rx.recv_timeout(std::time::Duration::from_secs(0)) {
            Ok(message) => {
                parsers[message.process_index]
                    .get_mut(message.output_channel)
                    .process(&message.bytes);
                Some(RenderType::Content)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                time_to_end = true;
                None
            }
        };
        loop {
            if crossterm::event::poll(std::time::Duration::from_secs(0)).unwrap() {
                let event = crossterm::event::read().unwrap();
                match event {
                    crossterm::event::Event::FocusGained
                    | crossterm::event::Event::FocusLost
                    | crossterm::event::Event::Mouse(_)
                    | crossterm::event::Event::Paste(_) => {}
                    crossterm::event::Event::Key(key_event) => {
                        if key_event.code == crossterm::event::KeyCode::Esc {
                            time_to_end = true;
                        }
                    }
                    crossterm::event::Event::Resize(terminal_columns, terminal_rows) => {
                        render_type = Some(RenderType::Everything);
                        terminal_manager.size_info = calculate_size(
                            terminal_columns,
                            terminal_rows,
                            pane_count as u16,
                            args.max_height,
                        )
                    }
                }
            } else {
                break;
            }
        }
        if let Some(render_type) = render_type {
            terminal_manager.render(&parsers, render_type);
        }
        if time_to_end {
            break;
        }
    }
    drop(terminal_manager);
    for thread in threads {
        if thread.is_finished() {
            if let Err(err) = thread.join() {
                eprintln!("Error: {err:?}");
            }
        }
    }
}

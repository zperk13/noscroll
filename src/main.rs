use clap::{Parser, ValueEnum};
use droppable_process::prelude::*;
use ratatui::crossterm;
use ratatui::layout::Spacing;
use ratatui::prelude::*;
use ratatui::symbols::merge::MergeStrategy;
use ratatui::widgets::Block;
use std::borrow::Cow;
use std::cell::OnceCell;
use std::ffi::OsStr;
use std::io::Read;
use std::thread::JoinHandle;
use tinyvec::ArrayVec;
use std::fmt::Display;

const BUFFER_SIZE: usize = 4096;

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum OverallLayout {
    Horizontal,
    #[default]
    Vertical,
}

impl Display for OverallLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", match self {
            OverallLayout::Horizontal => "horizontal",
            OverallLayout::Vertical => "vertical",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum SplitLayout {
    // Combined,
    #[default]
    Horizontal,
    Vertical,
}
impl Display for SplitLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", match self {
            SplitLayout::Horizontal => "horizontal",
            SplitLayout::Vertical => "vertical",
        })
    }
}

#[derive(Parser, Debug)]
#[command()]
struct Args {
    #[arg(long, short)]
    commands: Vec<Box<OsStr>>,

    #[arg(long, default_value_t=OverallLayout::Vertical)]
    /// Layout between commands
    overall_layout: OverallLayout,

    #[arg(long, default_value_t=SplitLayout::Horizontal)]
    /// Layout between the stdout and stderr of a command
    split_layout: SplitLayout,

    #[arg(long, short = 'r')]
    /// The height of what is rendered.
    /// The height of emulated terminals is rows-2 since a borders are rendered.
    /// If not provided, your terminal height will be used.
    rows: Option<u16>,
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

struct Process<'a> {
    title: Span<'a>,
    thread: std::thread::JoinHandle<()>,
    parser_pair: OnceCell<ParserPair>,
    layout: SplitLayout,
}

impl<'a> Process<'a> {
    fn new(title: Cow<'a, str>, thread: JoinHandle<()>, layout: SplitLayout) -> Self {
        Self {
            title: Span {
                style: Style::new(),
                content: title,
            },
            thread,
            parser_pair: OnceCell::new(),
            layout,
        }
    }

    fn get_mut(&mut self, channel: OutputChannel) -> &mut vt100::Parser {
        match channel {
            OutputChannel::Stdout => &mut self.parser_pair.get_mut().unwrap().stdout,
            OutputChannel::Stderr => &mut self.parser_pair.get_mut().unwrap().stderr,
        }
    }
}

fn vt100_color_to_ratatui_color(vt100_color: vt100::Color) -> ratatui::prelude::Color {
    match vt100_color {
        vt100::Color::Default => ratatui::prelude::Color::default(),
        vt100::Color::Idx(i) => ratatui::prelude::Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => ratatui::prelude::Color::Rgb(r, g, b),
    }
}

impl Widget for &mut Process<'_> {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        let [left_area, right_area] = Layout::new(
            match self.layout {
                SplitLayout::Horizontal => Direction::Horizontal,
                SplitLayout::Vertical => Direction::Vertical,
            },
            [Constraint::Fill(1), Constraint::Fill(1)],
        )
        .spacing(Spacing::Overlap(1))
        .areas(area);
        let left_block = Block::bordered()
            .title(vec!["stdout ".into(), self.title.clone()])
            .merge_borders(MergeStrategy::Exact);
        let right_block = Block::bordered()
            .title(vec!["stderr ".into(), self.title.clone()])
            .merge_borders(MergeStrategy::Exact);
        let left_inner_area = left_block.inner(left_area);
        let right_inner_area = right_block.inner(right_area);
        left_block.render(left_area, buf);
        right_block.render(right_area, buf);
        let _ = self.parser_pair.get_or_init(|| ParserPair {
            stdout: vt100::Parser::new(left_inner_area.height, left_inner_area.width, 0),
            stderr: vt100::Parser::new(right_inner_area.height, right_inner_area.width, 0),
        });
        let parser_pair = self.parser_pair.get_mut().unwrap();
        parser_pair
            .stdout
            .screen_mut()
            .set_size(left_inner_area.height, left_inner_area.width);
        parser_pair
            .stderr
            .screen_mut()
            .set_size(right_inner_area.height, right_inner_area.width);
        let mut render_parser = |parser: &vt100::Parser, area: Rect| {
            for y in 0..area.height {
                for x in 0..area.width {
                    let parser_cell = parser.screen().cell(y, x).unwrap();
                    let buffer_cell = buf.cell_mut(Position::new(x + area.x, y + area.y)).unwrap();
                    if parser_cell.has_contents() {
                        buffer_cell.set_symbol(parser_cell.contents());
                    }
                    let mut modifier = Modifier::default();
                    if parser_cell.bold() {
                        modifier |= Modifier::BOLD;
                    }
                    if parser_cell.dim() {
                        modifier |= Modifier::DIM;
                    }
                    if parser_cell.italic() {
                        modifier |= Modifier::ITALIC;
                    }
                    if parser_cell.underline() {
                        modifier |= Modifier::UNDERLINED;
                    }
                    if parser_cell.inverse() {
                        modifier |= Modifier::REVERSED;
                    }
                    buffer_cell.set_style(Style {
                        fg: Some(vt100_color_to_ratatui_color(parser_cell.fgcolor())),
                        bg: Some(vt100_color_to_ratatui_color(parser_cell.bgcolor())),
                        underline_color: None,
                        add_modifier: modifier,
                        sub_modifier: Modifier::default(),
                    });
                }
            }
        };
        render_parser(&parser_pair.stdout, left_inner_area);
        render_parser(&parser_pair.stderr, right_inner_area);
    }
}

fn main() {
    let args = Args::parse();

    if args.commands.is_empty() {
        eprintln!("Requires at least 1 command");
        return;
    }

    let (tx, rx) = std::sync::mpsc::channel();

    let mut ratatui_terminal = ratatui::init_with_options(ratatui::TerminalOptions {
        viewport: ratatui::Viewport::Inline(
            args.rows
                .unwrap_or_else(|| crossterm::terminal::size().unwrap().1),
        ),
    });

    let mut processes = Vec::with_capacity(args.commands.len());
    for (process_index, command) in args.commands.into_iter().enumerate() {
        let title = command.to_string_lossy().to_string();
        let tx = tx.clone();
        let thread = std::thread::spawn(move || {
            use std::process::Stdio;
            let mut process = DroppableProcess(
                std::process::Command::new("bash")
                    .arg("-c")
                    .arg(command)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
            let mut stdout = process.0.stdout.take().unwrap();
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
        });

        processes.push(Process::new(title.into(), thread, args.split_layout));
    }
    drop(tx);

    let mut lowest_point = 0;

    macro_rules! render {
        () => {
            ratatui_terminal
                .draw(|frame| {
                    let area = frame.area();
                    let areas = Layout::new(
                        match args.overall_layout {
                            OverallLayout::Horizontal => Direction::Horizontal,
                            OverallLayout::Vertical => Direction::Vertical,
                        },
                        std::iter::repeat_n(Constraint::Fill(1), processes.len()),
                    )
                    .split(area);
                    for i in 0..processes.len() {
                        frame.render_widget(&mut processes[i], areas[i]);
                    }
                    lowest_point = lowest_point.max(area.height + area.y);
                })
                .unwrap();
        };
    }

    // This is done so that the OnceCells can be initialized with layout information
    let mut is_first = true;
    loop {
        if is_first {
            render!();
            is_first = false;
        }
        let mut should_render = false;
        let mut time_to_end = false;
        match rx.recv_timeout(std::time::Duration::from_secs(0)) {
            Ok(message) => {
                processes[message.process_index]
                    .get_mut(message.output_channel)
                    .process(&message.bytes);
                should_render = true;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                time_to_end = true;
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
                    crossterm::event::Event::Resize(_, _) => should_render = true,
                }
            } else {
                break;
            }
        }
        if should_render {
            render!();
        }
        if time_to_end {
            break;
        }
    }
    for process in processes {
        if process.thread.is_finished()
            && let Err(err) = process.thread.join()
        {
            eprintln!("Error: {err:?}");
        }
    }
    ratatui_terminal
        .set_cursor_position((0, lowest_point))
        .unwrap();
    ratatui::restore();
    println!();
}

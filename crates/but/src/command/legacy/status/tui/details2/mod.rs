use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fmt::Display,
    sync::{Arc, Mutex, mpsc::TryRecvError},
    time::Instant,
};

use anyhow::Context as _;
use bstr::{BString, ByteSlice as _};
use but_ctx::{Context, OnDemand};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Stylize as _},
    text::{Line, Span},
};
use syntect::{easy::HighlightLines, highlighting, parsing::SyntaxSet};

use crate::{
    CliId,
    command::legacy::status::tui::{Message, details::DetailsMessage},
    theme::Theme,
    utils::DebugAsType,
};

mod rendering;

const CHANNEL_SIZE: usize = 1024;

#[derive(Debug)]
pub struct Details2 {
    theme: &'static Theme,
    selection: Option<CliId>,
    lines: Vec<DetailsLine>,
    line_reader: ChannelLineReader,
    syntax_set: DebugAsType<OnDemand<SyntaxSet>>,
    syntax_theme: DebugAsType<OnDemand<highlighting::Theme>>,
    strings: Strings,
    selected_section: Option<SectionId>,
}

#[derive(Debug, Default)]
enum ChannelLineReader {
    #[default]
    NotStarted,
    Started {
        rx: std::sync::mpsc::Receiver<DetailsLine>,
        start: Instant,
    },
    Finished,
}

impl Details2 {
    pub fn new(theme: &'static Theme) -> Self {
        Self {
            theme,
            selection: None,
            lines: Default::default(),
            syntax_set: OnDemand::new(|| Ok(SyntaxSet::load_defaults_newlines())).into(),
            syntax_theme: OnDemand::new(|| theme.load_syntax_highlighting_theme()).into(),
            strings: Default::default(),
            selected_section: Default::default(),
            line_reader: Default::default(),
        }
    }

    pub fn is_finished_rendering(&self) -> bool {
        match &self.line_reader {
            ChannelLineReader::NotStarted | ChannelLineReader::Started { .. } => false,
            ChannelLineReader::Finished => true,
        }
    }

    pub fn is_polling_thread(&self) -> bool {
        match &self.line_reader {
            ChannelLineReader::NotStarted | ChannelLineReader::Finished => false,
            ChannelLineReader::Started { .. } => true,
        }
    }

    pub fn update(
        &mut self,
        ctx: &mut Context,
        new_selection: Option<&CliId>,
    ) -> anyhow::Result<bool> {
        let selection = match (self.selection.as_ref(), new_selection) {
            (None, None) => {
                // no selection
                self.lines.clear();
                self.line_reader = Default::default();

                return Ok(false);
            }
            (None, Some(new)) => {
                // selected something
                self.selection = Some(new.clone());

                self.lines.clear();
                self.line_reader = Default::default();

                new
            }
            (Some(_), None) => {
                // deselected
                self.selection = None;
                self.lines.clear();
                self.line_reader = Default::default();

                return Ok(true);
            }
            (Some(old), Some(new)) => {
                if old == new {
                    // selection didn't change
                    // we might have to poll the channel so dont return
                    old
                } else {
                    // selected something new
                    self.selection = Some(new.clone());
                    self.lines.clear();
                    self.line_reader = Default::default();
                    new
                }
            }
        };

        match selection {
            CliId::Commit {
                commit_id: commit, ..
            } => match &mut self.line_reader {
                ChannelLineReader::NotStarted => {
                    tracing::debug!("spawning thread");
                    let (tx, rx) = std::sync::mpsc::sync_channel(CHANNEL_SIZE);
                    self.line_reader = ChannelLineReader::Started {
                        rx,
                        start: Instant::now(),
                    };
                    let mut line_writer = ChannelLineWriter { tx };
                    let strings = self.strings.clone();
                    let theme = self.theme;
                    let commit = *commit;
                    let ctx = ctx.to_sync();
                    std::thread::spawn(move || {
                        let ctx = ctx.into_thread_local();
                        let mut id_gen = IdGen::new(strings);

                        let info = allocation_counter::measure(|| {
                            if let Err(err) = rendering::render_commit(
                                &ctx,
                                commit,
                                theme,
                                &mut id_gen,
                                &mut line_writer,
                            )
                            .context("failed rendering commit diff")
                                && err.downcast_ref::<SendErrorCode>().is_none()
                            {
                                tracing::error!("{err:#}");
                            }
                        });

                        {
                            fn format_thousands_with_dots(number: u64) -> String {
                                let digits = number.to_string();
                                let separator_count = digits.len().saturating_sub(1) / 3;
                                let mut formatted =
                                    String::with_capacity(digits.len() + separator_count);
                                for (index, digit) in digits.chars().enumerate() {
                                    if index > 0 && (digits.len() - index).is_multiple_of(3) {
                                        formatted.push('.');
                                    }
                                    formatted.push(digit);
                                }
                                formatted
                            }
                            tracing::debug!(
                                "allocations: {}",
                                format_thousands_with_dots(info.count_total)
                            );
                        }
                    });
                    Ok(true)
                }
                ChannelLineReader::Started { rx, start } => {
                    let mut n = CHANNEL_SIZE;
                    loop {
                        match rx.try_recv() {
                            Ok(line) => {
                                self.lines.push(line);
                            }
                            Err(err) => match err {
                                TryRecvError::Empty => break Ok(false),
                                TryRecvError::Disconnected => {
                                    let num_strings = self.strings.len();
                                    tracing::debug!(
                                        "finished reading from channel in {:?} ({} lines, {} strings)",
                                        start.elapsed(),
                                        self.lines.len(),
                                        num_strings,
                                    );
                                    self.line_reader = ChannelLineReader::Finished;
                                    break Ok(true);
                                }
                            },
                        }

                        n -= 1;
                        if n == 0 {
                            break Ok(true);
                        }
                    }
                }
                ChannelLineReader::Finished => Ok(false),
            },
            CliId::UncommittedHunkOrFile(..)
            | CliId::PathPrefix { .. }
            | CliId::CommittedFile { .. }
            | CliId::Branch { .. }
            | CliId::Uncommitted { .. }
            | CliId::Stack { .. } => {
                self.lines.clear();
                Ok(true)
            }
        }
    }

    pub fn render(&self, _help_shown: bool, _has_focus: bool, area: Rect, frame: &mut Frame) {
        let syntax_set = self.syntax_set.get().unwrap();
        let syntax_theme = self.syntax_theme.get().unwrap();

        for (n, line) in self.lines.iter().enumerate() {
            let n = n as u16;

            if n >= area.height {
                break;
            }
            let y = area.y + n;
            let line_area = Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            };

            match line {
                DetailsLine::Text { line, id } => {
                    frame.render_widget(line, line_area);
                }
                DetailsLine::TextToWrap { text, id } => {
                    frame.render_widget(&**text, line_area);
                }
                DetailsLine::Code {
                    highlighted_line,
                    line_numbers,
                    line_start_end,
                    diff,
                    path,
                    bg,
                    id,
                } => {
                    if highlighted_line.borrow().is_none() {
                        let syntax = {
                            let path = path.to_path_lossy();
                            path.extension()
                                .and_then(|ext| syntax_set.find_syntax_by_extension(ext.to_str()?))
                                .or_else(|| {
                                    path.file_name().and_then(|file_name| {
                                        syntax_set.find_syntax_by_extension(file_name.to_str()?)
                                    })
                                })
                                .unwrap_or_else(|| syntax_set.find_syntax_plain_text())
                        };

                        // TODO: should this be cached?
                        let mut highlight_lines = HighlightLines::new(syntax, &syntax_theme);

                        let (start, end) = *line_start_end;
                        let line = diff[start..end].to_str_lossy();
                        let line = line.strip_suffix('\n').unwrap_or(&line);
                        let line = line.strip_suffix('\r').unwrap_or(line);

                        *highlighted_line.borrow_mut() =
                            Some(Line::from_iter(line_numbers.clone().into_iter().chain(
                                syntax_highlight(line, *bg, &mut highlight_lines, &syntax_set),
                            )));
                    }

                    frame.render_widget(highlighted_line.borrow().as_ref().unwrap(), line_area);
                }
                DetailsLine::EmptyLine => {
                    frame.render_widget("", line_area);
                }
            }
        }
    }

    #[allow(clippy::ptr_arg)]
    pub fn try_handle_message(
        &mut self,
        msg: DetailsMessage,
        _viewport: Rect,
        _messages: &mut Vec<Message>,
    ) -> anyhow::Result<()> {
        match msg {
            DetailsMessage::ScrollUp(_) => {}
            DetailsMessage::ScrollDown(_) => {}
            DetailsMessage::SelectNextSection => {}
            DetailsMessage::SelectPrevSection => {}
            DetailsMessage::Deselect => {}
            DetailsMessage::SelectFirstSection => {}
            DetailsMessage::CopyCurrentHunk => {}
            DetailsMessage::GotoTop => {}
            DetailsMessage::GotoBottom => {}
            DetailsMessage::StartRub => {}
            DetailsMessage::Unlock => {}
        }

        Ok(())
    }
}

trait LineWriter {
    fn push(&mut self, line: DetailsLine) -> anyhow::Result<()>;

    fn push_text(&mut self, id: SectionId, line: Line<'static>) -> anyhow::Result<()> {
        self.push(DetailsLine::Text { id, line })
    }

    fn push_empty_line(&mut self) -> anyhow::Result<()> {
        self.push(DetailsLine::EmptyLine)
    }

    fn push_text_to_wrap(&mut self, id: SectionId, text: String) -> anyhow::Result<()> {
        self.push(DetailsLine::TextToWrap { id, text })
    }

    fn push_code(
        &mut self,
        id: SectionId,
        line_numbers: [Span<'static>; 6],
        line_start_end: (usize, usize),
        diff: Arc<BString>,
        bg: Option<Color>,
        path: Arc<BString>,
    ) -> anyhow::Result<()> {
        self.push(DetailsLine::Code {
            id,
            highlighted_line: RefCell::new(None),
            line_numbers,
            line_start_end,
            diff,
            path,
            bg,
        })
    }
}

struct ChannelLineWriter {
    tx: std::sync::mpsc::SyncSender<DetailsLine>,
}

impl LineWriter for ChannelLineWriter {
    fn push(&mut self, line: DetailsLine) -> anyhow::Result<()> {
        let result = self.tx.send(line);
        if result.is_ok() {
            Ok(())
        } else {
            Err(anyhow::Error::new(SendErrorCode))
        }
    }
}

/// Error code used to identify errors cause the receiving half of channel having been dropped.
///
/// This is expected and will happen if we start rendering the diff of one item but then change our
/// selection.
#[derive(Debug)]
struct SendErrorCode;

impl Display for SendErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("send failed, receiver disconnected")
    }
}

impl std::error::Error for SendErrorCode {}

#[derive(Debug, Default, Clone)]
struct Strings {
    storage: Arc<Mutex<HashSet<&'static str>>>,
    u32s: Arc<Mutex<HashMap<u32, &'static str>>>,
}

impl Strings {
    fn len(&self) -> usize {
        let Self { storage, u32s } = self;
        storage.lock().unwrap().len() + u32s.lock().unwrap().len()
    }

    fn get(&self, s: String) -> &'static str {
        let mut storage = self.storage.lock().unwrap();
        if let Some(value) = storage.get(&*s) {
            return value;
        }
        let static_s = s.leak();
        storage.insert(static_s);
        static_s
    }

    fn get_u32(&self, n: u32) -> &'static str {
        let mut storage = self.u32s.lock().unwrap();
        if let Some(value) = storage.get(&n) {
            return value;
        }
        let static_s = n.to_string().leak();
        storage.insert(n, static_s);
        static_s
    }
}

#[derive(Debug)]
struct IdGen<'a> {
    pub strings: Strings,
    scope: &'static str,
    _marker: std::marker::PhantomData<&'a mut ()>,
}

impl IdGen<'_> {
    fn new(strings: Strings) -> Self {
        IdGen {
            strings,
            scope: "details",
            _marker: std::marker::PhantomData,
        }
    }

    fn new_id(&mut self, id: impl Display) -> SectionId {
        SectionId(self.strings.get(format!("{}/{}", self.scope, id)))
    }

    fn scoped(&mut self, scope: impl Display) -> IdGen<'_> {
        let scope = self.strings.get(format!("{}/{}", self.scope, scope));
        IdGen {
            strings: self.strings.clone(),
            scope,
            _marker: std::marker::PhantomData,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct SectionId(&'static str);

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
enum DetailsLine {
    Text {
        id: SectionId,
        line: Line<'static>,
    },
    TextToWrap {
        id: SectionId,
        text: String,
    },
    Code {
        id: SectionId,
        line_numbers: [Span<'static>; 6],
        // indexes into `diff` where the line starts and ends, including any line terminators
        line_start_end: (usize, usize),
        // the whole diff this line is part of
        //
        // we share the diff and store indexes to get the line to avoid allocating each line
        diff: Arc<BString>,
        path: Arc<BString>,
        // HACK: only when drawing this line to the screen do we syntax highlight it and cache the
        // result directly here. We dont have a mutable reference in `Details2::render` so have to
        // cheat with a `RefCell`.
        highlighted_line: RefCell<Option<Line<'static>>>,
        bg: Option<Color>,
    },
    EmptyLine,
}

fn syntax_highlight(
    code: &str,
    bg: Option<Color>,
    highlight_lines: &mut HighlightLines<'_>,
    syntax_set: &SyntaxSet,
) -> Vec<Span<'static>> {
    let Ok(ranges) = highlight_lines.highlight_line(code, syntax_set) else {
        return Vec::from([Span::raw(code.to_owned())]);
    };

    ranges
        .iter()
        .map(|(style, text)| {
            let color = Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);
            Span::raw(text.to_string()).fg(color)
        })
        .map(move |span| {
            if let Some(background) = bg {
                span.bg(background)
            } else {
                span
            }
        })
        .collect::<Vec<_>>()
}

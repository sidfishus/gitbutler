use std::{cell::RefCell, sync::Arc};

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
    command::legacy::status::tui::{
        Message, details::DetailsMessage, details2::rendering::LineBuffer,
    },
    theme::Theme,
    utils::DebugAsType,
};

mod rendering;

#[derive(Debug)]
pub struct Details2 {
    theme: &'static Theme,
    selection: Option<CliId>,
    lines: Option<Vec<DetailsLine>>,
    syntax_set: DebugAsType<OnDemand<SyntaxSet>>,
    syntax_theme: DebugAsType<OnDemand<highlighting::Theme>>,
}

impl Details2 {
    pub fn new(theme: &'static Theme) -> Self {
        Self {
            theme,
            selection: None,
            lines: Default::default(),
            syntax_set: OnDemand::new(|| Ok(SyntaxSet::load_defaults_newlines())).into(),
            syntax_theme: OnDemand::new(|| theme.load_syntax_highlighting_theme()).into(),
        }
    }

    pub fn update(&mut self, ctx: &mut Context, selection: Option<&CliId>) -> anyhow::Result<bool> {
        let selection = match (self.selection.as_ref(), selection) {
            (None, None) => {
                return Ok(false);
            }
            (None, Some(new)) => {
                self.selection = Some(new.clone());
                new
            }
            (Some(_), None) => {
                return Ok(false);
            }
            (Some(old), Some(new)) => {
                if old == new {
                    return Ok(false);
                } else {
                    self.selection = Some(new.clone());
                    new
                }
            }
        };

        tracing::info!("update");

        match selection {
            CliId::Commit {
                commit_id: commit, ..
            } => {
                let mut out = LineBuffer::default();
                rendering::render_commit(ctx, *commit, self.theme, &mut out)?;
                self.lines = Some(out.buf);
            }
            CliId::UncommittedHunkOrFile(..)
            | CliId::PathPrefix { .. }
            | CliId::CommittedFile { .. }
            | CliId::Branch { .. }
            | CliId::Uncommitted { .. }
            | CliId::Stack { .. } => {
                self.lines = None;
            }
        };

        Ok(true)
    }

    pub fn render(&self, _help_shown: bool, _has_focus: bool, area: Rect, frame: &mut Frame) {
        let syntax_set = self.syntax_set.get().unwrap();
        let syntax_theme = self.syntax_theme.get().unwrap();

        let Some(lines) = &self.lines else {
            frame.render_widget("", area);
            return;
        };

        for (n, line) in lines.iter().enumerate() {
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
                DetailsLine::Text { line } => {
                    frame.render_widget(line, line_area);
                }
                DetailsLine::TextToWrap { text } => {
                    frame.render_widget(&**text, line_area);
                }
                DetailsLine::RawCode {
                    highlighted_line,
                    line_numbers,
                    code,
                    path,
                    bg,
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

                        *highlighted_line.borrow_mut() =
                            Some(Line::from_iter(line_numbers.clone().into_iter().chain(
                                syntax_highlight(code, *bg, &mut highlight_lines, &syntax_set),
                            )));
                    }

                    frame.render_widget(highlighted_line.borrow().as_ref().unwrap(), line_area);
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
    fn push_text(&mut self, line: Line<'static>);

    fn push_text_to_wrap(&mut self, text: String);

    fn push_raw_code(
        &mut self,
        line_numbers: Vec<Span<'static>>,
        code: String,
        bg: Option<Color>,
        path: Arc<BString>,
    );
}

#[derive(Debug)]
enum DetailsLine {
    Text {
        line: Line<'static>,
    },
    TextToWrap {
        text: String,
    },
    RawCode {
        highlighted_line: RefCell<Option<Line<'static>>>,
        line_numbers: Vec<Span<'static>>,
        code: String,
        path: Arc<BString>,
        bg: Option<Color>,
    },
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

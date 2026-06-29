use std::{
    cell::RefCell,
    iter::{once, repeat_n},
    sync::Arc,
};

use bstr::{BStr, BString, ByteSlice as _};
use but_core::{
    UnifiedPatch,
    ui::{TreeChange, TreeStatus},
    unified_diff::DiffHunk,
};
use but_ctx::{Context, OnDemand};
use gix::{ObjectId, actor::Signature};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Stylize as _},
    text::{Line, Span},
};
use syntect::{easy::HighlightLines, highlighting, parsing::SyntaxSet};
use unicode_width::UnicodeWidthStr as _;

use crate::{
    CliId,
    command::legacy::status::tui::{Message, details::DetailsMessage},
    theme::Theme,
    utils::DebugAsType,
};

#[derive(Debug)]
pub struct Details2 {
    theme: &'static Theme,
    selection: Option<CliId>,
    lines: Vec<RenderLine>,
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

        let lines = match selection {
            CliId::Commit {
                commit_id: commit, ..
            } => {
                let mut out = RenderOut::default();
                render_commit(ctx, *commit, self.theme, &mut out)?;
                out.buf
            }
            CliId::UncommittedHunkOrFile(..)
            | CliId::PathPrefix { .. }
            | CliId::CommittedFile { .. }
            | CliId::Branch { .. }
            | CliId::Uncommitted { .. }
            | CliId::Stack { .. } => {
                return Ok(false);
            }
        };

        self.lines = lines;

        Ok(true)
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
                RenderLine::Text { line } => {
                    frame.render_widget(line, line_area);
                }
                RenderLine::TextToWrap { text } => {
                    frame.render_widget(&**text, line_area);
                }
                RenderLine::RawCode {
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

#[derive(Default)]
struct RenderOut {
    buf: Vec<RenderLine>,
}

impl RenderOut {
    fn push_text(&mut self, line: Line<'static>) {
        self.buf.push(RenderLine::Text { line });
    }

    fn push_text_to_wrap(&mut self, text: String) {
        self.buf.push(RenderLine::TextToWrap { text });
    }

    fn push_raw_code(
        &mut self,
        line_numbers: Vec<Span<'static>>,
        code: String,
        bg: Option<Color>,
        path: Arc<BString>,
    ) {
        self.buf.push(RenderLine::RawCode {
            highlighted_line: RefCell::new(None),
            line_numbers,
            code,
            path,
            bg,
        });
    }
}

#[derive(Debug)]
enum RenderLine {
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

fn render_commit(
    ctx: &Context,
    commit: ObjectId,
    theme: &'static Theme,
    out: &mut RenderOut,
) -> anyhow::Result<()> {
    let commit_details =
        but_api::diff::commit_details(ctx, commit, but_api::diff::ComputeLineStats::No)?;

    out.push_text(Line::from_iter([
        Span::raw(format!("{:<11}", "Commit ID:")),
        Span::styled(commit.to_hex().to_string(), theme.commit_id),
    ]));

    out.push_text(Line::from_iter(
        once(Span::raw(format!("{:<11}", "Author:")))
            .chain(render_signature(&commit_details.commit.author, theme)),
    ));

    out.push_text(Line::from_iter(
        once(Span::raw(format!("{:<11}", "Committer:")))
            .chain(render_signature(&commit_details.commit.committer, theme)),
    ));

    // out.push_end_of_section();

    out.push_text("".into());

    let message = commit_details.commit.message.to_string();
    if !message.is_empty() {
        out.push_text_to_wrap(message);
        // out.push_end_of_section();
        out.push_text("".into());
    }

    let tree_changes = commit_details
        .diff_with_first_parent
        .iter()
        .map(|change| TreeChange::from(change.clone()))
        .collect::<Vec<_>>();

    build_tree_changes(ctx, &tree_changes, theme, out);

    Ok(())
}

fn build_tree_changes(
    ctx: &Context,
    tree_changes: &[TreeChange],
    theme: &'static Theme,
    out: &mut RenderOut,
) {
    for tree_change in tree_changes {
        let path = Arc::new(tree_change.path_bytes.clone());
        if let Some(patch) = but_api::diff::tree_change_diffs(ctx, tree_change.clone())
            .ok()
            .flatten()
        {
            match patch {
                UnifiedPatch::Patch {
                    hunks,
                    is_result_of_binary_to_text_conversion,
                    lines_added: _,
                    lines_removed: _,
                } => {
                    let mut first_hunk = true;
                    for diff_hunk in hunks {
                        if std::mem::take(&mut first_hunk) {
                            render_hunk_path_header(
                                tree_change.path.as_ref(),
                                Some(ShortIdOrTreeStatus::TreeStatus(&tree_change.status)),
                                out,
                                theme,
                            );
                        }

                        build_unified_patch(
                            &path,
                            diff_hunk,
                            is_result_of_binary_to_text_conversion,
                            theme,
                            out,
                        );

                        // out.push_end_of_section();
                    }
                }
                UnifiedPatch::Binary => {
                    render_hunk_path_header(
                        tree_change.path.as_ref(),
                        Some(ShortIdOrTreeStatus::TreeStatus(&tree_change.status)),
                        out,
                        theme,
                    );

                    out.push_text("Binary file - no diff available".into());

                    // out.push_end_of_section();
                }
                UnifiedPatch::TooLarge { size_in_bytes } => {
                    render_hunk_path_header(
                        tree_change.path.as_ref(),
                        Some(ShortIdOrTreeStatus::TreeStatus(&tree_change.status)),
                        out,
                        theme,
                    );

                    out.push_text(
                        format!("File too large ({size_in_bytes} bytes) - no diff available")
                            .into(),
                    );

                    // out.push_end_of_section();
                }
            }
        }
    }
}

fn render_signature(
    sig: &Signature,
    theme: &'static Theme,
) -> impl IntoIterator<Item = Span<'static>> {
    [
        Span::styled(sig.name.to_string(), theme.user),
        Span::raw(" <"),
        Span::styled(sig.email.to_string(), theme.user),
        Span::raw(">"),
        Span::raw(" ("),
        Span::styled(
            sig.time.format_or_unix(gix::date::time::format::DEFAULT),
            theme.time,
        ),
        Span::raw(")"),
    ]
    .into_iter()
}

enum ShortIdOrTreeStatus<'a> {
    #[expect(dead_code)]
    ShortId(&'a str),
    TreeStatus(&'a TreeStatus),
}

fn render_hunk_path_header(
    path: &BStr,
    status: Option<ShortIdOrTreeStatus<'_>>,
    out: &mut RenderOut,
    theme: &'static Theme,
) {
    let status = status.map(|id_or_status| match id_or_status {
        ShortIdOrTreeStatus::ShortId(id) => Span::styled(id.to_owned(), theme.cli_id),
        ShortIdOrTreeStatus::TreeStatus(status) => change_status(status, theme),
    });
    let path = path.to_string();
    let path_line = Line::from_iter(
        [Span::raw(" ")]
            .into_iter()
            .chain(
                status
                    .into_iter()
                    .flat_map(|status| [status, Span::raw(" ")]),
            )
            .chain([Span::raw(path)]),
    );
    bordered_line_top_right_bottom(path_line, out, theme);
    out.push_text("".into());
}

fn change_status(status: &TreeStatus, theme: &'static Theme) -> Span<'static> {
    match status {
        TreeStatus::Addition { .. } => Span::styled("added", theme.addition),
        TreeStatus::Deletion { .. } => Span::styled("deleted", theme.deletion),
        TreeStatus::Modification { .. } => Span::styled("modified", theme.modification),
        TreeStatus::Rename { .. } => Span::styled("renamed", theme.renaming),
    }
}

fn bordered_line_top_right_bottom(
    mut text: Line<'static>,
    out: &mut RenderOut,
    theme: &'static Theme,
) {
    let width_including_padding = text.width() + 1;

    out.push_text(
        Line::from_iter(repeat_n("─", width_including_padding).chain(once("╮")))
            .style(theme.border),
    );

    text.spans
        .extend([Span::raw(" "), Span::styled("│", theme.border)]);
    out.push_text(text);

    out.push_text(
        Line::from_iter(repeat_n("─", width_including_padding).chain(once("╯")))
            .style(theme.border),
    );
}

fn build_unified_patch(
    path: &Arc<BString>,
    hunk: DiffHunk,
    is_result_of_binary_to_text_conversion: bool,
    theme: &'static Theme,
    out: &mut RenderOut,
) {
    let DiffHunk {
        old_start,
        new_start,
        diff,
        old_lines: _,
        new_lines: _,
    } = hunk;

    if is_result_of_binary_to_text_conversion {
        out.push_text("(diff generated from binary-to-text conversion)".into());
    }

    if let Some(headers) = diff.lines().next() {
        out.push_text(Span::styled(headers.to_str_lossy().to_string(), theme.hint).into());

        out.push_text(
            Line::from_iter(repeat_n("─", headers.to_str_lossy().width())).style(theme.border),
        );
    }

    let (old_width, new_width) = {
        let mut old_line = old_start;
        let mut new_line = new_start;
        for line in diff.lines().skip(1) {
            if line.starts_with(b"+") {
                new_line += 1;
            } else if line.starts_with(b"-") {
                old_line += 1;
            } else {
                old_line += 1;
                new_line += 1;
            }
        }
        (num_digits(old_line), num_digits(new_line))
    };

    let mut old_line_num = old_start;
    let mut new_line_num = new_start;

    for line in diff.lines().skip(1) {
        let (line_numbers, code, bg) = if let Some(rest) = line.strip_prefix(b"+") {
            let code = rest.to_str_lossy().to_string();
            let line_numbers = Vec::from_iter([
                Span::raw(" ".repeat(old_width as _)),
                Span::styled(" ┊ ", theme.border),
                Span::raw(" ".repeat((new_width - num_digits(new_line_num)) as _)),
                Span::raw(new_line_num.to_string()).style(theme.addition),
                Span::styled(" │ ", theme.border),
                Span::raw("+").style(theme.addition_rich),
            ]);
            new_line_num += 1;
            (line_numbers, code, theme.addition_rich.bg)
        } else if let Some(rest) = line.strip_prefix(b"-") {
            let code = rest.to_str_lossy().to_string();
            let line_numbers = Vec::from_iter([
                Span::raw(" ".repeat((old_width - num_digits(old_line_num)) as _)),
                Span::raw(old_line_num.to_string()).style(theme.deletion),
                Span::styled(" ┊ ", theme.border),
                Span::raw(" ".repeat(new_width as _)),
                Span::styled(" │ ", theme.border),
                Span::raw("-").style(theme.deletion_rich),
            ]);
            old_line_num += 1;
            (line_numbers, code, theme.deletion_rich.bg)
        } else {
            let line = line.strip_prefix(b" ").unwrap_or(line);
            let code = line.to_str_lossy().to_string();
            let line_numbers = Vec::from_iter([
                Span::raw(" ".repeat((old_width - num_digits(old_line_num)) as _)),
                Span::styled(old_line_num.to_string(), theme.hint),
                Span::styled(" ┊ ", theme.border),
                Span::raw(" ".repeat((new_width - num_digits(new_line_num)) as _)),
                Span::styled(new_line_num.to_string(), theme.hint),
                Span::styled(" │  ", theme.border),
            ]);
            old_line_num += 1;
            new_line_num += 1;
            (line_numbers, code, None)
        };

        out.push_raw_code(line_numbers, code, bg, Arc::clone(path));
    }
}

fn num_digits(n: u32) -> u32 {
    if n == 0 { 1 } else { n.ilog10() + 1 }
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

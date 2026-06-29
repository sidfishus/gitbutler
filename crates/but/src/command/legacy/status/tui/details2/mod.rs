#![allow(warnings)]

use std::iter::{once, repeat_n};

use bstr::{BStr, ByteSlice as _};
use but_core::{
    UnifiedPatch,
    ui::{TreeChange, TreeStatus},
    unified_diff::DiffHunk,
};
use but_ctx::Context;
use gix::{ObjectId, actor::Signature};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::ListItem,
};
use unicode_width::UnicodeWidthStr as _;

use crate::{
    CliId,
    command::legacy::status::tui::{Message, details::DetailsMessage},
    theme::Theme,
};

#[derive(Debug)]
pub struct Details2 {
    theme: &'static Theme,
    selection: Option<CliId>,
    lines: Vec<RenderLine>,
}

impl Details2 {
    pub fn new(theme: &'static Theme) -> Self {
        Self {
            theme,
            selection: None,
            lines: Default::default(),
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
                render_commit(ctx, *commit, &mut out, self.theme)?;
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

    pub fn render(&self, help_shown: bool, has_focus: bool, area: Rect, frame: &mut Frame) {
        let mut n = 0;
        for line in &self.lines {
            if matches!(line, RenderLine::EndOfSection) {
                continue;
            }

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
                RenderLine::RawCode { raw_line, .. } => {
                    frame.render_widget(raw_line, line_area);
                }
                RenderLine::EndOfSection => unreachable!(),
            }

            n += 1;
        }
    }

    #[allow(clippy::ptr_arg)]
    pub fn try_handle_message(
        &mut self,
        msg: DetailsMessage,
        viewport: Rect,
        messages: &mut Vec<Message>,
    ) -> anyhow::Result<()> {
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
        raw_line: Line<'static>,
        line_numbers: Vec<Span<'static>>,
        code: String,
    ) {
        self.buf.push(RenderLine::RawCode {
            raw_line,
            line_numbers,
            code,
        });
    }

    fn push_end_of_section(&mut self) {
        self.buf.push(RenderLine::EndOfSection);
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
        raw_line: Line<'static>,
        line_numbers: Vec<Span<'static>>,
        code: String,
    },
    EndOfSection,
}

fn render_commit(
    ctx: &Context,
    commit: ObjectId,
    out: &mut RenderOut,
    theme: &'static Theme,
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

    out.push_end_of_section();

    out.push_text("".into());

    let message = commit_details.commit.message.to_string();
    if !message.is_empty() {
        out.push_text_to_wrap(message);
        out.push_end_of_section();
        out.push_text("".into());
    }

    let tree_changes = commit_details
        .diff_with_first_parent
        .iter()
        .map(|change| TreeChange::from(change.clone()))
        .collect::<Vec<_>>();

    build_tree_changes(ctx, &tree_changes, out, theme);

    Ok(())
}

fn build_tree_changes(
    ctx: &Context,
    tree_changes: &[TreeChange],
    out: &mut RenderOut,
    theme: &'static Theme,
) {
    for tree_change in tree_changes {
        if let Some(patch) = but_api::diff::tree_change_diffs(ctx, tree_change.clone())
            .ok()
            .flatten()
        {
            match patch {
                UnifiedPatch::Patch {
                    hunks,
                    is_result_of_binary_to_text_conversion,
                    lines_added,
                    lines_removed,
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
                            tree_change.path.as_ref(),
                            diff_hunk,
                            is_result_of_binary_to_text_conversion,
                            theme,
                            out,
                        );

                        out.push_end_of_section();
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

                    out.push_end_of_section();
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

                    out.push_end_of_section();
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
    path: &BStr,
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
        let (line_numbers, code) = if let Some(rest) = line.strip_prefix(b"+") {
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
            (line_numbers, code)
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
            (line_numbers, code)
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
            (line_numbers, code)
        };

        let raw_line = Line::from_iter(
            line_numbers
                .clone()
                .into_iter()
                .chain([Span::raw(code.clone())]),
        );

        out.push_raw_code(raw_line, line_numbers, code);
    }
}

fn num_digits(n: u32) -> u32 {
    if n == 0 { 1 } else { n.ilog10() + 1 }
}

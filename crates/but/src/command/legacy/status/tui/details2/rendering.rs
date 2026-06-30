use std::{
    iter::{once, repeat_n},
    sync::Arc,
};

use bstr::{BStr, BString, ByteSlice as _};
use but_core::{
    UnifiedPatch,
    ui::{TreeChange, TreeStatus},
    unified_diff::DiffHunk,
};
use but_ctx::Context;
use gix::{ObjectId, actor::Signature};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr as _;

use crate::{
    command::legacy::status::tui::details2::{IdGen, LineWriter, SectionId},
    theme::Theme,
};

pub fn render_commit(
    ctx: &Context,
    commit: ObjectId,
    theme: &'static Theme,
    id_gen: &mut IdGen<'_>,
    out: &mut impl LineWriter,
) -> anyhow::Result<()> {
    let commit_details =
        but_api::diff::commit_details(ctx, commit, but_api::diff::ComputeLineStats::No)?;

    let mut id_gen = id_gen.scoped(commit);

    let commit_header_id = id_gen.new_id("header");
    out.push_text(
        commit_header_id,
        Line::from_iter([
            Span::raw(format!("{:<11}", "Commit ID:")),
            Span::styled(commit.to_hex().to_string(), theme.commit_id),
        ]),
    )?;
    out.push_text(
        commit_header_id,
        Line::from_iter(
            once(Span::raw(format!("{:<11}", "Author:")))
                .chain(render_signature(&commit_details.commit.author, theme)),
        ),
    )?;
    out.push_text(
        commit_header_id,
        Line::from_iter(
            once(Span::raw(format!("{:<11}", "Committer:")))
                .chain(render_signature(&commit_details.commit.committer, theme)),
        ),
    )?;

    out.push_empty_line()?;

    let message_id = id_gen.new_id("message");
    let message = commit_details.commit.message.to_string();
    if !message.is_empty() {
        out.push_text_to_wrap(message_id, message)?;
        out.push_empty_line()?;
    }

    let tree_changes = commit_details
        .diff_with_first_parent
        .iter()
        .map(|change| TreeChange::from(change.clone()))
        .collect::<Vec<_>>();

    build_tree_changes(ctx, &tree_changes, theme, &mut id_gen, out)?;

    Ok(())
}

fn build_tree_changes(
    ctx: &Context,
    tree_changes: &[TreeChange],
    theme: &'static Theme,
    id_gen: &mut IdGen<'_>,
    out: &mut impl LineWriter,
) -> anyhow::Result<()> {
    let mut id_gen = id_gen.scoped("tree_changes");

    for (i, tree_change) in tree_changes.iter().enumerate() {
        let mut id_gen = id_gen.scoped(i);

        let path = Arc::new(tree_change.path_bytes.clone());
        let Some(patch) = but_api::diff::tree_change_diffs(ctx, tree_change.clone())
            .ok()
            .flatten()
        else {
            continue;
        };

        match patch {
            UnifiedPatch::Patch {
                hunks,
                is_result_of_binary_to_text_conversion,
                lines_added: _,
                lines_removed: _,
            } => {
                let mut first_hunk = true;
                let mut id_gen = id_gen.scoped("hunks");
                for (j, hunk) in hunks.into_iter().enumerate() {
                    let hunk_id = id_gen.new_id(j);

                    if std::mem::take(&mut first_hunk) {
                        render_hunk_path_header(
                            hunk_id,
                            tree_change.path.as_ref(),
                            Some(ShortIdOrTreeStatus::TreeStatus(&tree_change.status)),
                            out,
                            theme,
                        )?;
                    }

                    build_unified_patch(
                        hunk_id,
                        &path,
                        hunk,
                        is_result_of_binary_to_text_conversion,
                        theme,
                        &mut id_gen,
                        out,
                    )?;

                    out.push_empty_line()?;
                }
            }
            UnifiedPatch::Binary => {
                let patch_id = id_gen.new_id("binary");

                render_hunk_path_header(
                    patch_id,
                    tree_change.path.as_ref(),
                    Some(ShortIdOrTreeStatus::TreeStatus(&tree_change.status)),
                    out,
                    theme,
                )?;

                out.push_text(patch_id, "Binary file - no diff available".into())?;

                out.push_empty_line()?;
            }
            UnifiedPatch::TooLarge { size_in_bytes } => {
                let patch_id = id_gen.new_id("too_large");

                render_hunk_path_header(
                    patch_id,
                    tree_change.path.as_ref(),
                    Some(ShortIdOrTreeStatus::TreeStatus(&tree_change.status)),
                    out,
                    theme,
                )?;

                out.push_text(
                    patch_id,
                    format!("File too large ({size_in_bytes} bytes) - no diff available").into(),
                )?;

                out.push_empty_line()?;
            }
        }
    }

    Ok(())
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
    id: SectionId,
    path: &BStr,
    status: Option<ShortIdOrTreeStatus<'_>>,
    out: &mut impl LineWriter,
    theme: &'static Theme,
) -> anyhow::Result<()> {
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
    bordered_line_top_right_bottom(id, path_line, out, theme)?;
    out.push_text(id, "".into())?;
    Ok(())
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
    id: SectionId,
    mut text: Line<'static>,
    out: &mut impl LineWriter,
    theme: &'static Theme,
) -> anyhow::Result<()> {
    let width_including_padding = text.width() + 1;

    out.push_text(
        id,
        Line::from_iter(repeat_n("─", width_including_padding).chain(once("╮")))
            .style(theme.border),
    )?;

    text.spans
        .extend([Span::raw(" "), Span::styled("│", theme.border)]);
    out.push_text(id, text)?;

    out.push_text(
        id,
        Line::from_iter(repeat_n("─", width_including_padding).chain(once("╯")))
            .style(theme.border),
    )?;

    Ok(())
}

fn build_unified_patch(
    id: SectionId,
    path: &Arc<BString>,
    hunk: DiffHunk,
    is_result_of_binary_to_text_conversion: bool,
    theme: &'static Theme,
    id_gen: &mut IdGen<'_>,
    out: &mut impl LineWriter,
) -> anyhow::Result<()> {
    let DiffHunk {
        old_start,
        new_start,
        diff,
        old_lines: _,
        new_lines: _,
    } = hunk;

    if is_result_of_binary_to_text_conversion {
        out.push_text(id, "(diff generated from binary-to-text conversion)".into())?;
    }

    if let Some(headers) = diff.lines().next() {
        out.push_text(
            id,
            Span::styled(headers.to_str_lossy().to_string(), theme.hint).into(),
        )?;

        out.push_text(
            id,
            Line::from_iter(repeat_n("─", headers.to_str_lossy().width())).style(theme.border),
        )?;
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
                Span::raw(id_gen.strings.get_u32(new_line_num)).style(theme.addition),
                Span::styled(" │ ", theme.border),
                Span::raw("+").style(theme.addition_rich),
            ]);
            new_line_num += 1;
            (line_numbers, code, theme.addition_rich.bg)
        } else if let Some(rest) = line.strip_prefix(b"-") {
            let code = rest.to_str_lossy().to_string();
            let line_numbers = Vec::from_iter([
                Span::raw(" ".repeat((old_width - num_digits(old_line_num)) as _)),
                Span::raw(id_gen.strings.get_u32(old_line_num)).style(theme.deletion),
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
                Span::styled(id_gen.strings.get_u32(old_line_num), theme.hint),
                Span::styled(" ┊ ", theme.border),
                Span::raw(" ".repeat((new_width - num_digits(new_line_num)) as _)),
                Span::styled(id_gen.strings.get_u32(new_line_num), theme.hint),
                Span::styled(" │  ", theme.border),
            ]);
            old_line_num += 1;
            new_line_num += 1;
            (line_numbers, code, None)
        };

        out.push_raw_code(id, line_numbers, code, bg, Arc::clone(path))?;
    }

    Ok(())
}

fn num_digits(n: u32) -> u32 {
    if n == 0 { 1 } else { n.ilog10() + 1 }
}

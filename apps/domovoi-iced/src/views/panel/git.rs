//! Git side panel: repository status, branch switcher & creation, pull/commit/push/PR
//! actions, changed files, worktrees management, and project scripts runner.

use domovoi_core::Event;
use iced::widget::{Column, Space, button, column, container, row, text};
use iced::{Alignment, Element, Length};

use crate::app::{App, Message};
use crate::ui::fonts;
use crate::ui::icons::Icon;
use crate::ui::tokens::*;
use crate::ui::widgets as w;

fn core(e: Event) -> Message {
    Message::Core(Box::new(e))
}

pub fn view(app: &App) -> Element<'_, Message> {
    let git = &app.vm.panel.git;

    if git.loading && !git.is_repo {
        return w::empty_state(Icon::Loader, "Checking repository…", "Reading git status from the server.", None);
    }
    if !git.is_repo {
        return w::empty_state(
            Icon::GitBranch,
            "Not a Git repository",
            "Initialize git in this project to use branches, worktrees, and PR helpers.",
            Some(w::small_button("Refresh", w::secondary_style, Some(core(Event::RefreshGit))).into()),
        );
    }

    let mut col = Column::new().spacing(M).padding(M);

    // Branch header & actions
    let branch_label = git.branch.as_deref().unwrap_or("(detached)");
    let mut header = row![
        w::themed_icon(Icon::GitBranch, ICON, |c| c.accent),
        text(branch_label).size(TEXT_UI).font(fonts::MONO_BOLD).width(Length::Fill),
    ]
    .spacing(S)
    .align_y(Alignment::Center);

    if git.ahead > 0 {
        header = header.push(w::badge_capsule(format!("↑{}", git.ahead), Some(|c| c.info)));
    }
    if git.behind > 0 {
        header = header.push(w::badge_capsule(format!("↓{}", git.behind), Some(|c| c.warning)));
    }
    if git.has_upstream {
        header = header.push(w::small_button("Pull", w::secondary_style, (!git.busy).then(|| core(Event::GitPull))));
    }
    header = header.push(w::icon_button(Icon::RefreshCw, "Refresh git status", Some(core(Event::RefreshGit))));
    col = col.push(w::card(header).width(Length::Fill));

    if let Some(err) = &git.error {
        col = col.push(container(w::colored(err.clone(), TEXT_SMALL, |c| c.danger)).padding(S).width(Length::Fill).style(w::banner_style(|c| c.danger)));
    }

    if !git.progress.is_empty() {
        let mut pcol = Column::new().spacing(XXS);
        for step in &git.progress {
            pcol = pcol.push(w::muted(step, TEXT_CAPTION));
        }
        col = col.push(container(pcol).padding(S).width(Length::Fill).style(w::surface));
    }

    // Active Pull Request badge if present
    if let Some(pr) = &git.pull_request {
        col = col.push(
            w::card(
                row![
                    w::themed_icon(Icon::GitPullRequest, ICON, |c| c.info),
                    column![
                        text(format!("#{} {}", pr.number, pr.title)).size(TEXT_SMALL).font(fonts::SEMIBOLD),
                        w::muted(format!("{} · {}", pr.state, pr.checks), TEXT_CAPTION),
                    ]
                    .width(Length::Fill),
                    w::small_button("Open", w::secondary_style, Some(Message::OpenUrl(pr.url.clone()))),
                ]
                .spacing(S)
                .align_y(Alignment::Center),
            )
            .width(Length::Fill),
        );
    }

    // Commit & Push form
    let mut commit_col = Column::new().spacing(S).push(
        row![
            w::heading("Changes"),
            Space::new().width(Length::Fill),
            w::colored(format!("+{}", git.insertions), TEXT_CAPTION, |c| c.success),
            w::colored(format!("−{}", git.deletions), TEXT_CAPTION, |c| c.danger),
        ]
        .spacing(S)
        .align_y(Alignment::Center),
    );
    if git.changed_files.is_empty() {
        commit_col = commit_col.push(w::faint("Working tree clean.", TEXT_SMALL));
    } else {
        for f in git.changed_files.iter().take(20) {
            commit_col = commit_col.push(
                row![
                    text(&f.path).size(TEXT_CAPTION).font(fonts::MONO).width(Length::Fill),
                    w::colored(format!("+{}", f.insertions), TEXT_CAPTION, |c| c.success),
                    w::colored(format!("−{}", f.deletions), TEXT_CAPTION, |c| c.danger),
                ]
                .spacing(XS),
            );
        }
        commit_col = commit_col
            .push(
                w::input("Commit message (leave blank to auto-generate)…", &app.git_commit_msg)
                    .on_input(Message::GitCommitMsgChanged),
            )
            .push(
                row![
                    w::small_button(
                        "Commit",
                        w::secondary_style,
                        (!git.busy).then(|| core(Event::GitAction {
                            action: "commit".into(),
                            message: app.git_commit_msg.clone(),
                        })),
                    ),
                    w::small_button(
                        "Commit & Push",
                        w::primary_style,
                        (!git.busy && git.has_remote).then(|| core(Event::GitAction {
                            action: "commit_push".into(),
                            message: app.git_commit_msg.clone(),
                        })),
                    ),
                    w::small_button(
                        "Commit, Push & PR",
                        w::secondary_style,
                        (!git.busy && git.has_remote).then(|| core(Event::GitAction {
                            action: "commit_push_pr".into(),
                            message: app.git_commit_msg.clone(),
                        })),
                    ),
                ]
                .spacing(XS),
            );
    }
    if git.has_remote && git.ahead > 0 {
        commit_col = commit_col.push(
            row![
                w::small_button(
                    "Push",
                    w::secondary_style,
                    (!git.busy).then(|| core(Event::GitAction { action: "push".into(), message: String::new() })),
                ),
                w::small_button(
                    "Create PR",
                    w::secondary_style,
                    (!git.busy).then(|| core(Event::GitAction { action: "create_pr".into(), message: String::new() })),
                ),
            ]
            .spacing(XS),
        );
    }
    col = col.push(w::card(commit_col).width(Length::Fill));

    // Branches
    let mut br_col = Column::new().spacing(XS).push(w::heading("Branches"));
    br_col = br_col.push(
        row![
            w::input("New branch name…", &app.git_branch_name)
                .on_input(Message::GitBranchNameChanged)
                .width(Length::Fill),
            w::small_button(
                "Create",
                w::secondary_style,
                (!app.git_branch_name.trim().is_empty() && !git.busy).then(|| core(Event::CreateBranch {
                    name: app.git_branch_name.trim().to_owned(),
                })),
            ),
        ]
        .spacing(XS),
    );
    for br in git.branches.iter().take(12) {
        let current = git.branch.as_deref() == Some(br.as_str());
        br_col = br_col.push(
            w::list_row(
                row![
                    w::themed_icon(Icon::GitBranch, ICON_XS, move |c| if current { c.accent } else { c.muted }),
                    text(br).size(TEXT_SMALL).font(fonts::MONO).width(Length::Fill),
                    if current { w::badge_capsule("Current", Some(|c| c.accent)) } else { Space::new().into() },
                ]
                .spacing(S)
                .align_y(Alignment::Center),
                current,
                false,
            )
            .on_press_maybe((!current && !git.busy).then(|| core(Event::SwitchBranch { name: br.clone() }))),
        );
    }
    col = col.push(w::card(br_col).width(Length::Fill));

    // Worktrees
    let mut wt_col = Column::new().spacing(XS).push(
        row![
            w::heading("Worktrees"),
            Space::new().width(Length::Fill),
            w::small_button(
                if app.worktree_draft.open { "Cancel" } else { "+ Worktree" },
                w::ghost_style,
                Some(Message::ToggleWorktreeForm),
            ),
        ]
        .align_y(Alignment::Center),
    );
    if app.worktree_draft.open {
        wt_col = wt_col.push(
            column![
                w::input("Base branch (e.g. main)", &app.worktree_draft.base).on_input(Message::WorktreeBaseChanged),
                w::input("New branch (optional)", &app.worktree_draft.branch)
                    .on_input(Message::WorktreeBranchChanged)
                    .on_submit(Message::SubmitCreateWorktree),
                w::small_button("Create Worktree", w::primary_style, Some(Message::SubmitCreateWorktree)),
            ]
            .spacing(XS),
        );
    }
    if git.worktrees.is_empty() {
        wt_col = wt_col.push(w::faint("No worktrees.", TEXT_SMALL));
    } else {
        for wt in &git.worktrees {
            wt_col = wt_col.push(
                row![
                    column![
                        text(&wt.branch).size(TEXT_SMALL).font(fonts::MONO_BOLD),
                        w::faint(&wt.path, TEXT_CAPTION),
                    ]
                    .width(Length::Fill),
                    if wt.current {
                        w::badge_capsule("Active", Some(|c| c.success))
                    } else {
                        w::icon_button(
                            Icon::Trash2,
                            "Remove worktree",
                            Some(core(Event::RemoveWorktree { path: wt.path.clone(), force: false })),
                        )
                    },
                ]
                .spacing(S)
                .align_y(Alignment::Center),
            );
        }
    }
    col = col.push(w::card(wt_col).width(Length::Fill));

    // Project scripts
    if !git.scripts.is_empty() {
        let mut sc_col = Column::new().spacing(XS).push(w::heading("Project scripts"));
        for sc in &git.scripts {
            sc_col = sc_col.push(
                button(
                    row![
                        w::themed_icon(Icon::Play, ICON_SMALL, |c| c.accent),
                        column![
                            text(&sc.name).size(TEXT_SMALL).font(fonts::MEDIUM),
                            w::faint(&sc.command, TEXT_CAPTION).font(fonts::MONO),
                        ]
                        .width(Length::Fill),
                    ]
                    .spacing(S)
                    .align_y(Alignment::Center),
                )
                .width(Length::Fill)
                .padding([TIGHT, S])
                .style(w::ghost_style)
                .on_press(core(Event::RunScript { id: sc.id.clone() })),
            );
        }
        col = col.push(w::card(sc_col).width(Length::Fill));
    }

    w::scroll(col).height(Length::Fill).into()
}

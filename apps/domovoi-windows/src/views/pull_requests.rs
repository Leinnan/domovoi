//! The Pull Requests page: the open ones of the server's projects on the
//! left, and the one chosen on the right with its checks and description,
//! what can be done with it, a review and a comment.

use std::rc::Rc;

use domovoi_core::Event;
use domovoi_core::view::{PullRequestDetailView, PullRequestRowView, PullRequestsView};
use domovoi_desktop_host::palette;
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, ComboBox, FontWeight, Grid, GridLength, HorizontalAlignment, Orientation, ScrollBarVisibility, ScrollViewer, StackPanel,
    TextBlock, TextBox, TextTrimming, TextWrapping, Thickness, TooltipExt as _, VerticalAlignment, View,
};

use crate::core_link::CoreLink;
use crate::views::markdown;
use crate::views::transcript::Draw;
use crate::{theme, widgets};

const METHODS: [(&str, &str); 3] = [("squash", "Squash and merge"), ("merge", "Merge commit"), ("rebase", "Rebase and merge")];
const VERDICTS: [(&str, &str); 3] = [("comment", "Comment"), ("approve", "Approve"), ("request-changes", "Request changes")];

/// What is typed and chosen on the page, until it is sent.
#[derive(Default)]
pub struct State {
    /// Empty: squash, as the other clients start.
    merge_method: String,
    /// Empty: a comment.
    verdict: String,
    review: String,
    comment: String,
}

/// What is done on the page that is not one event for the core.
#[derive(Clone, Debug)]
pub enum Act {
    Method(String),
    Verdict(String),
    Review(Rc<str>),
    Comment(Rc<str>),
    /// Merge, send the review, or post the comment of the pull request with this key.
    Merge(String),
    SubmitReview(String),
    SubmitComment(String),
    /// A page for the browser.
    Open(String),
}

impl State {
    fn method(&self) -> &str {
        if self.merge_method.is_empty() { "squash" } else { &self.merge_method }
    }

    fn verdict(&self) -> &str {
        if self.verdict.is_empty() { "comment" } else { &self.verdict }
    }

    /// Does `act`: what it tells the core, and a page to open if it is one.
    pub fn apply(&mut self, act: Act) -> (Option<Event>, Option<String>) {
        match act {
            Act::Method(method) => self.merge_method = method,
            Act::Verdict(verdict) => self.verdict = verdict,
            Act::Review(text) => self.review = text.to_string(),
            Act::Comment(text) => self.comment = text.to_string(),
            Act::Merge(key) => return (Some(Event::PullRequestAction { key, action: "merge".into(), merge_method: self.method().to_owned() }), None),
            Act::SubmitReview(key) => {
                let verdict = self.verdict().to_owned();
                return (Some(Event::ReviewPullRequest { key, verdict, body: std::mem::take(&mut self.review).trim().to_owned() }), None);
            }
            Act::SubmitComment(key) => {
                let body = std::mem::take(&mut self.comment).trim().to_owned();
                return ((!body.is_empty()).then_some(Event::CommentOnPullRequest { key, body }), None);
            }
            Act::Open(url) => return (None, Some(url)),
        }
        (None, None)
    }
}

fn caption(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_size(theme::CAPTION).opacity(0.65)
}

fn card(lines: Vec<View>) -> View {
    Border::new().padding(Thickness::uniform(12.0)).background(theme::code()).corner_radius(6.0).content(StackPanel::new().orientation(Orientation::Vertical).spacing(8.0).children(lines)).into()
}

fn tell(core: &CoreLink, label: &str, event: Event) -> Button {
    let core = core.clone();
    Button::new().content(TextBlock::new().text(label).font_size(theme::CAPTION + 1.0)).on_click(move || core.send(event.clone()))
}

fn does(on_act: &Callback<Act>, label: &str, act: Act) -> Button {
    let on_act = on_act.clone();
    Button::new().content(TextBlock::new().text(label).font_size(theme::CAPTION + 1.0)).on_click(move || on_act.call(act.clone()))
}

/// A state as a word in its colour: open, a draft, merged, closed.
fn state_word(state: &str, draft: bool, dark: bool) -> View {
    let colour = match state {
        "merged" => "violet",
        "closed" => "red",
        _ if draft => "gray",
        _ => "green",
    };
    TextBlock::new()
        .text(if draft { "Draft" } else { state })
        .font_size(theme::CAPTION)
        .font_weight(FontWeight::SEMI_BOLD)
        .foreground(theme::rgb(palette::project_rgb(colour, dark)))
        .vertical_alignment(VerticalAlignment::Center)
        .into()
}

pub fn page(core: &CoreLink, requests: &PullRequestsView, state: &State, draw: &Draw<'_>, on_act: &Callback<Act>) -> View {
    let top = Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto]).column_spacing(8.0).margin(Thickness::new(16.0, 12.0, 12.0, 8.0)).children((
        widgets::line_icon("git-pull-request", 18.0, draw.dark).vertical_alignment(VerticalAlignment::Center),
        TextBlock::new().grid_column(1).text("Pull Requests").font_size(theme::TITLE).font_weight(FontWeight::SEMI_BOLD),
        tell(core, if requests.loading { "Loading…" } else { "Refresh" }, Event::LoadPullRequests).grid_column(2).is_enabled(!requests.loading).vertical_alignment(VerticalAlignment::Center),
    ));

    let mut list: Vec<View> = Vec::new();
    if let Some(notice) = &requests.notice {
        list.push(card(vec![caption(notice).text_wrapping(TextWrapping::WrapWholeWords).into()]));
    }
    if let Some(error) = &requests.error {
        list.push(card(vec![TextBlock::new().text(error).font_size(theme::CAPTION + 1.0).foreground(theme::critical()).text_wrapping(TextWrapping::WrapWholeWords).into()]));
    }
    if requests.items.is_empty() {
        list.push(caption(if requests.loading { "Loading the pull requests…" } else { "No open pull requests." }).margin(Thickness::uniform(8.0)).into());
    }
    let chosen = requests.selected.as_ref().map(|d| d.key.as_str());
    list.extend(requests.items.iter().map(|request| row(core, request, chosen == Some(request.key.as_str()), draw.dark)));

    let detail: View = match &requests.selected {
        Some(selected) => self::detail(core, selected, requests.busy, state, draw, on_act),
        None => StackPanel::new()
            .orientation(Orientation::Vertical)
            .spacing(8.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .children(vec![
                widgets::line_icon("git-pull-request", 30.0, draw.dark).opacity(0.5).horizontal_alignment(HorizontalAlignment::Center).into(),
                TextBlock::new().text("Select a pull request").font_weight(FontWeight::SEMI_BOLD).horizontal_alignment(HorizontalAlignment::Center).into(),
                caption("Its checks and description show here, with a review and a thread to work on it.").into(),
            ])
            .into(),
    };
    Grid::new()
        .rows([GridLength::Auto, GridLength::STAR])
        .columns([GridLength::Pixel(340.0), GridLength::STAR])
        .children((
            Border::new().grid_column_span(2).content(top),
            ScrollViewer::new()
                .grid_row(1)
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
                .content(StackPanel::new().orientation(Orientation::Vertical).spacing(2.0).margin(Thickness::new(8.0, 0.0, 12.0, 8.0)).children(list)),
            Border::new().grid_row(1).grid_column(1).border_brush(theme::stroke()).border_thickness(Thickness::new(1.0, 0.0, 0.0, 0.0)).content(detail),
        ))
        .into()
}

/// A pull request in the list.
fn row(core: &CoreLink, request: &PullRequestRowView, chosen: bool, dark: bool) -> View {
    let about = [request.repository.as_str(), request.author.as_str(), request.age.as_str()].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ");
    let mut lines: Vec<View> = vec![
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(8.0)
            .children((
                TextBlock::new().text(format!("#{} {}", request.number, request.title)).text_trimming(TextTrimming::CharacterEllipsis),
                Border::new().grid_column(1).content(state_word(&request.state, request.draft, dark)),
            ))
            .into(),
        caption(&about).text_trimming(TextTrimming::CharacterEllipsis).into(),
    ];
    if !request.head.is_empty() {
        lines.push(caption(&format!("{} → {}", request.head, request.base)).font_family(theme::MONO).text_trimming(TextTrimming::CharacterEllipsis).into());
    }
    if !request.checks.is_empty() {
        lines.push(caption(&request.checks).into());
    }
    let (core, key) = (core.clone(), request.key.clone());
    let button = Button::new()
        .style(ButtonStyle::Subtle)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .horizontal_content_alignment(HorizontalAlignment::Stretch)
        .automation_name(format!("Pull request {} {}", request.number, request.title))
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(1.0).children(lines))
        .on_click(move || core.send(Event::SelectPullRequest { key: key.clone() }));
    if chosen { Border::new().corner_radius(4.0).background(theme::selected()).content(button).into() } else { button.into() }
}

/// The chosen pull request.
fn detail(core: &CoreLink, request: &PullRequestDetailView, busy: bool, state: &State, draw: &Draw<'_>, on_act: &Callback<Act>) -> View {
    let (added, removed) = (palette::project_rgb("green", draw.dark), palette::project_rgb("red", draw.dark));
    let key = &request.key;
    let mono = |text: String| TextBlock::new().text(text).font_family(theme::MONO).font_size(theme::CAPTION).vertical_alignment(VerticalAlignment::Center);
    let facts: Vec<View> = vec![
        state_word(&request.state, request.draft, draw.dark),
        mono(format!("{} → {}", request.head, request.base)).opacity(0.7).into(),
        mono(format!("+{}", request.additions)).foreground(theme::rgb(added)).into(),
        mono(format!("−{}", request.deletions)).foreground(theme::rgb(removed)).into(),
        caption(&format!("{} file{}", request.changed_files, if request.changed_files == 1 { "" } else { "s" })).vertical_alignment(VerticalAlignment::Center).into(),
    ];
    let mut lines: Vec<View> = vec![
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(12.0)
            .children((
                StackPanel::new().orientation(Orientation::Vertical).spacing(4.0).children(vec![
                    TextBlock::new().text(format!("#{} {}", request.number, request.title)).font_size(theme::TITLE - 2.0).font_weight(FontWeight::SEMI_BOLD).text_wrapping(TextWrapping::WrapWholeWords).into(),
                    StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(facts).into(),
                ]),
                StackPanel::new().grid_column(1).orientation(Orientation::Horizontal).spacing(4.0).vertical_alignment(VerticalAlignment::Top).children(vec![
                    tell(core, "Start Thread", Event::StartPullRequestThread { key: key.clone() }).style(ButtonStyle::Accent).is_enabled(!busy).tooltip("Checks the pull request out in a worktree and starts a thread there"),
                    does(on_act, "Browser", Act::Open(request.url.clone())).into(),
                ]),
            ))
            .into(),
    ];
    if request.loading {
        lines.push(caption("Loading its details…").into());
    }

    // What can be done with it as it stands.
    let action = |label: &str, action: &str| tell(core, label, Event::PullRequestAction { key: key.clone(), action: action.into(), merge_method: String::new() }).is_enabled(!busy);
    let mut actions: Vec<View> = Vec::new();
    if request.state == "open" {
        let chosen = METHODS.iter().position(|(value, _)| *value == state.method());
        let on_method = on_act.clone();
        actions.push(
            ComboBox::new()
                .automation_name("How to merge")
                .items_source(METHODS.iter().map(|(_, label)| (*label).to_owned()))
                .selected_index(chosen)
                .on_selection_changed(move |index: Option<usize>| {
                    if let Some((value, _)) = index.filter(|i| Some(*i) != chosen).and_then(|i| METHODS.get(i)) {
                        on_method.call(Act::Method((*value).to_owned()));
                    }
                })
                .into(),
        );
        actions.push(does(on_act, "Merge", Act::Merge(key.clone())).is_enabled(!busy && !request.draft).into());
        actions.push(if request.draft { action("Mark Ready", "ready").into() } else { action("Convert to Draft", "draft").into() });
        actions.push(action("Close", "close").into());
    } else if request.state == "closed" {
        actions.push(action("Reopen", "reopen").into());
    }
    if !actions.is_empty() {
        lines.push(StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children(actions).into());
    }

    if !request.checks.is_empty() {
        let mut checks: Vec<View> = vec![TextBlock::new().text("Checks").font_weight(FontWeight::SEMI_BOLD).into()];
        for check in &request.checks {
            let colour = match check.status.as_str() {
                "success" | "pass" | "completed" => "green",
                "failure" | "failed" | "error" => "red",
                _ => "amber",
            };
            let ink = theme::rgb(palette::project_rgb(colour, draw.dark));
            let mut cells: Vec<View> = vec![
                Border::new().width(7.0).height(7.0).corner_radius(3.5).background(ink.clone()).vertical_alignment(VerticalAlignment::Center).into(),
                TextBlock::new().grid_column(1).text(&check.name).font_size(theme::CAPTION + 1.0).text_trimming(TextTrimming::CharacterEllipsis).vertical_alignment(VerticalAlignment::Center).into(),
                TextBlock::new().grid_column(2).text(&check.status).font_size(theme::CAPTION).foreground(ink).vertical_alignment(VerticalAlignment::Center).into(),
            ];
            if !check.url.is_empty() {
                cells.push(does(on_act, "Open", Act::Open(check.url.clone())).grid_column(3).style(ButtonStyle::Subtle).into());
            }
            checks.push(Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto, GridLength::Auto]).column_spacing(8.0).children(cells).into());
        }
        lines.push(card(checks));
    }
    if !request.body.is_empty() {
        lines.push(card(markdown::blocks(&request.body, draw)));
    }

    let chosen = VERDICTS.iter().position(|(value, _)| *value == state.verdict());
    let (on_verdict, on_review, on_comment) = (on_act.clone(), on_act.clone(), on_act.clone());
    lines.push(card(vec![
        TextBlock::new().text("Review").font_weight(FontWeight::SEMI_BOLD).into(),
        ComboBox::new()
            .automation_name("Verdict")
            .horizontal_alignment(HorizontalAlignment::Left)
            .min_width(200.0)
            .items_source(VERDICTS.iter().map(|(_, label)| (*label).to_owned()))
            .selected_index(chosen)
            .on_selection_changed(move |index: Option<usize>| {
                if let Some((value, _)) = index.filter(|i| Some(*i) != chosen).and_then(|i| VERDICTS.get(i)) {
                    on_verdict.call(Act::Verdict((*value).to_owned()));
                }
            })
            .into(),
        TextBox::new()
            .automation_name("Review summary")
            .placeholder_text("What the review says")
            .accepts_return(true)
            .text_wrapping(TextWrapping::Wrap)
            .min_height(60.0)
            .text(&state.review)
            .on_text_changed(move |text| on_review.call(Act::Review(text)))
            .into(),
        does(on_act, "Submit Review", Act::SubmitReview(key.clone())).style(ButtonStyle::Accent).horizontal_alignment(HorizontalAlignment::Left).is_enabled(!busy).into(),
    ]));
    lines.push(card(vec![
        TextBlock::new().text("Comment").font_weight(FontWeight::SEMI_BOLD).into(),
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(6.0)
            .children((
                TextBox::new().automation_name("Comment on the pull request").placeholder_text("Leave a comment").text(&state.comment).on_text_changed(move |text| on_comment.call(Act::Comment(text))),
                does(on_act, "Post", Act::SubmitComment(key.clone())).grid_column(1).is_enabled(!busy && !state.comment.trim().is_empty()),
            ))
            .into(),
    ]));
    ScrollViewer::new()
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(12.0).margin(Thickness::new(16.0, 4.0, 20.0, 16.0)).children(lines))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_merge_goes_by_the_method_chosen_and_a_review_by_its_verdict() {
        let mut state = State::default();
        assert!(matches!(state.apply(Act::Merge("k".into())).0, Some(Event::PullRequestAction { action, merge_method, .. }) if action == "merge" && merge_method == "squash"));
        state.apply(Act::Method("rebase".into()));
        assert!(matches!(state.apply(Act::Merge("k".into())).0, Some(Event::PullRequestAction { merge_method, .. }) if merge_method == "rebase"));
        state.apply(Act::Review(" Looks right. ".into()));
        state.apply(Act::Verdict("approve".into()));
        assert!(matches!(state.apply(Act::SubmitReview("k".into())).0, Some(Event::ReviewPullRequest { verdict, body, .. }) if verdict == "approve" && body == "Looks right."));
        assert!(state.review.is_empty(), "what was sent is not kept");
    }

    #[test]
    fn an_empty_comment_is_not_posted_and_a_page_is_only_opened() {
        let mut state = State::default();
        assert!(state.apply(Act::SubmitComment("k".into())).0.is_none());
        state.apply(Act::Comment("thanks".into()));
        assert!(matches!(state.apply(Act::SubmitComment("k".into())).0, Some(Event::CommentOnPullRequest { body, .. }) if body == "thanks"));
        assert_eq!(state.apply(Act::Open("https://example.com".into())), (None, Some("https://example.com".to_owned())));
    }
}

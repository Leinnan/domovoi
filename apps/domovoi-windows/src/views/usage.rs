//! The Usage page: what the agents on this server used over a span of days,
//! by provider and by model, with each provider account's limits.

use domovoi_core::Event;
use domovoi_core::view::{UsageRowView, UsageSeriesPointView, UsageView};
use domovoi_desktop_host::palette;
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, FontWeight, Grid, GridLength, Orientation, ScrollBarVisibility, ScrollViewer, StackPanel, TextBlock,
    TextTrimming, TextWrapping, Thickness, TooltipExt as _, VerticalAlignment, View,
};

use crate::core_link::CoreLink;
use crate::{theme, widgets};

/// The spans offered, in days.
const SPANS: [(u32, &str); 4] = [(1, "Today"), (7, "7 days"), (30, "30 days"), (90, "90 days")];
/// How tall the chart's tallest bar is, and how many rows a table shows.
const CHART: f64 = 150.0;
const ROWS: usize = 40;

/// The colour a provider is drawn in, by its key.
fn provider_colour(key: &str) -> &'static str {
    let key = key.to_ascii_lowercase();
    if key.contains("codex") || key.contains("openai") {
        "emerald"
    } else if key.contains("antigravity") || key.contains("gemini") {
        "blue"
    } else if key.contains("claude") || key.contains("anthropic") {
        "orange"
    } else if key.contains("pi") {
        "violet"
    } else {
        "gray"
    }
}

/// `42%`, `<1%` for a share that rounds to nothing, or a dash with no tokens.
fn share(percent: u32, tokens: u64) -> String {
    match (percent, tokens) {
        (_, 0) => "—".into(),
        (0, _) => "<1%".into(),
        (percent, _) => format!("{percent}%"),
    }
}

/// A row's tokens by kind, for the tooltip on its total.
fn breakdown(row: &UsageRowView) -> String {
    let kinds = [("Input", &row.input), ("Cache read", &row.cached), ("Cache write", &row.cache_write), ("Output", &row.output), ("Reasoning", &row.reasoning)];
    kinds.into_iter().filter(|(_, value)| !value.is_empty() && value.as_str() != "0").map(|(kind, value)| format!("{kind} {value}")).collect::<Vec<_>>().join(" · ")
}

/// How tall each provider's part of a bar is, for a chart whose tallest bar holds `most` tokens.
fn bar_parts(point: &UsageSeriesPointView, most: u64) -> Vec<(&str, f64)> {
    let height = |tokens: u64| tokens as f64 / most.max(1) as f64 * CHART;
    if point.by_provider.is_empty() {
        return if point.total_tokens > 0 { vec![("", height(point.total_tokens))] } else { Vec::new() };
    }
    point.by_provider.iter().map(|part| (part.provider_key.as_str(), height(part.tokens))).filter(|(_, height)| *height > 0.5).collect()
}

fn caption(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_size(theme::CAPTION).opacity(0.65)
}

fn heading(text: &str) -> View {
    TextBlock::new().text(text).font_weight(FontWeight::SEMI_BOLD).into()
}

fn card(lines: Vec<View>) -> Border {
    Border::new().padding(Thickness::uniform(12.0)).background(theme::code()).corner_radius(6.0).content(StackPanel::new().orientation(Orientation::Vertical).spacing(6.0).children(lines))
}

/// A notice across the page.
fn banner(text: &str, colour: Option<&str>, dark: bool) -> View {
    let line = TextBlock::new().text(text).font_size(theme::CAPTION + 1.0).text_wrapping(TextWrapping::WrapWholeWords);
    match colour {
        Some(colour) => Border::new().padding(Thickness::uniform(10.0)).corner_radius(6.0).background(theme::tint(palette::project_rgb(colour, dark), dark)).content(line).into(),
        None => Border::new().padding(Thickness::uniform(10.0)).corner_radius(6.0).background(theme::code()).content(line.opacity(0.8)).into(),
    }
}

/// A figure with what it is above it and a note below.
fn figure(column: i32, label: &str, value: &str, note: &str, tip: &str) -> View {
    let card = card(vec![
        caption(label).into(),
        TextBlock::new().text(if value.is_empty() { "—" } else { value }).font_size(22.0).font_weight(FontWeight::BOLD).into(),
        caption(note).text_trimming(TextTrimming::CharacterEllipsis).into(),
    ])
    .grid_column(column);
    if tip.is_empty() { card.into() } else { card.tooltip(tip) }
}

pub fn page(core: &CoreLink, usage: &UsageView, dark: bool, on_open: &Callback<String>) -> View {
    let spans = SPANS.iter().map(|(days, label)| {
        let (core, days) = (core.clone(), *days);
        Button::new()
            .style(if usage.days.max(1) == days { ButtonStyle::Accent } else { ButtonStyle::Default })
            .content(TextBlock::new().text(*label).font_size(theme::CAPTION + 1.0))
            .on_click(move || core.send(Event::LoadUsage { days }))
            .into()
    });
    let refresh = {
        let core = core.clone();
        Button::new()
            .grid_column(3)
            .is_enabled(!usage.limits_refreshing)
            .vertical_alignment(VerticalAlignment::Center)
            .content(TextBlock::new().text(if usage.limits_refreshing { "Refreshing…" } else { "Refresh Limits" }).font_size(theme::CAPTION + 1.0))
            .on_click(move || core.send(Event::RefreshUsageLimits))
    };
    let top = Grid::new().columns([GridLength::Auto, GridLength::Auto, GridLength::STAR, GridLength::Auto]).column_spacing(10.0).margin(Thickness::new(16.0, 12.0, 12.0, 8.0)).children((
        widgets::line_icon("bar-chart", 18.0, dark).vertical_alignment(VerticalAlignment::Center),
        TextBlock::new().grid_column(1).text("Usage").font_size(theme::TITLE).font_weight(FontWeight::SEMI_BOLD),
        StackPanel::new().grid_column(2).orientation(Orientation::Horizontal).spacing(4.0).margin(Thickness::new(8.0, 0.0, 0.0, 0.0)).vertical_alignment(VerticalAlignment::Center).children(spans.collect::<Vec<View>>()),
        refresh,
    ));

    let mut lines: Vec<View> = Vec::new();
    if let Some(error) = &usage.error {
        lines.push(banner(error, Some("red"), dark));
    }
    if !usage.notice.is_empty() {
        lines.push(banner(&usage.notice, None, dark));
    }
    // Logs that could not be read whole: the totals may be short.
    lines.extend(usage.source_notices.iter().map(|notice| banner(notice, Some("amber"), dark)));
    if usage.loading && usage.series.is_empty() {
        lines.push(caption("Reading the usage…").into());
    }

    if !usage.providers.is_empty() {
        let chip = |label: String, key: &str, on: bool| -> View {
            let (core, provider) = (core.clone(), key.to_owned());
            Button::new()
                .style(if on { ButtonStyle::Accent } else { ButtonStyle::Default })
                .content(TextBlock::new().text(label).font_size(theme::CAPTION + 1.0))
                .on_click(move || core.send(Event::FilterUsageProvider { provider: provider.clone() }))
                .into()
        };
        let mut chips = vec![chip("All providers".into(), "", usage.selected_provider.is_empty())];
        chips.extend(usage.providers.iter().map(|p| chip(format!("{} · {} · {}", p.label, p.tokens_label, share(p.share_percent, p.tokens)), &p.key, usage.selected_provider == p.key)));
        lines.push(StackPanel::new().orientation(Orientation::Horizontal).spacing(4.0).children(chips).into());
        // Each provider's share of the span, as one bar.
        let shares: Vec<_> = usage.providers.iter().filter(|p| p.share_percent > 0).collect();
        if shares.len() > 1 {
            let parts = shares.iter().enumerate().map(|(column, p)| {
                Border::new()
                    .grid_column(column as i32)
                    .height(6.0)
                    .corner_radius(3.0)
                    .background(theme::rgb(palette::project_rgb(provider_colour(&p.key), dark)))
                    .tooltip(format!("{} {}", p.label, share(p.share_percent, p.tokens)))
            });
            lines.push(Grid::new().columns(shares.iter().map(|p| GridLength::Star(f64::from(p.share_percent))).collect::<Vec<_>>()).column_spacing(2.0).children(parts.collect::<Vec<View>>()).into());
        }
    }

    let activity = if usage.hourly || usage.active_days == 0 {
        format!("{} sessions · {} turns", usage.sessions, usage.turns)
    } else {
        format!("{} active day{} · {} sessions · {} turns", usage.active_days, if usage.active_days == 1 { "" } else { "s" }, usage.sessions, usage.turns)
    };
    let exact = if usage.total_tokens_exact.is_empty() { String::new() } else { format!("{} tokens", usage.total_tokens_exact) };
    let cache = if usage.cache_hit_label.is_empty() { String::new() } else { format!("{}% of the input came from the cache", usage.cache_hit_percent) };
    lines.push(
        Grid::new()
            .columns(vec![GridLength::STAR; 4])
            .column_spacing(10.0)
            .children(vec![
                figure(0, "Total tokens", &usage.total_tokens, &usage.range_label, &exact),
                figure(1, "Cost", &usage.total_cost, "Estimated", ""),
                figure(2, "Cache hit", &usage.cache_hit_label, "Input read from the cache", &cache),
                figure(3, if usage.hourly { "Per active hour" } else { "Per active day" }, &usage.daily_avg_tokens, &activity, &activity),
            ])
            .into(),
    );

    if !usage.series.is_empty() {
        lines.push(card(vec![heading(if usage.hourly { "Today by hour" } else { "Daily activity" }), chart(&usage.series, dark)]).into());
    }
    if !usage.token_mix.is_empty() {
        let slices = usage.token_mix.iter().enumerate().map(|(column, slice)| {
            card(vec![caption(&slice.label).into(), TextBlock::new().text(&slice.tokens_label).font_weight(FontWeight::SEMI_BOLD).into(), caption(&format!("{}%", slice.percent)).into()])
                .grid_column(column as i32)
                .tooltip(format!("{} tokens", slice.tokens))
        });
        lines.push(heading("Token composition"));
        lines.push(Grid::new().columns(vec![GridLength::STAR; usage.token_mix.len()]).column_spacing(10.0).children(slices.collect::<Vec<View>>()).into());
    }

    if !usage.limits.is_empty() {
        lines.push(heading("Provider account limits"));
        for account in &usage.limits {
            let mut head: Vec<View> = vec![TextBlock::new().text(&account.provider).font_weight(FontWeight::SEMI_BOLD).vertical_alignment(VerticalAlignment::Center).into()];
            if !account.checked.is_empty() {
                head.push(caption(&format!("Checked {}", account.checked)).grid_column(1).vertical_alignment(VerticalAlignment::Center).into());
            }
            if !account.external_url.is_empty() {
                let (on_open, url) = (on_open.clone(), account.external_url.clone());
                head.push(
                    Button::new()
                        .grid_column(2)
                        .style(ButtonStyle::Subtle)
                        .content(TextBlock::new().text(if account.external_label.is_empty() { "Open" } else { account.external_label.as_str() }).font_size(theme::CAPTION + 1.0))
                        .on_click(move || on_open.call(url.clone()))
                        .tooltip("Where the account shows its usage"),
                );
            }
            let mut rows: Vec<View> = vec![Grid::new().columns([GridLength::STAR, GridLength::Auto, GridLength::Auto]).column_spacing(10.0).children(head).into()];
            if !account.notice.is_empty() {
                rows.push(caption(&account.notice).text_wrapping(TextWrapping::WrapWholeWords).into());
            }
            rows.push(widgets::limit_windows(&account.windows, dark));
            lines.push(card(rows).into());
        }
    }

    if !usage.by_model.is_empty() {
        lines.push(table("By model", &usage.by_model, usage, true, dark));
    }
    if !usage.rows.is_empty() {
        lines.push(table("Activity log", &usage.rows, usage, false, dark));
    }
    Grid::new()
        .rows([GridLength::Auto, GridLength::STAR])
        .children((
            top,
            ScrollViewer::new()
                .grid_row(1)
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
                .content(StackPanel::new().orientation(Orientation::Vertical).spacing(12.0).max_width(1100.0).margin(Thickness::new(16.0, 4.0, 20.0, 16.0)).children(lines)),
        ))
        .into()
}

/// A bar for each hour or day, its providers stacked, with every so many of them named below.
fn chart(series: &[UsageSeriesPointView], dark: bool) -> View {
    let most = series.iter().map(|point| point.total_tokens).max().unwrap_or(1);
    let label_every = (series.len() / 8).max(1);
    let mut cells: Vec<View> = Vec::new();
    for (column, point) in series.iter().enumerate() {
        // From the top down: the first provider is at the bottom.
        let parts = bar_parts(point, most).into_iter().rev().map(|(key, height)| {
            let colour = if key.is_empty() { palette::project_rgb("blue", dark) } else { palette::project_rgb(provider_colour(key), dark) };
            Border::new().height(height).background(theme::rgb(colour)).into()
        });
        let tip = if point.cost.is_empty() { format!("{}: {}", point.detail_label, point.total_label) } else { format!("{}: {} ({})", point.detail_label, point.total_label, point.cost) };
        cells.push(
            Border::new()
                .grid_column(column as i32)
                .height(CHART)
                .max_width(28.0)
                // Something to point at where nothing was used.
                .background(theme::clear())
                .content(StackPanel::new().orientation(Orientation::Vertical).vertical_alignment(VerticalAlignment::Bottom).children(parts.collect::<Vec<View>>()))
                .tooltip(tip),
        );
        if column % label_every == 0 {
            cells.push(TextBlock::new().grid_row(1).grid_column(column as i32).grid_column_span(label_every as i32).text(&point.label).font_size(10.0).opacity(0.6).into());
        }
    }
    Grid::new().columns(vec![GridLength::STAR; series.len()]).rows([GridLength::Auto, GridLength::Auto]).column_spacing(2.0).row_spacing(4.0).children(cells).into()
}

/// The span by model, or its rows by day or hour.
fn table(title: &str, rows: &[UsageRowView], usage: &UsageView, by_model: bool, dark: bool) -> View {
    let widths = [3.0, 2.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0].map(GridLength::Star);
    let line = |cells: Vec<View>| -> View { Grid::new().columns(widths).column_spacing(8.0).children(cells).into() };
    let head = ["", "Provider", "Input", "Cached", "Output", "Total", "Share", "Cache hit", "Cost"];
    let first = if by_model {
        "Model"
    } else if usage.hourly {
        "Hour"
    } else {
        "When"
    };
    let mut lines: Vec<View> = vec![
        heading(title),
        line(head.iter().enumerate().map(|(column, name)| caption(if column == 0 { first } else { name }).grid_column(column as i32).into()).collect()),
    ];
    for row in rows.iter().take(ROWS) {
        let name = if by_model {
            row.model.clone()
        } else if usage.hourly && !row.hour_label.is_empty() {
            row.hour_label.clone()
        } else if !row.hour_label.is_empty() {
            format!("{} {}", row.day, row.hour_label)
        } else {
            row.day.clone()
        };
        let cell = |column: i32, text: &str| -> TextBlock {
            TextBlock::new().grid_column(column).text(if text.is_empty() { "—" } else { text }).font_size(theme::CAPTION + 1.0).text_trimming(TextTrimming::CharacterEllipsis)
        };
        let number = |column: i32, text: &str| cell(column, text).font_family(theme::MONO).opacity(0.75);
        let named = cell(0, &name);
        lines.push(line(vec![
            if by_model { named.font_family(theme::MONO).into() } else { named.into() },
            cell(1, &row.provider).foreground(theme::rgb(palette::project_rgb(provider_colour(&row.provider_key), dark))).into(),
            number(2, &row.input).into(),
            number(3, &row.cached).into(),
            number(4, &row.output).into(),
            cell(5, &row.total).font_family(theme::MONO).font_weight(FontWeight::SEMI_BOLD).tooltip(breakdown(row)),
            cell(6, &share(row.share_percent, row.total_tokens)).opacity(0.75).into(),
            cell(7, &format!("{}%", row.cache_hit_percent)).opacity(0.75).into(),
            cell(8, &row.cost).opacity(0.75).into(),
        ]));
    }
    if rows.len() > ROWS {
        lines.push(caption(&format!("… and {} more", rows.len() - ROWS)).into());
    }
    card(lines).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use domovoi_core::view::UsageProviderPointView;

    #[test]
    fn a_share_too_small_to_round_is_still_some_and_no_tokens_is_none() {
        assert_eq!(share(42, 1000), "42%");
        assert_eq!(share(0, 3), "<1%");
        assert_eq!(share(0, 0), "—");
    }

    #[test]
    fn a_bar_is_its_providers_stacked_to_scale() {
        let part = |key: &str, tokens| UsageProviderPointView { provider_key: key.into(), tokens, ..UsageProviderPointView::default() };
        let point = UsageSeriesPointView { total_tokens: 100, by_provider: vec![part("claude", 75), part("codex", 25), part("pi", 0)], ..UsageSeriesPointView::default() };
        assert_eq!(bar_parts(&point, 200), vec![("claude", CHART * 0.375), ("codex", CHART * 0.125)]);
        let whole = UsageSeriesPointView { total_tokens: 50, ..UsageSeriesPointView::default() };
        assert_eq!(bar_parts(&whole, 100), vec![("", CHART * 0.5)]);
        assert!(bar_parts(&UsageSeriesPointView::default(), 100).is_empty());
    }

    #[test]
    fn a_rows_tooltip_lists_only_what_it_used() {
        let row = UsageRowView { input: "1.2k".into(), cached: "0".into(), output: "300".into(), ..UsageRowView::default() };
        assert_eq!(breakdown(&row), "Input 1.2k · Output 300");
        assert_eq!(provider_colour("claudeAgent"), "orange");
        assert_eq!(provider_colour("something-else"), "gray");
    }
}

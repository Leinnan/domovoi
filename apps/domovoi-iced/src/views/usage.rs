//! Usage analytics page: span selector, provider filters, KPI cards, token mix,
//! custom Iced Canvas stacked bar chart, provider account limits, and tables.

use domovoi_core::Event;
use domovoi_core::view::{UsageRowView, UsageSeriesPointView, UsageView};
use iced::mouse;
use iced::widget::canvas::{self, Canvas, Geometry, Text as CanvasText};
use iced::widget::{Column, Space, button, column, container, row, text};
use iced::{Alignment, Background, Border, Element, Length, Point, Rectangle, Renderer, Size, Theme};

use crate::app::{App, Message};
use crate::ui::fonts;
use crate::ui::icons::Icon;
use crate::ui::theme::{agent_color, alpha, colors};
use crate::ui::tokens::*;
use crate::ui::widgets as w;

fn core(e: Event) -> Message {
    Message::Core(Box::new(e))
}

pub fn view(app: &App) -> Element<'_, Message> {
    let u = &app.vm.usage;

    let top = container(
        row![
            w::themed_icon(Icon::BarChart2, ICON, |c| c.accent),
            text("Usage").size(TEXT_BODY).font(fonts::SEMIBOLD),
            Space::new().width(S),
            w::segmented(
                &[(1u32, None, "Today"), (7, None, "7d"), (30, None, "30d"), (90, None, "90d")],
                u.days.max(1),
                |days| core(Event::LoadUsage { days }),
            ),
            Space::new().width(Length::Fill),
            w::small_button(
                if u.limits_refreshing { "Refreshing…" } else { "Refresh Limits" },
                w::secondary_style,
                (!u.limits_refreshing).then(|| core(Event::RefreshUsageLimits)),
            ),
        ]
        .spacing(S)
        .align_y(Alignment::Center),
    )
    .padding([S, L])
    .style(w::surface);

    let mut col = Column::new().spacing(L).padding(L).max_width(1100);

    if let Some(err) = &u.error {
        col = col.push(container(w::colored(err, TEXT_SMALL, |c| c.danger)).padding(M).width(Length::Fill).style(w::banner_style(|c| c.danger)));
    }
    if !u.notice.is_empty() {
        col = col.push(container(w::muted(&u.notice, TEXT_SMALL)).padding(S).width(Length::Fill).style(w::banner_style(|c| c.info)));
    }
    // Log sources that couldn't be read in full: the totals may be short.
    for note in &u.source_notices {
        col = col.push(
            container(
                row![w::themed_icon(Icon::TriangleAlert, ICON_SMALL, |c| c.warning), w::muted(note, TEXT_SMALL).width(Length::Fill)]
                    .spacing(S)
                    .align_y(Alignment::Center),
            )
            .padding(S)
            .width(Length::Fill)
            .style(w::banner_style(|c| c.warning)),
        );
    }

    // Provider filter chips
    if !u.providers.is_empty() {
        let mut chips = row![
            button(text("All providers").size(TEXT_SMALL))
                .padding([4.0, S])
                .style(w::chip_style(u.selected_provider.is_empty()))
                .on_press(core(Event::FilterUsageProvider { provider: String::new() })),
        ]
        .spacing(XS)
        .align_y(Alignment::Center);
        for p in &u.providers {
            let active = u.selected_provider == p.key;
            chips = chips.push(
                button(text(format!("{} · {} · {}", p.label, p.tokens_label, percent_label(p.share_percent, p.tokens))).size(TEXT_SMALL))
                    .padding([4.0, S])
                    .style(w::chip_style(active))
                    .on_press(core(Event::FilterUsageProvider { provider: p.key.clone() })),
            );
        }
        col = col.push(chips);
        // Each provider's share of the span, as one stacked bar.
        if u.providers.len() > 1 {
            let mut share = row![].spacing(XXS).height(6.0);
            for p in u.providers.iter().filter(|p| p.share_percent > 0) {
                let key = p.key.clone();
                share = share.push(
                    container(Space::new().width(Length::Fill).height(6.0))
                        .width(Length::FillPortion(p.share_percent.min(100) as u16))
                        .style(move |t: &Theme| container::Style {
                            background: Some(Background::Color(agent_color(&key, colors(t)))),
                            border: Border { radius: RADIUS_PILL.into(), ..Border::default() },
                            ..Default::default()
                        }),
                );
            }
            col = col.push(share);
        }
    }

    // KPI cards
    let activity = if u.hourly || u.active_days == 0 {
        format!("{} sessions · {} turns", u.sessions, u.turns)
    } else {
        format!("{} active {} · {} sessions · {} turns", u.active_days, if u.active_days == 1 { "day" } else { "days" }, u.sessions, u.turns)
    };
    let total_tip = if u.total_tokens_exact.is_empty() { String::new() } else { format!("{} tokens", u.total_tokens_exact) };
    let kpis = row![
        w::with_tooltip(kpi_card("Total tokens", &u.total_tokens, &u.range_label), &total_tip),
        kpi_card("Cost", if u.total_cost.is_empty() { "—" } else { u.total_cost.as_str() }, "Estimated"),
        kpi_card("Cache hit", if u.cache_hit_label.is_empty() { "—" } else { u.cache_hit_label.as_str() }, "Input read cache"),
        kpi_card(if u.hourly { "Per active hour" } else { "Per active day" }, &u.daily_avg_tokens, activity),
    ]
    .spacing(M);
    col = col.push(kpis);

    // Stacked activity bar chart
    if !u.series.is_empty() {
        let chart = Canvas::new(UsageChart { series: u.series.clone() }).width(Length::Fill).height(Length::Fixed(210.0));
        let heading = if u.hourly { "Today by hour" } else { "Daily activity" };
        col = col.push(w::card(column![w::heading(heading), chart].spacing(S)).width(Length::Fill));
    }

    // Token mix breakdown
    if !u.token_mix.is_empty() {
        let mut mix_row = row![].spacing(S);
        for sl in &u.token_mix {
            mix_row = mix_row.push(
                w::card(
                    column![
                        w::muted(&sl.label, TEXT_CAPTION),
                        text(&sl.tokens_label).size(TEXT_UI).font(fonts::SEMIBOLD),
                        w::faint(format!("{}%", sl.percent), TEXT_CAPTION),
                    ]
                    .spacing(XXS),
                )
                .width(Length::FillPortion(1)),
            );
        }
        col = col.push(column![w::heading("Token composition"), mix_row].spacing(XS));
    }

    // Account Limits
    if !u.limits.is_empty() {
        let mut lim_col = Column::new().spacing(S).push(w::heading("Provider account limits"));
        for pl in &u.limits {
            let mut card_col = Column::new().spacing(XS).push(
                row![
                    text(&pl.provider).size(TEXT_UI).font(fonts::SEMIBOLD).width(Length::Fill),
                    if !pl.checked.is_empty() { w::faint(format!("Checked {}", pl.checked), TEXT_CAPTION) } else { w::faint("", TEXT_CAPTION) },
                    if !pl.external_url.is_empty() {
                        w::small_button(
                            if pl.external_label.is_empty() { "Open" } else { pl.external_label.as_str() },
                            w::ghost_style,
                            Some(Message::OpenUrl(pl.external_url.clone())),
                        )
                        .into()
                    } else {
                        Element::from(Space::new())
                    },
                ]
                .spacing(S)
                .align_y(Alignment::Center),
            );
            if !pl.notice.is_empty() {
                card_col = card_col.push(w::muted(&pl.notice, TEXT_SMALL));
            }
            card_col = card_col.push(w::limit_windows(&pl.windows));
            lim_col = lim_col.push(w::card(card_col).width(Length::Fill));
        }
        col = col.push(lim_col);
    }

    // By-model table
    if !u.by_model.is_empty() {
        col = col.push(usage_table("By model", u, true));
    }
    if !u.rows.is_empty() {
        col = col.push(usage_table("Activity log", u, false));
    }

    column![top, w::divider(), w::scroll(container(col).center_x(Length::Fill)).height(Length::Fill)]
        .height(Length::Fill)
        .into()
}

/// `42%`, `<1%` for a share that rounds to nothing, or `—` with no tokens.
fn percent_label(percent: u32, tokens: u64) -> String {
    match (percent, tokens) {
        (_, 0) => "—".into(),
        (0, _) => "<1%".into(),
        (p, _) => format!("{p}%"),
    }
}

fn kpi_card<'a>(label: &'a str, value: &'a str, sub: impl Into<String>) -> Element<'a, Message> {
    let sub: String = sub.into();
    w::card(
        column![
            w::muted(label, TEXT_CAPTION),
            text(if value.is_empty() { "—" } else { value }).size(22.0).font(fonts::BOLD),
            w::faint(sub, TEXT_CAPTION),
        ]
        .spacing(XXS),
    )
    .width(Length::FillPortion(1))
    .into()
}

fn usage_table<'a>(title: &'a str, u: &'a UsageView, by_model: bool) -> Element<'a, Message> {
    let rows_src = if by_model { &u.by_model } else { &u.rows };
    let head = |label: &'a str, portion: u16| w::faint(label, TEXT_CAPTION).width(Length::FillPortion(portion));
    let mut tbl = Column::new().spacing(XS).push(w::heading(title));
    tbl = tbl.push(
        row![
            head(if by_model { "Model" } else if u.hourly { "Hour" } else { "When" }, 3),
            head("Provider", 2),
            head("Input", 1),
            head("Cached", 1),
            head("Output", 1),
            head("Total", 1),
            head("Share", 1),
            head("Cache hit", 1),
            head("Cost", 1),
        ]
        .spacing(S),
    );
    tbl = tbl.push(w::divider());
    for r in rows_src.iter().take(40) {
        let first = if by_model {
            r.model.clone()
        } else if u.hourly && !r.hour_label.is_empty() {
            r.hour_label.clone()
        } else if !r.hour_label.is_empty() {
            format!("{} {}", r.day, r.hour_label)
        } else {
            r.day.clone()
        };
        let num = |v: &'a str| w::muted(if v.is_empty() { "—" } else { v }, TEXT_SMALL).font(fonts::MONO).width(Length::FillPortion(1));
        tbl = tbl.push(
            row![
                text(first).size(TEXT_SMALL).font(if by_model { fonts::MONO } else { fonts::INTER }).width(Length::FillPortion(3)),
                w::muted(&r.provider, TEXT_SMALL).width(Length::FillPortion(2)),
                num(&r.input),
                num(&r.cached),
                num(&r.output),
                container(w::with_tooltip(text(&r.total).size(TEXT_SMALL).font(fonts::MONO_BOLD), &token_breakdown(r))).width(Length::FillPortion(1)),
                w::muted(percent_label(r.share_percent, r.total_tokens), TEXT_SMALL).width(Length::FillPortion(1)),
                w::muted(format!("{}%", r.cache_hit_percent), TEXT_SMALL).width(Length::FillPortion(1)),
                w::muted(if r.cost.is_empty() { "—" } else { r.cost.as_str() }, TEXT_SMALL).width(Length::FillPortion(1)),
            ]
            .spacing(S),
        );
    }
    w::card(tbl).width(Length::Fill).into()
}

/// A row's tokens by kind, for the tooltip on its total.
fn token_breakdown(r: &UsageRowView) -> String {
    [("Input", &r.input), ("Cache read", &r.cached), ("Cache write", &r.cache_write), ("Output", &r.output), ("Reasoning", &r.reasoning)]
        .into_iter()
        .filter(|(_, v)| !v.is_empty() && v.as_str() != "0")
        .map(|(k, v)| format!("{k} {v}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

struct UsageChart {
    series: Vec<UsageSeriesPointView>,
}

impl canvas::Program<Message> for UsageChart {
    type State = ();

    fn draw(&self, _state: &Self::State, renderer: &Renderer, theme: &Theme, bounds: Rectangle, cursor: mouse::Cursor) -> Vec<Geometry<Renderer>> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let h = colors(theme);
        if self.series.is_empty() {
            return vec![frame.into_geometry()];
        }
        let max_val = self.series.iter().map(|p| p.total_tokens).max().unwrap_or(1).max(1) as f32;
        let pad_x = 12.0;
        let pad_top = 24.0;
        let pad_bot = 24.0;
        let chart_w = (bounds.width - pad_x * 2.0).max(1.0);
        let chart_h = (bounds.height - pad_top - pad_bot).max(1.0);
        let n = self.series.len() as f32;
        let slot_w = chart_w / n;
        let bar_w = (slot_w * 0.72).clamp(3.0, 28.0);

        let hovered_idx = cursor.position_in(bounds).and_then(|pos| {
            if pos.x >= pad_x && pos.x <= bounds.width - pad_x {
                Some(((pos.x - pad_x) / slot_w).floor() as usize)
            } else {
                None
            }
        });

        let label_step = (self.series.len() / 8).max(1);
        for (i, pt) in self.series.iter().enumerate() {
            let x = pad_x + (i as f32) * slot_w + (slot_w - bar_w) * 0.5;
            let mut cur_y = pad_top + chart_h;
            if pt.by_provider.is_empty() && pt.total_tokens > 0 {
                let bh = (pt.total_tokens as f32 / max_val) * chart_h;
                cur_y -= bh;
                frame.fill_rectangle(Point::new(x, cur_y), Size::new(bar_w, bh), h.accent);
            } else {
                for prov in &pt.by_provider {
                    let bh = (prov.tokens as f32 / max_val) * chart_h;
                    if bh > 0.5 {
                        cur_y -= bh;
                        let c = agent_color(&prov.provider_key, h);
                        frame.fill_rectangle(Point::new(x, cur_y), Size::new(bar_w, bh), c);
                    }
                }
            }
            if hovered_idx == Some(i) {
                frame.fill_rectangle(Point::new(x - 2.0, pad_top), Size::new(bar_w + 4.0, chart_h), alpha(h.text, 0.06));
                let tip = if pt.cost.is_empty() {
                    format!("{}: {}", pt.detail_label, pt.total_label)
                } else {
                    format!("{}: {} ({})", pt.detail_label, pt.total_label, pt.cost)
                };
                frame.fill_text(CanvasText {
                    content: tip,
                    position: Point::new(pad_x, 4.0),
                    color: h.text,
                    size: iced::Pixels(11.0),
                    font: fonts::SEMIBOLD,
                    ..CanvasText::default()
                });
            }
            if i % label_step == 0 {
                frame.fill_text(CanvasText {
                    content: pt.label.clone(),
                    position: Point::new(x, pad_top + chart_h + 6.0),
                    color: h.faint,
                    size: iced::Pixels(10.0),
                    font: fonts::INTER,
                    ..CanvasText::default()
                });
            }
        }

        vec![frame.into_geometry()]
    }
}

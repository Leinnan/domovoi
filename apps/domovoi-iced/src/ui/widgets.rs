//! Reusable Hearth design-system widgets: buttons, surfaces, rows, pills,
//! modals, popovers, code spans, project icons, and status badges.

use std::collections::HashMap;

use domovoi_core::view::{CodeSpan, ProjectIconView};
use iced::widget::{Space, button, column, container, image, mouse_area, opaque, rich_text, row, rule, scrollable, span, text, text_editor, text_input, tooltip};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Shadow, Theme, Vector};

use super::fonts;
use super::icons::{Icon, project_symbol};
use super::theme::{Hearth, alpha, code_span_color, colors, project_color};
use super::tokens::*;

pub type Tone = fn(&Hearth) -> Color;

// ── Text ──────────────────────────────────────────────────────────────

pub fn muted<'a>(s: impl text::IntoFragment<'a>, size: f32) -> text::Text<'a> {
    text(s).size(size).style(|t: &Theme| text::Style { color: Some(colors(t).muted) })
}

pub fn faint<'a>(s: impl text::IntoFragment<'a>, size: f32) -> text::Text<'a> {
    text(s).size(size).style(|t: &Theme| text::Style { color: Some(colors(t).faint) })
}

pub fn colored<'a>(s: impl text::IntoFragment<'a>, size: f32, pick: Tone) -> text::Text<'a> {
    text(s).size(size).style(move |t: &Theme| text::Style { color: Some(pick(colors(t))) })
}

fn section_text<'a>(s: &str) -> text::Text<'a> {
    text(s.to_uppercase()).size(TEXT_CAPTION).font(fonts::SEMIBOLD).style(|t: &Theme| text::Style { color: Some(colors(t).muted) })
}

pub fn section<'a, M: 'a>(s: &str) -> Element<'a, M> {
    container(section_text(s)).padding(Padding { top: M, bottom: XS, left: S, right: S }).into()
}

pub fn heading<'a, M: 'a>(s: &str) -> Element<'a, M> {
    container(section_text(s)).padding(Padding { top: S, bottom: XS, left: 0.0, right: 0.0 }).into()
}

pub fn kbd<'a, M: 'a>(shortcut: &str) -> Element<'a, M> {
    let label = shortcut_label(shortcut);
    container(text(label).size(TEXT_CAPTION).font(fonts::MEDIUM).style(|t: &Theme| text::Style { color: Some(colors(t).muted) }))
        .padding([1.0, 5.0])
        .style(|t: &Theme| {
            let c = colors(t);
            container::Style {
                background: Some(Background::Color(c.raised)),
                border: Border { radius: RADIUS_SMALL.into(), width: 1.0, color: c.border },
                ..Default::default()
            }
        })
        .into()
}

pub fn shortcut_label(s: &str) -> String {
    let mac = cfg!(target_os = "macos");
    s.split('+')
        .map(|k| match k {
            "Mod" if mac => "⌘".to_owned(),
            "Mod" => "Ctrl".to_owned(),
            "Shift" if mac => "⇧".to_owned(),
            "Alt" if mac => "⌥".to_owned(),
            "Left" => "←".to_owned(),
            "Right" => "→".to_owned(),
            "Up" => "↑".to_owned(),
            "Down" => "↓".to_owned(),
            other => other.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(if mac { "" } else { "+" })
}

// ── Buttons ───────────────────────────────────────────────────────────

pub fn primary_style(t: &Theme, status: button::Status) -> button::Style {
    let c = colors(t);
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => c.accent_hover,
        button::Status::Disabled => alpha(c.accent, 0.35),
        button::Status::Active => c.accent,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: c.on_accent,
        border: Border { radius: RADIUS_MEDIUM.into(), ..Border::default() },
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn secondary_style(t: &Theme, status: button::Status) -> button::Style {
    let c = colors(t);
    let (bg, border) = match status {
        button::Status::Hovered | button::Status::Pressed => (c.surface, c.border_strong),
        _ => (c.raised, c.border),
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: if status == button::Status::Disabled { c.faint } else { c.text },
        border: Border { radius: RADIUS_MEDIUM.into(), width: 1.0, color: border },
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn danger_style(t: &Theme, status: button::Status) -> button::Style {
    let c = colors(t);
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => alpha(c.danger, 0.85),
        button::Status::Disabled => alpha(c.danger, 0.35),
        button::Status::Active => c.danger,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: if c.dark { c.sunken } else { c.raised },
        border: Border { radius: RADIUS_MEDIUM.into(), ..Border::default() },
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn ghost_style(t: &Theme, status: button::Status) -> button::Style {
    let c = colors(t);
    let bg = match status {
        button::Status::Hovered => Some(Background::Color(alpha(c.text, 0.07))),
        button::Status::Pressed => Some(Background::Color(alpha(c.text, 0.12))),
        _ => None,
    };
    button::Style {
        background: bg,
        text_color: if status == button::Status::Disabled { c.faint } else { c.text },
        border: Border { radius: RADIUS_MEDIUM.into(), ..Border::default() },
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn chip_style(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |t, status| {
        let c = colors(t);
        let (bg, border, txt) = if active {
            (c.accent_soft, alpha(c.accent, 0.45), c.accent)
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            (c.raised, c.border_strong, c.text)
        } else {
            (c.surface, c.border, c.muted)
        };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: if status == button::Status::Disabled { c.faint } else { txt },
            border: Border { radius: RADIUS_PILL.into(), width: 1.0, color: border },
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

pub fn row_style(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |t, status| {
        let c = colors(t);
        let bg = if selected {
            Some(Background::Color(c.accent_soft))
        } else {
            match status {
                button::Status::Hovered => Some(Background::Color(alpha(c.text, 0.06))),
                button::Status::Pressed => Some(Background::Color(alpha(c.text, 0.10))),
                _ => None,
            }
        };
        button::Style {
            background: bg,
            text_color: c.text,
            border: Border { radius: RADIUS_MEDIUM.into(), ..Border::default() },
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

pub fn segment_style(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |t, status| {
        let c = colors(t);
        button::Style {
            background: if active {
                Some(Background::Color(c.raised))
            } else if status == button::Status::Hovered {
                Some(Background::Color(alpha(c.text, 0.05)))
            } else {
                None
            },
            text_color: if active { c.text } else { c.muted },
            border: Border { radius: RADIUS_SMALL.into(), width: if active { 1.0 } else { 0.0 }, color: c.border },
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

pub fn label_button<'a, M: Clone + 'a>(
    label: impl text::IntoFragment<'a>,
    style: fn(&Theme, button::Status) -> button::Style,
    msg: Option<M>,
) -> button::Button<'a, M> {
    button(text(label).size(TEXT_UI).font(fonts::MEDIUM)).padding(BUTTON_PAD).style(style).on_press_maybe(msg)
}

pub fn small_button<'a, M: Clone + 'a>(
    label: impl text::IntoFragment<'a>,
    style: fn(&Theme, button::Status) -> button::Style,
    msg: Option<M>,
) -> button::Button<'a, M> {
    button(text(label).size(TEXT_SMALL).font(fonts::MEDIUM)).padding(SMALL_BUTTON_PAD).style(style).on_press_maybe(msg)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Primary,
    Secondary,
    Danger,
}

impl Kind {
    fn style(self) -> fn(&Theme, button::Status) -> button::Style {
        match self {
            Kind::Primary => primary_style,
            Kind::Secondary => secondary_style,
            Kind::Danger => danger_style,
        }
    }
}

pub fn icon_label_button<'a, M: Clone + 'a>(i: Icon, label: impl text::IntoFragment<'a>, kind: Kind, msg: Option<M>) -> button::Button<'a, M> {
    let style = kind.style();
    button(
        row![
            themed_icon(i, ICON_SMALL, move |c| match kind {
                Kind::Primary => c.on_accent,
                Kind::Danger => if c.dark { c.sunken } else { c.raised },
                _ => c.text,
            }),
            text(label).size(TEXT_UI).font(fonts::MEDIUM),
        ]
        .spacing(ICON_GAP)
        .align_y(Alignment::Center),
    )
    .padding(BUTTON_PAD)
    .style(style)
    .on_press_maybe(msg)
}

pub fn icon_button<'a, M: Clone + 'a>(i: Icon, tip: &str, msg: Option<M>) -> Element<'a, M> {
    icon_button_tinted(i, tip, msg, false)
}

pub fn icon_button_tinted<'a, M: Clone + 'a>(i: Icon, tip: &str, msg: Option<M>, active: bool) -> Element<'a, M> {
    let enabled = msg.is_some();
    let b = button(themed_icon(i, ICON, move |c| {
        if !enabled {
            c.faint
        } else if active {
            c.accent
        } else {
            c.muted
        }
    }))
    .padding(6.0)
    .style(ghost_style)
    .on_press_maybe(msg);
    with_tooltip(b, tip)
}

pub fn with_tooltip<'a, M: 'a>(content: impl Into<Element<'a, M>>, tip: &str) -> Element<'a, M> {
    if tip.is_empty() {
        return content.into();
    }
    tooltip(
        content,
        container(text(tip.to_owned()).size(TEXT_SMALL)).padding([4.0, 8.0]).style(|t: &Theme| {
            let c = colors(t);
            container::Style {
                background: Some(Background::Color(c.raised)),
                text_color: Some(c.text),
                border: Border { radius: RADIUS_SMALL.into(), width: 1.0, color: c.border },
                shadow: popover_shadow(),
                ..Default::default()
            }
        }),
        tooltip::Position::Bottom,
    )
    .gap(4.0)
    .into()
}

pub fn themed_icon<'a, M: 'a>(i: Icon, size: f32, pick: impl Fn(&Hearth) -> Color + 'a) -> Element<'a, M> {
    iced::widget::svg(i.handle())
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |t: &Theme, _| iced::widget::svg::Style { color: Some(pick(colors(t))) })
        .into()
}

// ── Surfaces ──────────────────────────────────────────────────────────

pub fn surface(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style { background: Some(Background::Color(c.surface)), text_color: Some(c.text), ..Default::default() }
}

pub fn sunken(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style { background: Some(Background::Color(c.sunken)), text_color: Some(c.text), ..Default::default() }
}

pub fn page(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style { background: Some(Background::Color(c.bg)), text_color: Some(c.text), ..Default::default() }
}

pub fn card_style(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(Background::Color(c.raised)),
        text_color: Some(c.text),
        border: Border { radius: RADIUS_CARD.into(), width: 1.0, color: c.border },
        ..Default::default()
    }
}

pub fn popover_style(t: &Theme) -> container::Style {
    container::Style { shadow: popover_shadow(), ..card_style(t) }
}

pub fn popover_shadow() -> Shadow {
    Shadow { color: Color { a: 0.28, ..Color::BLACK }, offset: Vector::new(0.0, 4.0), blur_radius: 14.0 }
}

pub fn banner_style(pick: Tone) -> impl Fn(&Theme) -> container::Style {
    move |t| {
        let c = colors(t);
        let tone = pick(c);
        container::Style {
            background: Some(Background::Color(alpha(tone, if c.dark { 0.14 } else { 0.10 }))),
            text_color: Some(c.text),
            border: Border { radius: RADIUS_CHIP.into(), width: 1.0, color: alpha(tone, 0.35) },
            ..Default::default()
        }
    }
}

pub fn card<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> container::Container<'a, M> {
    container(content).padding(M).style(card_style)
}

pub fn divider<'a, M: 'a>() -> Element<'a, M> {
    rule::horizontal(1).style(|t: &Theme| rule::Style { color: colors(t).border, ..rule::default(t) }).into()
}

pub fn vdivider<'a, M: 'a>() -> Element<'a, M> {
    rule::vertical(1).style(|t: &Theme| rule::Style { color: colors(t).border, ..rule::default(t) }).into()
}

pub fn scroll_style(t: &Theme, status: scrollable::Status) -> scrollable::Style {
    let c = colors(t);
    let mut s = scrollable::default(t, status);
    let hovered = matches!(status, scrollable::Status::Hovered { .. } | scrollable::Status::Dragged { .. });
    for rail in [&mut s.vertical_rail, &mut s.horizontal_rail] {
        rail.background = None;
        rail.border = Border::default();
        rail.scroller.background = Background::Color(alpha(c.muted, if hovered { 0.45 } else { 0.24 }));
        rail.scroller.border = Border { radius: RADIUS_PILL.into(), ..Border::default() };
    }
    s
}

pub fn scroll<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> scrollable::Scrollable<'a, M> {
    scrollable(content)
        .direction(scrollable::Direction::Vertical(scrollable::Scrollbar::new().width(6).scroller_width(6).margin(2)))
        .style(scroll_style)
}

pub fn scroll_both<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> scrollable::Scrollable<'a, M> {
    scrollable(content)
        .direction(scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::new().width(6).scroller_width(6).margin(2),
            horizontal: scrollable::Scrollbar::new().width(6).scroller_width(6).margin(2),
        })
        .style(scroll_style)
}

pub fn input_style(t: &Theme, status: text_input::Status) -> text_input::Style {
    let c = colors(t);
    let border = match status {
        text_input::Status::Focused { .. } => c.accent,
        text_input::Status::Hovered => c.border_strong,
        _ => c.border,
    };
    text_input::Style {
        background: Background::Color(c.raised),
        border: Border { radius: RADIUS_MEDIUM.into(), width: 1.0, color: border },
        icon: c.muted,
        placeholder: c.faint,
        value: c.text,
        selection: alpha(c.accent, 0.3),
    }
}

pub fn input<'a, M: Clone + 'a>(placeholder: &str, value: &str) -> text_input::TextInput<'a, M> {
    text_input(placeholder, value).size(TEXT_UI).padding([7.0, 10.0]).style(input_style)
}

pub fn editor_style(t: &Theme, status: text_editor::Status) -> text_editor::Style {
    let c = colors(t);
    let border = match status {
        text_editor::Status::Focused { .. } => c.accent,
        text_editor::Status::Hovered => c.border_strong,
        _ => c.border,
    };
    text_editor::Style {
        background: Background::Color(c.raised),
        border: Border { radius: RADIUS_MEDIUM.into(), width: 1.0, color: border },
        placeholder: c.faint,
        value: c.text,
        selection: alpha(c.accent, 0.3),
    }
}

// ── Composites ────────────────────────────────────────────────────────

pub fn empty_state<'a, M: 'a>(i: Icon, title: &str, detail: &str, actions: Option<Element<'a, M>>) -> Element<'a, M> {
    let mut col = column![
        themed_icon(i, 36.0, |c| c.faint),
        text(title.to_owned()).size(TEXT_TITLE).font(fonts::SEMIBOLD),
        muted(detail.to_owned(), TEXT_UI).align_x(Alignment::Center),
    ]
    .spacing(S)
    .align_x(Alignment::Center)
    .max_width(440);
    if let Some(a) = actions {
        col = col.push(Space::new().height(S)).push(a);
    }
    container(col).center(Length::Fill).into()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Placement {
    Top(f32),
    Center,
}

pub fn modal<'a, M: Clone + 'a>(content: impl Into<Element<'a, M>>, close: M, placement: Placement) -> Element<'a, M> {
    let layer = container(opaque(content)).center_x(Length::Fill).height(Length::Fill);
    let layer = match placement {
        Placement::Top(top) => layer.padding(Padding { top, bottom: XL, left: XL, right: XL }),
        Placement::Center => layer.center_y(Length::Fill).padding(XL),
    };
    opaque(
        mouse_area(layer.style(|_| container::Style {
            background: Some(Background::Color(Color { a: 0.45, ..Color::BLACK })),
            ..Default::default()
        }))
        .on_press(close),
    )
}

pub fn dialog<'a, M: 'a>(content: impl Into<Element<'a, M>>, width: f32) -> container::Container<'a, M> {
    container(content).padding(XL).width(width).style(popover_style)
}

pub fn dialog_header<'a, M: Clone + 'a>(i: Option<(Icon, Tone)>, title: impl text::IntoFragment<'a>, close: Option<M>) -> Element<'a, M> {
    let mut r = row![].spacing(S).align_y(Alignment::Center);
    if let Some((i, tone)) = i {
        r = r.push(themed_icon(i, ICON_LARGE, tone));
    }
    r = r.push(text(title).size(TEXT_TITLE).font(fonts::SEMIBOLD).width(Length::Fill));
    if let Some(close) = close {
        r = r.push(icon_button(Icon::X, "Close", Some(close)));
    }
    r.into()
}

pub fn dialog_actions<'a, M: 'a>(buttons: Vec<Element<'a, M>>) -> Element<'a, M> {
    let mut r = row![Space::new().width(Length::Fill)].spacing(S).align_y(Alignment::Center);
    for b in buttons {
        r = r.push(b);
    }
    container(r).padding(Padding { top: XS, ..Padding::ZERO }).into()
}

pub fn list_row<'a, M: Clone + 'a>(content: impl Into<Element<'a, M>>, selected: bool, tall: bool) -> button::Button<'a, M> {
    button(content).width(Length::Fill).padding(if tall { ROW_PAD_TALL } else { ROW_PAD }).style(row_style(selected))
}

pub fn menu_item<'a, M: Clone + 'a>(i: Icon, label: impl text::IntoFragment<'a>, msg: M, danger: bool) -> Element<'a, M> {
    list_row(
        row![
            themed_icon(i, ICON_SMALL, move |c| if danger { c.danger } else { c.muted }),
            text(label)
                .size(TEXT_UI)
                .width(Length::Fill)
                .style(move |t: &Theme| text::Style { color: Some(if danger { colors(t).danger } else { colors(t).text }) }),
        ]
        .spacing(ICON_GAP)
        .align_y(Alignment::Center),
        false,
        false,
    )
    .on_press(msg)
    .into()
}

pub fn badge_capsule<'a, M: 'a>(label: impl Into<String>, tone: Option<Tone>) -> Element<'a, M> {
    let s = label.into();
    container(
        text(s)
            .size(TEXT_CAPTION)
            .font(fonts::MEDIUM)
            .style(move |t: &Theme| {
                let c = colors(t);
                text::Style { color: Some(tone.map_or(c.muted, |f| f(c))) }
            }),
    )
    .padding([2.0, TIGHT])
    .style(move |t: &Theme| {
        let c = colors(t);
        let bg = tone.map_or(alpha(c.muted, 0.14), |f| alpha(f(c), 0.16));
        container::Style {
            background: Some(Background::Color(bg)),
            border: Border { radius: RADIUS_PILL.into(), ..Border::default() },
            ..Default::default()
        }
    })
    .into()
}

pub fn segmented<'a, T: Copy + PartialEq + 'a, M: Clone + 'a>(
    options: &[(T, Option<Icon>, &'a str)],
    current: T,
    msg: impl Fn(T) -> M,
) -> Element<'a, M> {
    let mut r = row![].spacing(XXS);
    for (value, i, label) in options.iter().copied() {
        let active = value == current;
        let mut content = row![].spacing(ICON_GAP).align_y(Alignment::Center);
        if let Some(i) = i {
            content = content.push(themed_icon(i, ICON_SMALL, move |c| if active { c.accent } else { c.muted }));
        }
        content = content.push(text(label).size(TEXT_SMALL).font(fonts::MEDIUM));
        r = r.push(button(content).padding(SMALL_BUTTON_PAD).style(segment_style(active)).on_press(msg(value)));
    }
    container(r)
        .padding(XXS)
        .style(|t: &Theme| {
            let c = colors(t);
            container::Style {
                background: Some(Background::Color(c.surface)),
                border: Border { radius: RADIUS_MEDIUM.into(), width: 1.0, color: c.border },
                ..Default::default()
            }
        })
        .into()
}

pub fn dot<'a, M: 'a>(pick: impl Fn(&Hearth) -> Color + 'a, size: f32) -> Element<'a, M> {
    container(Space::new().width(size).height(size))
        .style(move |t: &Theme| container::Style {
            background: Some(Background::Color(pick(colors(t)))),
            border: Border { radius: (size / 2.0).into(), ..Border::default() },
            ..Default::default()
        })
        .into()
}

pub fn toast<'a, M: Clone + 'a>(
    kind: &str,
    message: &str,
    action_label: &str,
    on_action: Option<M>,
    dismiss: M,
) -> Element<'a, M> {
    let (ic, tone): (Icon, Tone) = match kind {
        "error" => (Icon::CircleAlert, |c| c.danger),
        "success" => (Icon::CircleCheck, |c| c.success),
        _ => (Icon::Info, |c| c.info),
    };
    let mut r = row![themed_icon(ic, ICON_SMALL, tone), text(message.to_owned()).size(TEXT_UI)].spacing(S).align_y(Alignment::Center);
    if !action_label.is_empty()
        && let Some(act) = on_action {
            r = r.push(small_button(action_label.to_owned(), secondary_style, Some(act)));
        }
    r = r.push(button(themed_icon(Icon::X, ICON_XS, |c| c.muted)).padding(2.0).style(ghost_style).on_press(dismiss));
    let pill = container(r).padding([S, M]).style(popover_style);
    container(pill)
        .center_x(Length::Fill)
        .align_bottom(Length::Fill)
        .padding(Padding { bottom: STATUS_BAR + M, ..Padding::ZERO })
        .into()
}

pub fn project_icon_tile<'a, M: 'a>(icon_view: &'a ProjectIconView, size: f32, images: &'a HashMap<String, image::Handle>) -> Element<'a, M> {
    if let Some(url) = icon_view.image_url.as_deref()
        && let Some(handle) = images.get(url) {
            return container(image(handle.clone()).width(size).height(size))
                .width(size)
                .height(size)
                .clip(true)
                .style(move |_| container::Style {
                    border: Border { radius: (size * 0.25).into(), ..Border::default() },
                    ..Default::default()
                })
                .into();
        }
    let color_key = icon_view.color.clone();
    let inner: Element<'a, M> = match icon_view.kind.as_str() {
        "emoji" => text(icon_view.text.clone()).size(size * 0.62).into(),
        "symbol" => {
            let sym = project_symbol(&icon_view.symbol);
            let k = color_key.clone();
            themed_icon(sym, size * 0.56, move |h| project_color(&k, h.dark))
        }
        _ => {
            let k = color_key.clone();
            text(icon_view.text.clone())
                .size((size * 0.44).max(9.0))
                .font(fonts::MONO_BOLD)
                .style(move |t: &Theme| text::Style { color: Some(project_color(&k, colors(t).dark)) })
                .into()
        }
    };
    container(inner)
        .width(size)
        .height(size)
        .center_x(size)
        .center_y(size)
        .style(move |t: &Theme| {
            let h = colors(t);
            let fg = project_color(&color_key, h.dark);
            container::Style {
                background: Some(Background::Color(alpha(fg, if h.dark { 0.16 } else { 0.11 }))),
                border: Border { radius: (size * 0.25).into(), ..Border::default() },
                ..Default::default()
            }
        })
        .into()
}

/// Renders `domovoi_core` syntax-highlighted `CodeSpan`s as an Iced `rich_text`.
pub fn code_spans_view<'a, M: 'a>(spans: &[CodeSpan], fallback: &str, dark: bool, size: f32) -> Element<'a, M> {
    let h = if dark { &super::theme::DARK } else { &super::theme::LIGHT };
    if spans.is_empty() {
        return text(fallback.to_owned()).font(fonts::MONO).size(size).into();
    }
    let mut out = Vec::with_capacity(spans.len());
    for sp in spans {
        let mut s = span::<(), _>(sp.text.clone()).font(if sp.kind == "comment" { fonts::MONO_ITALIC } else { fonts::MONO });
        if let Some(c) = code_span_color(&sp.kind, h) {
            s = s.color(c);
        }
        out.push(s);
    }
    rich_text(out).size(size).into()
}

// ── Usage meters ──────────────────────────────────────────────────────

/// A horizontal bar `percent` full (0–100), with an optional tick at
/// `tick` (0: none), e.g. how far through a limit window the period is.
pub fn percent_bar<'a, M: 'a>(percent: u32, tick: u32, tone: Tone) -> Element<'a, M> {
    const H: f32 = 6.0;
    let p = percent.min(100) as u16;
    let track = |t: &Theme| container::Style {
        background: Some(Background::Color(colors(t).sunken)),
        border: Border { radius: (H / 2.0).into(), ..Border::default() },
        ..Default::default()
    };
    let mut fill = row![].height(H);
    if p > 0 {
        fill = fill.push(container(Space::new()).width(Length::FillPortion(p)).height(H).style(move |t: &Theme| container::Style {
            background: Some(Background::Color(tone(colors(t)))),
            border: Border { radius: (H / 2.0).into(), ..Border::default() },
            ..Default::default()
        }));
    }
    if p < 100 {
        fill = fill.push(Space::new().width(Length::FillPortion(100 - p)));
    }
    let mut layers = iced::widget::stack![container(fill).width(Length::Fill).height(H).style(track)];
    if (1..100).contains(&tick) {
        let t = tick as u16;
        layers = layers.push(row![
            Space::new().width(Length::FillPortion(t)),
            container(Space::new()).width(2.0).height(H + 4.0).style(|t: &Theme| container::Style {
                background: Some(Background::Color(alpha(colors(t).text, 0.75))),
                ..Default::default()
            }),
            Space::new().width(Length::FillPortion(100 - t)),
        ]);
    }
    container(layers).height(H + 4.0).align_y(Alignment::Center).width(Length::Fill).into()
}

/// A limit window's tone: danger at 90 %, caution at `high` (75 %), else accent.
pub fn limit_tone(w: &domovoi_core::view::LimitWindowView) -> Tone {
    if w.percent >= 90 {
        |c| c.danger
    } else if w.high {
        |c| c.warning
    } else {
        |c| c.accent
    }
}

/// A provider's usage windows: how much is used, how far through its period
/// it is (the tick), when it refills and whether usage runs ahead of time.
pub fn limit_windows<'a, M: 'a>(windows: &'a [domovoi_core::view::LimitWindowView]) -> Element<'a, M> {
    let mut col = column![].spacing(M);
    for win in windows {
        let tone = limit_tone(win);
        let resets = match win.resets.as_str() {
            "" => String::new(),
            "now" => "Resets now".to_owned(),
            r => {
                let clock = super::time::local_clock(&win.resets_at);
                if clock.is_empty() { format!("Resets {r}") } else { format!("Resets {r} · {clock}") }
            }
        };
        let pace = match win.pace.as_str() {
            "ahead" => "Ahead of pace",
            "on" => "On pace",
            "under" => "Under pace",
            _ => "",
        };
        let details = [resets.as_str(), pace].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
        let mut c = column![
            row![
                text(&win.label).size(TEXT_UI).font(fonts::MEDIUM).width(Length::Fill),
                text(format!("{}%", win.percent)).size(TEXT_UI).font(fonts::MONO).style(move |t: &Theme| text::Style {
                    color: Some(if win.high { tone(colors(t)) } else { colors(t).muted }),
                }),
            ]
            .align_y(Alignment::Center),
            percent_bar(win.percent, win.elapsed_percent, tone),
        ]
        .spacing(XS);
        if !details.is_empty() {
            let warn = win.pace == "ahead" && win.high;
            c = c.push(text(details).size(TEXT_CAPTION).style(move |t: &Theme| text::Style { color: Some(if warn { tone(colors(t)) } else { colors(t).muted }) }));
        }
        col = col.push(c);
    }
    col.into()
}

// ── Image tiles ───────────────────────────────────────────────────────

/// What a transcript image tile has to show.
#[derive(Clone, Copy)]
pub enum TileImage<'a> {
    Loaded(&'a image::Handle),
    Loading,
    Failed,
}

/// A clickable image thumbnail: cropped to fill, rounded, with a hover ring.
/// A failed one offers Retry; `more` covers it with "+N" (images not shown).
pub struct ImageTile<'a, M> {
    pub image: TileImage<'a>,
    pub width: f32,
    pub height: f32,
    pub name: &'a str,
    pub more: Option<usize>,
    pub selected: bool,
    pub on_press: Option<M>,
    pub on_right_press: Option<M>,
}

impl<'a, M: Clone + 'a> ImageTile<'a, M> {
    pub fn view(self) -> Element<'a, M> {
        let (w, h) = (self.width, self.height);
        let small = w.min(h) < 64.0;
        let body: Element<'a, M> = match self.image {
            TileImage::Loaded(handle) => image(handle.clone())
                .width(w)
                .height(h)
                .content_fit(iced::ContentFit::Cover)
                .border_radius(RADIUS_CHIP)
                .into(),
            TileImage::Loading => container(themed_icon(Icon::Image, ICON, |c| c.faint)).center(Length::Fixed(w)).height(h).into(),
            TileImage::Failed => {
                let mut col = column![themed_icon(Icon::RefreshCw, ICON_SMALL, |c| c.muted)].spacing(XXS).align_x(Alignment::Center);
                if !small {
                    col = col.push(muted("Retry", TEXT_CAPTION));
                }
                container(col).center(Length::Fixed(w)).height(h).into()
            }
        };
        let body: Element<'a, M> = match self.more {
            Some(n) if n > 0 => iced::widget::stack![
                body,
                container(text(format!("+{n}")).size(TEXT_TITLE).font(fonts::SEMIBOLD).style(|_| text::Style { color: Some(Color::WHITE) }))
                    .center(Length::Fixed(w))
                    .height(h)
                    .style(|_| container::Style {
                        background: Some(Background::Color(Color { a: 0.5, ..Color::BLACK })),
                        border: Border { radius: RADIUS_CHIP.into(), ..Border::default() },
                        ..Default::default()
                    }),
            ]
            .into(),
            _ => body,
        };
        let selected = self.selected;
        let tile = button(body).padding(0).width(w).height(h).on_press_maybe(self.on_press).style(move |t: &Theme, status| {
            let c = colors(t);
            let ring = if selected {
                c.accent
            } else {
                match status {
                    button::Status::Hovered | button::Status::Pressed => alpha(c.accent, 0.6),
                    _ => c.border,
                }
            };
            button::Style {
                background: Some(Background::Color(alpha(c.muted, 0.10))),
                text_color: c.text,
                border: Border { radius: RADIUS_CHIP.into(), width: if selected { 2.0 } else { 1.0 }, color: ring },
                ..Default::default()
            }
        });
        let tile: Element<'a, M> = match self.on_right_press {
            Some(m) => mouse_area(tile).on_right_press(m).into(),
            None => tile.into(),
        };
        let tip = match self.image {
            TileImage::Failed => format!("{} — couldn’t load", self.name),
            _ => self.name.to_owned(),
        };
        with_tooltip(tile, &tip)
    }
}

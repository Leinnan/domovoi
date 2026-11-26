//! Server connection and pairing view (the server on this computer, a local
//! URL + code, or a remote iroh link), and the dialog a link from outside raises.

use domovoi_core::Event;
use domovoi_core::view::LinkRequestView;
use iced::widget::{Column, Space, button, column, container, image, row, text};
use iced::{Alignment, Element, Length};

use crate::app::{App, ConnectMode, Message, Overlay};
use crate::ui::fonts;
use crate::ui::icons::Icon;
use crate::ui::tokens::*;
use crate::ui::widgets as w;

fn core(e: Event) -> Message {
    Message::Core(Box::new(e))
}

pub fn view(app: &App) -> Element<'_, Message> {
    let conn = &app.vm.connection;
    let busy = matches!(conn.state.as_str(), "pairing" | "connecting");

    let header = row![
        container(image(app.app_icon_handle.clone()).width(44).height(44))
            .width(44)
            .height(44)
            .clip(true)
            .style(|t| iced::widget::container::Style {
                border: iced::Border {
                    radius: 10.0.into(),
                    width: 1.0,
                    color: crate::ui::theme::colors(t).border,
                },
                ..Default::default()
            }),
        column![
            text("Connect to a Domovoi server").size(TEXT_TITLE).font(fonts::SEMIBOLD),
            w::muted("domovoi-server on this machine or another", TEXT_UI),
        ]
        .spacing(XXS),
    ]
    .spacing(M)
    .align_y(Alignment::Center);

    let mut body = Column::new().spacing(L).push(header);

    if let Some(local) = &conn.local_server {
        let r = row![
            w::themed_icon(Icon::Laptop, ICON, |c| c.muted),
            column![
                text(format!("{} · {}", local.label, local.url)).size(TEXT_UI).font(fonts::MEDIUM),
                w::muted("No pairing code needed.", TEXT_CAPTION),
            ]
            .spacing(XXS)
            .width(Length::Fill),
            w::small_button("Connect", w::primary_style, (!busy).then_some(Message::ConnectLocal)),
        ]
        .spacing(S)
        .align_y(Alignment::Center);
        let local_col = column![w::heading("Server on this computer"), container(r).padding([TIGHT, S]).style(w::surface)].spacing(XS);
        body = body.push(w::card(local_col).width(Length::Fill));
    }

    if !conn.environments.is_empty() {
        let mut envs_col = Column::new().spacing(XS).push(w::heading("Saved servers"));
        for env in &conn.environments {
            let icon = if env.remote { Icon::Globe } else { Icon::Laptop };
            let r = row![
                w::themed_icon(icon, ICON, |c| c.muted),
                column![
                    text(&env.label).size(TEXT_UI).font(fonts::MEDIUM),
                    w::muted(&env.detail, TEXT_CAPTION),
                ]
                .spacing(XXS)
                .width(Length::Fill),
                w::small_button(
                    "Connect",
                    w::secondary_style,
                    (!busy).then(|| core(Event::SelectEnvironment { id: env.id.clone() })),
                ),
                w::icon_button(
                    Icon::Trash2,
                    "Forget this server",
                    Some(Message::Overlay(Some(Overlay::ConfirmForgetServer {
                        env_id: env.id.clone(),
                        label: env.label.clone(),
                    }))),
                ),
            ]
            .spacing(S)
            .align_y(Alignment::Center);
            envs_col = envs_col.push(container(r).padding([TIGHT, S]).style(w::surface));
        }
        body = body.push(w::card(envs_col).width(Length::Fill));
    }

    let mode_picker = w::segmented(
        &[
            (ConnectMode::Local, Some(Icon::Server), "Server address"),
            (ConnectMode::Remote, Some(Icon::Globe), "Remote link (iroh)"),
        ],
        app.connect.mode,
        Message::ConnectModeChanged,
    );
    body = body.push(mode_picker);

    match app.connect.mode {
        ConnectMode::Local => {
            body = body
                .push(w::muted(
                    "Enter the server address and the one-time pairing code it printed (or from `domovoi-server pair`).",
                    TEXT_SMALL,
                ))
                .push(
                    column![
                        w::muted("Server URL", TEXT_SMALL).font(fonts::MEDIUM),
                        w::input("http://127.0.0.1:13773", &app.connect.server_url)
                            .on_input(Message::ConnectUrlChanged)
                            .on_submit(Message::ConnectSubmit),
                        Space::new().height(XS),
                        w::muted("Pairing code (optional if already paired)", TEXT_SMALL).font(fonts::MEDIUM),
                        w::input("XXXX-XXXX", &app.connect.pairing_code)
                            .on_input(Message::ConnectCodeChanged)
                            .on_submit(Message::ConnectSubmit),
                    ]
                    .spacing(XS),
                );
        }
        ConnectMode::Remote => {
            body = body
                .push(w::muted(
                    "Paste the `domovoi://connect?...` link from `domovoi-server pair --remote` or Settings → Access on your server. Connects end-to-end over iroh.",
                    TEXT_SMALL,
                ))
                .push(
                    column![
                        w::muted("Pairing link", TEXT_SMALL).font(fonts::MEDIUM),
                        w::input("domovoi://connect?ticket=...&code=...", &app.connect.remote_link)
                            .on_input(Message::ConnectLinkChanged)
                            .on_submit(Message::ConnectSubmit),
                    ]
                    .spacing(XS),
                );
        }
    }

    let can_submit = !busy
        && match app.connect.mode {
            ConnectMode::Local => !app.connect.server_url.trim().is_empty(),
            ConnectMode::Remote => !app.connect.remote_link.trim().is_empty(),
        };

    let mut actions = row![
        w::label_button(
            if busy { "Connecting…" } else { "Connect" },
            w::primary_style,
            can_submit.then_some(Message::ConnectSubmit),
        ),
    ]
    .spacing(S)
    .align_y(Alignment::Center);

    if app.show_main() && app.connect.adding_server {
        actions = actions.push(w::label_button("Cancel", w::secondary_style, Some(Message::ShowAddServer(false))));
    }
    body = body.push(actions);

    if let Some(err) = &conn.error {
        let mut err_col = column![
            row![
                w::themed_icon(Icon::TriangleAlert, ICON, |c| c.danger),
                w::colored(err.clone(), TEXT_UI, |c| c.danger).width(Length::Fill),
            ]
            .spacing(S)
            .align_y(Alignment::Center),
        ]
        .spacing(XS);
        if !conn.hint.is_empty() {
            err_col = err_col.push(w::muted(&conn.hint, TEXT_SMALL));
        }
        body = body.push(container(err_col).padding(M).width(Length::Fill).style(w::banner_style(|c| c.danger)));
    }

    let card = container(body).padding(XXL).max_width(560).width(Length::Fill).style(w::card_style);
    let centered = container(w::scroll(container(card).center_x(Length::Fill).padding(XXL))).center_y(Length::Fill).width(Length::Fill).height(Length::Fill);

    // Subtle theme switcher in bottom-right corner on the Connect screen.
    let footer = container(
        row![
            Space::new().width(Length::Fill),
            button(
                row![
                    w::themed_icon(if app.is_dark() { Icon::Sun } else { Icon::Moon }, ICON_SMALL, |c| c.muted),
                    w::muted(if app.is_dark() { "Light theme" } else { "Dark theme" }, TEXT_SMALL),
                ]
                .spacing(XS)
                .align_y(Alignment::Center)
            )
            .padding(SMALL_BUTTON_PAD)
            .style(w::ghost_style)
            .on_press(Message::Appearance(if app.is_dark() {
                crate::ui::theme::Appearance::Light
            } else {
                crate::ui::theme::Appearance::Dark
            })),
        ]
        .padding([S, L]),
    );

    container(column![centered, footer]).width(Length::Fill).height(Length::Fill).style(w::page).into()
}

/// The dialog for a `domovoi://` link that reached the app from outside: what
/// it asks, what identifies the server, then Cancel and the accepting button.
/// Only those two (and Escape, as Cancel) answer it; a press beside the
/// dialog does nothing, so a link that arrives mid-click is neither accepted
/// nor dropped.
pub fn link_dialog(request: &LinkRequestView) -> Element<'_, Message> {
    fn fact<'a>(label: &'a str, value: iced::widget::Text<'a>) -> Element<'a, Message> {
        row![w::muted(label, TEXT_SMALL).width(LINK_FACT_LABEL), value.size(TEXT_SMALL).width(Length::Fill)]
            .spacing(S)
            .align_y(Alignment::Center)
            .into()
    }

    let mut facts = Column::new().spacing(TIGHT).push(fact("Server key", text(format!("{}…", request.endpoint)).font(fonts::MONO)));
    // The saved name is this app's own; the link's is whatever its author
    // wrote, so it shows only for a server not saved here, and says whose it is.
    if !request.known_label.is_empty() {
        facts = facts.push(fact("Saved here as", text(&request.known_label).font(fonts::MEDIUM)));
    } else if !request.link_name.is_empty() {
        facts = facts.push(fact("Name in the link", text(&request.link_name)));
    }

    let answer = |accept| Message::AnswerLink { id: request.id, accept };
    let dialog = w::dialog(
        column![
            w::dialog_header(Some((Icon::Link, |c| c.accent)), &request.title, None),
            w::muted(&request.message, TEXT_UI),
            container(facts).padding([S, M]).width(Length::Fill).style(w::surface),
            w::dialog_actions(vec![
                w::label_button("Cancel", w::secondary_style, Some(answer(false))).into(),
                w::label_button(&request.confirm_label, w::primary_style, Some(answer(true))).into(),
            ]),
        ]
        .spacing(M),
        460.0,
    );
    w::blocking_modal(dialog, w::Placement::Center)
}

/// Width of the label column in the link dialog, wide enough for "Name in the link".
const LINK_FACT_LABEL: f32 = 120.0;

//! Server connection and pairing view (local URL + code, or remote iroh link).

use domovoi_core::Event;
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
            w::muted("domovoi-server or any t3code-compatible environment", TEXT_UI),
        ]
        .spacing(XXS),
    ]
    .spacing(M)
    .align_y(Alignment::Center);

    let mut body = Column::new().spacing(L).push(header);

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

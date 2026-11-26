//! An entry's images full-window: wheel to zoom and drag to pan, ←/→ (or
//! the side arrows, or the filmstrip) to move between them, and Copy, Save
//! and Open in Default App. Also the thumbnails' right-click menu, and the
//! thumbnail strips the transcript and the Files panel share.

use domovoi_core::view::ImageView;
use iced::widget::{Column, Space, button, column, container, image, keyed_column, mouse_area, opaque, row, text};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Theme};

use crate::app::{App, ImageAction, Message, Overlay, ViewerKey};
use crate::ui::fonts;
use crate::ui::icons::Icon;
use crate::ui::tokens::*;
use crate::ui::widgets::{self as w, ImageTile, TileImage};

/// At most this many thumbnails show in an entry; the last of them says how many more.
pub const SHOWN: usize = 3;
const MULTI: f32 = 96.0;
const STRIP: f32 = 56.0;

/// How an image looks to a tile right now.
pub fn tile_image<'a>(app: &'a App, url: &str) -> TileImage<'a> {
    match app.images.get(url) {
        Some(handle) => TileImage::Loaded(handle),
        None if app.failed_images.contains(url) => TileImage::Failed,
        None => TileImage::Loading,
    }
}

/// One thumbnail of `images[index]`: a click opens the viewer there (or
/// retries a failed one), a right-click offers what can be done with it.
pub fn thumb<'a>(app: &'a App, images: &'a [ImageView], index: usize, width: f32, height: f32, more: Option<usize>) -> Element<'a, Message> {
    let img = &images[index];
    let state = tile_image(app, &img.url);
    let (on_press, on_right_press) = match state {
        TileImage::Loaded(_) => (
            Some(Message::OpenImages { images: images.to_vec(), index }),
            Some(Message::Overlay(Some(Overlay::ImageMenu { images: images.to_vec(), index }))),
        ),
        TileImage::Failed => (Some(Message::RetryImage(img.url.clone())), None),
        // Opens once loaded; the viewer waits for it.
        TileImage::Loading => (Some(Message::OpenImages { images: images.to_vec(), index }), None),
    };
    ImageTile { image: state, width, height, name: &img.name, more, selected: false, on_press, on_right_press }.view()
}

/// An entry's images: one shows large (`single` square); two or three in a
/// row; more, and the third says how many are not shown (all are in the viewer).
pub fn entry_images<'a>(app: &'a App, images: &'a [ImageView], single: f32) -> Element<'a, Message> {
    let mut r = row![].spacing(XS);
    if images.len() == 1 {
        return r.push(thumb(app, images, 0, single, single * 0.75, None)).into();
    }
    let shown = images.len().min(SHOWN);
    for i in 0..shown {
        let hidden = images.len() - shown;
        let more = (i + 1 == shown && hidden > 0).then_some(hidden);
        r = r.push(thumb(app, images, i, MULTI, MULTI, more));
    }
    r.into()
}

/// A folded step group's images, small, all of them (the core sends at most six).
pub fn group_strip<'a>(app: &'a App, images: &'a [ImageView]) -> Element<'a, Message> {
    let mut r = row![].spacing(XS);
    for i in 0..images.len() {
        r = r.push(thumb(app, images, i, STRIP, STRIP, None));
    }
    r.into()
}

fn on_dark<'a>(s: impl text::IntoFragment<'a>, size: f32, a: f32) -> text::Text<'a> {
    text(s).size(size).style(move |_| text::Style { color: Some(Color { a, ..Color::WHITE }) })
}

fn bar_button<'a>(i: Icon, tip: &str, msg: Option<Message>) -> Element<'a, Message> {
    let enabled = msg.is_some();
    let b = button(w::themed_icon(i, ICON_LARGE, move |_| Color { a: if enabled { 0.92 } else { 0.3 }, ..Color::WHITE }))
        .padding(S)
        .on_press_maybe(msg)
        .style(|_: &Theme, status| button::Style {
            background: matches!(status, button::Status::Hovered | button::Status::Pressed).then_some(Background::Color(Color { a: 0.14, ..Color::WHITE })),
            border: Border { radius: RADIUS_MEDIUM.into(), ..Border::default() },
            ..Default::default()
        });
    w::with_tooltip(b, tip)
}

pub fn view<'a>(app: &'a App, images: &'a [ImageView], index: usize) -> Element<'a, Message> {
    let close = Message::Overlay(None);
    let n = images.len();
    let index = index.min(n.saturating_sub(1));
    let Some(current) = images.get(index) else { return Space::new().into() };
    let loaded = app.images.contains_key(&current.url);
    let act = |action: ImageAction| loaded.then(|| Message::Image { action, image: current.clone() });

    let mut title = row![on_dark(&current.name, TEXT_UI, 0.95).font(fonts::SEMIBOLD)].spacing(S).align_y(Alignment::Center);
    if n > 1 {
        title = title.push(on_dark(format!("{} of {n}", index + 1), TEXT_SMALL, 0.6));
    }
    let top = row![
        container(title).width(Length::Fill),
        bar_button(Icon::Copy, &format!("Copy Image ({})", w::shortcut_label("Mod+C")), act(ImageAction::Copy)),
        bar_button(Icon::Download, &format!("Save Image As… ({})", w::shortcut_label("Mod+S")), act(ImageAction::Save)),
        bar_button(Icon::ExternalLink, "Open in Default App", act(ImageAction::OpenExternally)),
        bar_button(Icon::X, "Close (Esc)", Some(close.clone())),
    ]
    .spacing(XS)
    .align_y(Alignment::Center);

    let picture: Element<'a, Message> = match tile_image(app, &current.url) {
        // Keyed by position, so each image starts unzoomed.
        TileImage::Loaded(handle) => keyed_column([(
            index,
            image::viewer(handle.clone()).min_scale(1.0).max_scale(8.0).scale_step(0.15).width(Length::Fill).height(Length::Fill).into(),
        )])
        .width(Length::Fill)
        .height(Length::Fill)
        .into(),
        TileImage::Loading => container(on_dark("Loading image…", TEXT_SMALL, 0.6)).center(Length::Fill).into(),
        TileImage::Failed => container(
            column![
                on_dark("Couldn’t load this image.", TEXT_UI, 0.8),
                w::small_button("Retry", w::secondary_style, Some(Message::RetryImage(current.url.clone()))),
            ]
            .spacing(S)
            .align_x(Alignment::Center),
        )
        .center(Length::Fill)
        .into(),
    };
    let side = |i: Icon, tip: &str, msg: Option<Message>| -> Element<'a, Message> {
        container(if n > 1 { bar_button(i, tip, msg) } else { Space::new().into() }).width(48).center_y(Length::Fill).into()
    };
    let middle = row![
        side(Icon::ArrowLeft, "Previous (←)", (index > 0).then(|| Message::ViewerKey(ViewerKey::Previous))),
        picture,
        side(Icon::ArrowRight, "Next (→)", (index + 1 < n).then(|| Message::ViewerKey(ViewerKey::Next))),
    ]
    .spacing(S)
    .height(Length::Fill)
    .align_y(Alignment::Center);

    let mut col = Column::new().spacing(M).push(top).push(middle);
    if n > 1 {
        let mut strip = row![].spacing(XS);
        for (i, img) in images.iter().enumerate() {
            strip = strip.push(
                ImageTile {
                    image: tile_image(app, &img.url),
                    width: STRIP,
                    height: STRIP,
                    name: &img.name,
                    more: None,
                    selected: i == index,
                    on_press: Some(Message::ShowImage(i)),
                    on_right_press: None,
                }
                .view(),
            );
        }
        col = col.push(container(strip).center_x(Length::Fill));
    }

    // A click on the backdrop (not on the picture or a button) closes.
    opaque(
        mouse_area(
            container(col)
                .padding(Padding { top: M, bottom: L, left: L, right: L })
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|_| container::Style { background: Some(Background::Color(Color { a: 0.94, ..Color::from_rgb8(10, 10, 12) })), ..Default::default() }),
        )
        .on_press(close),
    )
}

/// A thumbnail's right-click menu.
pub fn menu<'a>(images: &'a [ImageView], index: usize) -> Element<'a, Message> {
    let Some(img) = images.get(index) else { return Space::new().into() };
    let act = |action: ImageAction| Message::Image { action, image: img.clone() };
    w::menu_dialog(
        column![
            w::dialog_header(Some((Icon::Image, |c| c.accent)), img.name.as_str(), Some(Message::Overlay(None))),
            w::menu_item(Icon::Eye, "Open", Message::OpenImages { images: images.to_vec(), index }, false),
            w::menu_item(Icon::Copy, "Copy Image", act(ImageAction::Copy), false),
            w::menu_item(Icon::Download, "Save Image As…", act(ImageAction::Save), false),
            w::menu_item(Icon::ExternalLink, "Open in Default App", act(ImageAction::OpenExternally), false),
        ]
        .spacing(XXS),
        320.0,
    )
    .into()
}

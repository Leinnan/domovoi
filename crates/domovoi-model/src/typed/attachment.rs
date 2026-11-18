//! Files attached to messages, and where a captured image came from.

use super::common::Timestamp;
use super::macros::{string_enum, tagged_union_deserialize};
use serde::{Deserialize, Serialize};

/// A file attached to a message or to an answer.
///
/// `Other` is an attachment type this build does not know; the schema gives
/// every such attachment the same five fields.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatAttachment {
    Image(ImageAttachment),
    File(FileAttachment),
    #[serde(untagged)]
    Other(UnknownAttachment),
}

tagged_union_deserialize! {
    ChatAttachment by "type" {
        "image" => Image,
        "file" => File,
        _ => Other,
    }
}

/// An attached image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageAttachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ImageSource>,
}

/// An attached file that is not shown as an image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAttachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<FileSource>,
}

/// An attachment of a type this build does not know.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnknownAttachment {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
}

/// How an attached image was made.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ImageSource {
    /// A capture of an application window.
    #[serde(rename = "snap-shot")]
    SnapShot(Box<SnapShot>),
}

/// How an attached file was made.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum FileSource {
    /// Text the user pasted, kept as a file so it does not fill the prompt.
    #[serde(rename = "pasted-text")]
    PastedText,
}

/// A window capture: which window, and what was readable in it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapShot {
    pub captured_at: Timestamp,
    pub app_name: String,
    pub window_title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accessible_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accessibility: Option<SnapShotAccessibility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_icon_data_url: Option<String>,
}

/// What assistive technology could read in a captured window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "format", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum SnapShotAccessibility {
    FlatText {
        text: String,
        truncated: bool,
    },
    ElementTree {
        coordinate_space: CoordinateSpace,
        image_size: ImageSize,
        truncated: bool,
        root: Box<AccessibilityNode>,
    },
}

string_enum! {
    /// The coordinate system accessibility bounds are given in.
    CoordinateSpace {
        CapturedImage = "captured-image",
    }
}

/// The pixel size of a captured image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSize {
    pub width: u64,
    pub height: u64,
}

/// One element of a captured window's accessibility tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessibilityNode {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub bounds: Option<AccessibilityBounds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<AccessibilityState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<String>>,
    pub children: Vec<AccessibilityNode>,
}

/// Where an accessibility element sits in the captured image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessibilityBounds {
    pub x: u64,
    pub y: u64,
    pub width: u64,
    pub height: u64,
}

/// The state flags of an accessibility element.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessibilityState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busy: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<CheckedState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
}

string_enum! {
    /// Whether a checkbox-like element is ticked.
    CheckedState {
        On = "on",
        Off = "off",
        Mixed = "mixed",
    }
}

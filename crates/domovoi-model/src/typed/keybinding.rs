//! Keybindings as the server resolves them (`keybindings.ts`): what each
//! shortcut does, and the condition under which it applies.

use super::list::ForwardList;
use super::server::ServerConfigIssue;
use super::wire::required;
use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// The longest script id a `script.<id>.run` command names (`MAX_SCRIPT_ID_LENGTH`).
const MAX_SCRIPT_ID_LENGTH: usize = 24;

/// Defines [`KeybindingCommand`] from the commands t3code lists.
macro_rules! keybinding_commands {
    ($($variant:ident = $literal:literal),+ $(,)?) => {
        /// What a keybinding does (`KeybindingCommand`): one of the commands
        /// t3code lists, or the run of a project script.
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        pub enum KeybindingCommand {
            $($variant,)+
            /// Runs a project script: the whole command, `script.<id>.run`.
            ScriptRun(Box<str>),
            /// A command this build does not know, kept as it was sent.
            Other(Box<str>),
        }

        impl KeybindingCommand {
            /// The wire spelling.
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $literal,)+
                    Self::ScriptRun(command) | Self::Other(command) => command,
                }
            }

            /// The command `command` spells.
            fn from_wire(command: &str) -> Self {
                match command {
                    $($literal => Self::$variant,)+
                    _ if script_id(command).is_some() => Self::ScriptRun(command.into()),
                    _ => Self::Other(command.into()),
                }
            }
        }
    };
}

keybinding_commands! {
    WorkspaceFocusNext = "workspace.focusNext",
    WorkspaceFocusPrevious = "workspace.focusPrevious",
    SidebarToggle = "sidebar.toggle",
    NavigationBack = "navigation.back",
    NavigationForward = "navigation.forward",
    TerminalToggle = "terminal.toggle",
    TerminalSplit = "terminal.split",
    TerminalSplitVertical = "terminal.splitVertical",
    TerminalNew = "terminal.new",
    TerminalClose = "terminal.close",
    RightPanelToggle = "rightPanel.toggle",
    RightPanelNew = "rightPanel.new",
    ThreadPanelToggle = "threadPanel.toggle",
    RightPanelToggleMaximized = "rightPanel.toggleMaximized",
    RightPanelClose = "rightPanel.close",
    ViewReopenClosed = "view.reopenClosed",
    PullRequestCopyNumber = "pullRequest.copyNumber",
    DiffToggle = "diff.toggle",
    PreviewToggle = "preview.toggle",
    PreviewRefresh = "preview.refresh",
    PreviewFocusUrl = "preview.focusUrl",
    PreviewZoomIn = "preview.zoomIn",
    PreviewZoomOut = "preview.zoomOut",
    PreviewResetZoom = "preview.resetZoom",
    PreviewFind = "preview.find",
    CommandPaletteToggle = "commandPalette.toggle",
    FilePickerToggle = "filePicker.toggle",
    ProjectSearchToggle = "projectSearch.toggle",
    UsageOpen = "usage.open",
    ThemeSelect = "theme.select",
    AppearanceCycle = "appearance.cycle",
    ThemeEditorToggle = "themeEditor.toggle",
    ComposerStash = "composer.stash",
    ComposerSendAlternate = "composer.sendAlternate",
    ComposerSendBackground = "composer.sendBackground",
    ComposerSendAndNewThread = "composer.sendAndNewThread",
    ComposerDictation = "composer.dictation",
    ComposerHost = "composer.host",
    ComposerCycleHost = "composer.cycleHost",
    ComposerEffort = "composer.effort",
    ComposerIncreaseEffort = "composer.increaseEffort",
    ComposerDecreaseEffort = "composer.decreaseEffort",
    ComposerMode = "composer.mode",
    ComposerWorkspace = "composer.workspace",
    ComposerPreviousWorktree = "composer.previousWorktree",
    ComposerBranch = "composer.branch",
    ChatNew = "chat.new",
    ChatNewLocal = "chat.newLocal",
    ChatNewWithoutProject = "chat.newWithoutProject",
    ChatFind = "chat.find",
    EditorOpenFavorite = "editor.openFavorite",
    UsageCost = "usage.cost",
    UsageTokens = "usage.tokens",
    UsageLimits = "usage.limits",
    UsagePeriodDay = "usage.period.day",
    UsagePeriodWeek = "usage.period.week",
    UsagePeriodMonth = "usage.period.month",
    UsagePeriodQuarter = "usage.period.quarter",
    ModelPickerToggle = "modelPicker.toggle",
    ModelPickerPreviousProvider = "modelPicker.previousProvider",
    ModelPickerNextProvider = "modelPicker.nextProvider",
    ModelPickerJump1 = "modelPicker.jump.1",
    ModelPickerJump2 = "modelPicker.jump.2",
    ModelPickerJump3 = "modelPicker.jump.3",
    ModelPickerJump4 = "modelPicker.jump.4",
    ModelPickerJump5 = "modelPicker.jump.5",
    ModelPickerJump6 = "modelPicker.jump.6",
    ModelPickerJump7 = "modelPicker.jump.7",
    ModelPickerJump8 = "modelPicker.jump.8",
    ModelPickerJump9 = "modelPicker.jump.9",
    ThreadStop = "thread.stop",
    ThreadSteerQueuedMessage = "thread.steerQueuedMessage",
    ThreadEditQueuedMessage = "thread.editQueuedMessage",
    ThreadPrevious = "thread.previous",
    ThreadNext = "thread.next",
    ThreadCopyReference = "thread.copyReference",
    ThreadSettle = "thread.settle",
    ThreadPin = "thread.pin",
    ThreadRegenerateTitle = "thread.regenerateTitle",
    ThreadUndo = "thread.undo",
    ThreadJump1 = "thread.jump.1",
    ThreadJump2 = "thread.jump.2",
    ThreadJump3 = "thread.jump.3",
    ThreadJump4 = "thread.jump.4",
    ThreadJump5 = "thread.jump.5",
    ThreadJump6 = "thread.jump.6",
    ThreadJump7 = "thread.jump.7",
    ThreadJump8 = "thread.jump.8",
    ThreadJump9 = "thread.jump.9",
}

/// The script id in `command` if it is `script.<id>.run` with an id the schema allows.
fn script_id(command: &str) -> Option<&str> {
    let id = command.strip_prefix("script.")?.strip_suffix(".run")?;
    let allowed = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let fits = id.len() <= MAX_SCRIPT_ID_LENGTH
        && id.bytes().next().is_some_and(allowed)
        && id.bytes().all(|byte| allowed(byte) || byte == b'-');
    fits.then_some(id)
}

impl KeybindingCommand {
    /// The id of the project script the command runs, if it runs one.
    pub fn script_id(&self) -> Option<&str> {
        match self {
            Self::ScriptRun(command) => script_id(command),
            _ => None,
        }
    }
}

impl From<&str> for KeybindingCommand {
    /// The command with this wire spelling: one t3code lists, the run of a
    /// project script, or `Other` keeping it.
    fn from(command: &str) -> Self {
        Self::from_wire(command)
    }
}

impl Serialize for KeybindingCommand {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for KeybindingCommand {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl serde::de::Visitor<'_> for Visitor {
            type Value = KeybindingCommand;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a string")
            }

            fn visit_str<E: Error>(self, command: &str) -> Result<KeybindingCommand, E> {
                Ok(KeybindingCommand::from_wire(command))
            }
        }

        deserializer.deserialize_str(Visitor)
    }
}

/// The keys of a shortcut (`KeybindingShortcut`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeybindingShortcut {
    /// The key in lower case, as the keyboard event names it; the space bar is `" "`.
    pub key: String,
    pub meta_key: bool,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub alt_key: bool,
    /// The platform's main modifier: Command on macOS, Control elsewhere.
    pub mod_key: bool,
}

/// The condition a keybinding applies under (`KeybindingWhenNode`): a tree
/// over the names of client states such as `terminalFocus`.
///
/// Decoding is written by hand so that a node of a known `type` must decode
/// in full; only an unknown `type` becomes `Other`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum KeybindingWhenNode {
    /// True while the client state `name` holds.
    Identifier { name: String },
    Not { node: Box<KeybindingWhenNode> },
    And { left: Box<KeybindingWhenNode>, right: Box<KeybindingWhenNode> },
    Or { left: Box<KeybindingWhenNode>, right: Box<KeybindingWhenNode> },
    /// A node of a type this build does not know, kept as it was sent.
    #[serde(untagged)]
    Other(Map<String, Value>),
}

impl<'de> Deserialize<'de> for KeybindingWhenNode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut fields = Map::<String, Value>::deserialize(deserializer)?;
        let Some(kind) = fields.get("type") else {
            return Err(D::Error::missing_field("type"));
        };
        let Some(kind) = kind.as_str() else {
            return Err(D::Error::custom("`type` is not a string"));
        };
        match kind {
            "identifier" => Ok(Self::Identifier { name: required(&mut fields, "name")? }),
            "not" => Ok(Self::Not { node: required(&mut fields, "node")? }),
            "and" => Ok(Self::And { left: required(&mut fields, "left")?, right: required(&mut fields, "right")? }),
            "or" => Ok(Self::Or { left: required(&mut fields, "left")?, right: required(&mut fields, "right")? }),
            _ => Ok(Self::Other(fields)),
        }
    }
}

/// One keybinding as the server resolved it (`ResolvedKeybindingRule`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedKeybindingRule {
    pub command: KeybindingCommand,
    pub shortcut: KeybindingShortcut,
    /// The rule applies always when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_ast: Option<KeybindingWhenNode>,
}

/// Every keybinding in force (`ResolvedKeybindingsConfig`): the user's rules
/// over the defaults, later rules winning.
pub type ResolvedKeybindingsConfig = ForwardList<ResolvedKeybindingRule>;

/// The keybindings after the server changed one (`ServerUpsertKeybindingResult`):
/// the result of `server.upsertKeybinding` and of `server.removeKeybinding`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerUpsertKeybindingResult {
    pub keybindings: ResolvedKeybindingsConfig,
    pub issues: ForwardList<ServerConfigIssue>,
}

/// The result of `server.removeKeybinding` (`ServerRemoveKeybindingResult`).
pub type ServerRemoveKeybindingResult = ServerUpsertKeybindingResult;

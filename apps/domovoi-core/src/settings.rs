//! The settings the settings page shows and changes, by the keys the server
//! knows them by: read from the server's settings record, and written to the
//! copy shown while the server has not answered a change yet.

use crate::model::Model;
use serde::Deserialize;
use serde::de::value::{Error, StrDeserializer};
use serde_json::Number;
use domovoi_model::typed::ServerSettings;

/// What the user gave a setting.
#[derive(Debug, Clone, PartialEq)]
pub enum Typed {
    Switch(bool),
    Text(String),
    Number(i64),
}

/// A setting as its record holds it.
enum Held<'a> {
    Switch(bool),
    Text(&'a str),
    Number(&'a Number),
    /// Not sent, `null`, or not a setting this page knows.
    Unset,
}

/// The settings that are switches and the ones that are text, each with its
/// field: `held` reads them and `put` writes them.
macro_rules! plain_settings {
    (switches { $($switch_key:literal => $switch:ident,)+ } texts { $($text_key:literal => $text:ident,)+ }) => {
        fn held<'a>(settings: &'a ServerSettings, key: &str) -> Held<'a> {
            match key {
                $($switch_key => settings.$switch.map_or(Held::Unset, Held::Switch),)+
                $($text_key => settings.$text.as_deref().map_or(Held::Unset, Held::Text),)+
                "defaultRuntimeMode" => settings.default_runtime_mode.as_ref().map_or(Held::Unset, |mode| Held::Text(mode.as_str())),
                "branchNamingMode" => settings.branch_naming_mode.as_ref().map_or(Held::Unset, |mode| Held::Text(mode.as_str())),
                "sidebarAutoSettleAfterDays" => match &settings.sidebar_auto_settle_after_days {
                    Some(Some(days)) => Held::Number(days),
                    _ => Held::Unset,
                },
                _ => Held::Unset,
            }
        }

        /// Gives the setting `key` the value typed, if its record can hold
        /// it (a switch a switch, a number a number).
        fn put(settings: &mut ServerSettings, key: &str, value: &Typed) -> bool {
            match (key, value) {
                $(($switch_key, Typed::Switch(on)) => settings.$switch = Some(*on),)+
                $(($text_key, Typed::Text(text)) => settings.$text = Some(text.clone()),)+
                ("defaultRuntimeMode", Typed::Text(mode)) => settings.default_runtime_mode = named(mode),
                ("branchNamingMode", Typed::Text(mode)) => settings.branch_naming_mode = named(mode),
                ("sidebarAutoSettleAfterDays", Typed::Number(days)) => settings.sidebar_auto_settle_after_days = Some(Some(Number::from(*days))),
                _ => return false,
            }
            true
        }
    };
}

plain_settings! {
    switches {
        "newWorktreesStartFromOrigin" => new_worktrees_start_from_origin,
        "fastWorktreeCheckout" => fast_worktree_checkout,
        "sidebarAutoSettleOnMerge" => sidebar_auto_settle_on_merge,
        "sidebarAutoSettleKeepPullRequests" => sidebar_auto_settle_keep_pull_requests,
        "defaultAutoPull" => default_auto_pull,
        "ghqCloneLayout" => ghq_clone_layout,
        "keepAwakeWhileAgentsWork" => keep_awake_while_agents_work,
        "enableAgentBrowserAccess" => enable_agent_browser_access,
        "enableProviderUpdateChecks" => enable_provider_update_checks,
        "continueThreadsAfterServerUpdate" => continue_threads_after_server_update,
        "removeAgentCreditsOnMerge" => remove_agent_credits_on_merge,
        "reviewGuidesEnabled" => review_guides_enabled,
    }
    texts {
        "worktreesDirectory" => worktrees_directory,
        "branchNamePrefix" => branch_name_prefix,
        "addProjectBaseDirectory" => add_project_base_directory,
    }
}

/// The change of the setting `key` to `typed` as the settings record holds
/// it: a record with that setting alone. `None` if the record cannot hold
/// the value (words where a number goes, a setting this page does not know).
pub fn patch(key: &str, typed: &Typed) -> Option<ServerSettings> {
    let mut change = ServerSettings::default();
    put(&mut change, key, typed).then_some(change)
}

/// One of a setting's named choices, by its name (any name is one: a newer
/// server's is kept as it is).
fn named<'de, T: Deserialize<'de>>(name: &'de str) -> Option<T> {
    T::deserialize(StrDeserializer::<Error>::new(name)).ok()
}

/// What was typed into `key` that its record cannot hold, while that shows.
fn misfit<'a>(model: &'a Model, key: &str) -> Option<&'a Typed> {
    model.setting_misfits.iter().find(|(of, _)| of == key).map(|(_, typed)| typed)
}

fn of<'a>(model: &'a Model, key: &str) -> Held<'a> {
    model.settings.as_ref().map_or(Held::Unset, |settings| held(settings, key))
}

/// Whether the switch `key` is on.
pub fn switch(model: &Model, key: &str) -> bool {
    match misfit(model, key) {
        Some(typed) => *typed == Typed::Switch(true),
        None => matches!(of(model, key), Held::Switch(true)),
    }
}

/// The setting `key` as text: its text, or its number or switch written out.
pub fn text(model: &Model, key: &str) -> String {
    match misfit(model, key) {
        Some(Typed::Text(text)) => text.clone(),
        Some(Typed::Switch(on)) => on.to_string(),
        Some(Typed::Number(number)) => number.to_string(),
        None => match of(model, key) {
            Held::Text(text) => text.to_owned(),
            Held::Number(number) => number.to_string(),
            Held::Switch(on) => on.to_string(),
            Held::Unset => String::new(),
        },
    }
}

/// The choice made for `key`, or empty when none is.
pub fn choice(model: &Model, key: &str) -> String {
    match misfit(model, key) {
        Some(Typed::Text(text)) => text.clone(),
        Some(_) => String::new(),
        None => match of(model, key) {
            Held::Text(text) => text.to_owned(),
            _ => String::new(),
        },
    }
}

/// Whether the setting `key` shows a number now.
pub fn is_number(model: &Model, key: &str) -> bool {
    match misfit(model, key) {
        Some(typed) => matches!(typed, Typed::Number(_)),
        None => matches!(of(model, key), Held::Number(_)),
    }
}

/// Shows `typed` as the setting `key` (the server has yet to confirm it).
pub fn show(model: &mut Model, key: &str, typed: Typed) {
    model.setting_misfits.retain(|(of, _)| of != key);
    let fits = put(model.settings.get_or_insert_with(ServerSettings::default), key, &typed);
    if !fits {
        model.setting_misfits.push((key.to_owned(), typed));
    }
}

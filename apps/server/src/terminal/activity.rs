//! Advisory activity inferred from a recognized foreground terminal program.

use serde::Serialize;
#[cfg(unix)]
use std::collections::HashMap;

#[cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "foreground process groups are only sampled on Unix"
    )
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Agent {
    ClaudeCode,
    Codex,
}

#[cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "foreground process groups are only sampled on Unix"
    )
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Activity {
    Unknown,
    Idle,
    Working,
    NeedsInput,
    Finished,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Presence {
    pub(super) agent: Agent,
    pub(super) activity: Activity,
    pub(super) revision: u64,
}

#[derive(Default)]
pub(super) struct ScreenMetadata {
    pub(super) title: Option<String>,
    pub(super) cwd: Option<String>,
}

impl vt100::Callbacks for ScreenMetadata {
    fn set_window_title(&mut self, _screen: &mut vt100::Screen, title: &[u8]) {
        self.title = clean(title, 128);
    }

    fn unhandled_osc(&mut self, _screen: &mut vt100::Screen, params: &[&[u8]]) {
        if let [b"7", cwd] = params
            && let Ok(uri) = std::str::from_utf8(cwd)
            && uri.len() <= 4096
            && let Ok(url) = url::Url::parse(uri)
            && url.scheme() == "file"
            && let Ok(path) = url.to_file_path()
        {
            self.cwd = clean(path.to_string_lossy().as_bytes(), 1024);
        }
    }
}

fn clean(bytes: &[u8], limit: usize) -> Option<String> {
    let text: String = String::from_utf8_lossy(bytes)
        .chars()
        .filter(|ch| {
            !ch.is_control() && !matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(limit)
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

#[cfg(unix)]
pub(super) fn is_agent(command: &str) -> Option<Agent> {
    match command.rsplit('/').next()? {
        "claude" => Some(Agent::ClaudeCode),
        "codex" => Some(Agent::Codex),
        _ => None,
    }
}

#[cfg(unix)]
#[derive(PartialEq, Eq)]
pub(super) struct Foreground {
    pub(super) agent: Agent,
    pub(super) native_session: Option<String>,
}

#[cfg(unix)]
impl Foreground {
    pub(super) fn driver(&self) -> &'static str {
        match self.agent {
            Agent::ClaudeCode => "claudeAgent",
            Agent::Codex => "codex",
        }
    }
}

#[cfg(unix)]
fn resumed_session(
    agent: Agent,
    mut args: impl Iterator<Item = impl AsRef<str>>,
) -> Option<String> {
    let expected = match agent {
        Agent::ClaudeCode => "--resume",
        Agent::Codex => "resume",
    };
    let executable = args.next()?;
    if is_agent(executable.as_ref()) != Some(agent) || args.next()?.as_ref() != expected {
        return None;
    }
    let id = args.next()?;
    uuid::Uuid::parse_str(id.as_ref()).ok()?;
    Some(id.as_ref().to_owned())
}

#[cfg(unix)]
pub(super) fn groups(output: &str) -> HashMap<i32, Foreground> {
    let mut candidates = HashMap::new();
    let mut ambiguous = std::collections::HashSet::new();
    for line in output.lines() {
        let mut columns = line.split_whitespace();
        let Some(group) = columns.next().and_then(|group| group.parse::<i32>().ok()) else {
            continue;
        };
        let Some(agent) = columns.next().and_then(is_agent) else {
            continue;
        };
        let native_session = resumed_session(agent, columns);
        let next = Foreground {
            agent,
            native_session,
        };
        if let Some(previous) = candidates.insert(group, next)
            && candidates.get(&group) != Some(&previous)
        {
            ambiguous.insert(group);
        }
    }
    candidates.retain(|group, _| !ambiguous.contains(group));
    candidates
}

#[cfg(unix)]
pub(super) fn classify(agent: Agent, contents: &str, prior: Option<Activity>) -> Activity {
    let bottom: Vec<_> = contents.lines().rev().take(10).map(str::trim).collect();
    let approval = match agent {
        Agent::ClaudeCode => bottom.iter().any(|line| *line == "Do you want to proceed?"),
        Agent::Codex => bottom
            .iter()
            .any(|line| line.starts_with("Would you like to run the following command?")),
    };
    if approval
        && bottom
            .iter()
            .any(|line| line.contains("1.") && line.contains("Yes"))
    {
        return Activity::NeedsInput;
    }
    if bottom.iter().any(|line| line.contains("esc to interrupt")) {
        return Activity::Working;
    }
    let prompt = match agent {
        Agent::ClaudeCode => "❯",
        Agent::Codex => "›",
    };
    if bottom.iter().any(|line| line.starts_with(prompt))
        && bottom.iter().any(|line| line.contains("? for shortcuts"))
    {
        return match prior {
            Some(Activity::Working | Activity::Finished) => Activity::Finished,
            _ => Activity::Idle,
        };
    }
    Activity::Unknown
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn classifies_confirmed_prompt_shapes_without_marking_startup_done() {
        let ready = "› Ask a question\n? for shortcuts";
        assert_eq!(classify(Agent::Codex, ready, None), Activity::Idle);
        assert_eq!(
            classify(Agent::Codex, ready, Some(Activity::Working)),
            Activity::Finished
        );
        assert_eq!(
            classify(Agent::ClaudeCode, "Do you want to proceed?", None),
            Activity::Unknown
        );
        assert_eq!(
            classify(Agent::ClaudeCode, "Do you want to proceed?\n❯ 1. Yes", None),
            Activity::NeedsInput
        );
    }
}

use crate::environments::{endpoint_key, save_environments};
use crate::model::{Environment, Model};
use crate::present::PROJECT_COLORS;
use crate::update::Cmd;
use crate::view::CompanionLookView;
use crux_core::{Command, render::render};
use facet::Facet;
use serde::{Deserialize, Serialize};

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct CompanionLook {
    pub body: String,
    pub eye_shape: String,
    pub pupil: String,
    pub pupil_size: String,
    pub antenna: bool,
    pub ears: String,
    pub eyebrows: bool,
    pub energy: String,
    pub blink: String,
    pub follow: bool,
}

const EYES: [&str; 4] = ["round", "oval", "pill", "sleepy"];

pub fn derive(seed: &str) -> CompanionLook {
    let hash = seed.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    CompanionLook {
        body: PROJECT_COLORS[(hash % PROJECT_COLORS.len() as u64) as usize].into(),
        eye_shape: EYES[((hash >> 24) % EYES.len() as u64) as usize].into(),
        pupil: if hash >> 63 == 0 { "ink" } else { PROJECT_COLORS[((hash >> 32) % PROJECT_COLORS.len() as u64) as usize] }.into(),
        pupil_size: "medium".into(),
        antenna: true,
        ears: "none".into(),
        eyebrows: true,
        energy: "normal".into(),
        blink: "normal".into(),
        follow: true,
    }
}

impl CompanionLook {
    fn valid(&self) -> bool {
        PROJECT_COLORS.contains(&self.body.as_str())
            && EYES.contains(&self.eye_shape.as_str())
            && (self.pupil == "ink" || PROJECT_COLORS.contains(&self.pupil.as_str()))
            && matches!(self.pupil_size.as_str(), "small" | "medium" | "large")
            && matches!(self.ears.as_str(), "none" | "round" | "pointy")
            && matches!(self.energy.as_str(), "calm" | "normal" | "lively")
            && matches!(self.blink.as_str(), "rare" | "normal" | "often")
    }
}

pub(crate) fn resolve(environment: Option<&Environment>) -> CompanionLookView {
    if let Some(env) = environment {
        if let Some(look) = env.companion.as_ref().filter(|look| look.valid()) {
            return CompanionLookView { look: look.clone(), custom: true };
        }
        let seed = if env.is_remote() { endpoint_key(&env.ticket) } else { env.url.clone() };
        return CompanionLookView { look: derive(&seed), custom: false };
    }
    CompanionLookView { look: derive(""), custom: false }
}

pub(crate) fn save(model: &mut Model, id: &str, look: Option<CompanionLook>) -> Cmd {
    if look.as_ref().is_some_and(|look| !look.valid()) {
        return Command::done();
    }
    let Some(env) = model.environments.iter_mut().find(|env| env.id == id) else { return Command::done() };
    if env.companion == look {
        return Command::done();
    }
    env.companion = look;
    save_environments(model).and(render())
}

//! What tokens cost (t3code `usage/usagePricing.ts`, and the rate loading of
//! `usage/UsageService.ts`): LiteLLM's `model_prices_and_context_window.json`,
//! read into a table of per-token rates by model, and the price of one usage
//! record from it.
//!
//! The table:
//! - a model's standard rates (input, output, cache read, cache write, 1-hour
//!   cache write), its `_priority` (fast; or `provider_specific_entry.fast`, a
//!   multiple) and `_ultrafast` rates, and its context tiers
//!   (`*_above_<n>k_tokens`: a request whose prompt is larger bills every token
//!   at the tier). A missing cache rate is the input rate, scaled like the
//!   standard one for a tier or speed; a model without input or output rates is
//!   left out, so a cost is never understated.
//! - Keys are lower-cased. A provider-prefixed key (`openai/gpt-x`) is also
//!   known by its bare name unless another key has it; when several do, the
//!   model maker's own API (`anthropic`, `openai`, ...) wins, and equal rates
//!   merge (a reseller that omits tiers takes them from the other). Bedrock's
//!   `anthropic.<model>` is known as `<model>`.
//! - `<synthetic>` and the bare family names (`opus`, `sonnet`, ...) are never
//!   priced; `name[1m]` is priced as `name`.
//!
//! Pricing a record: a `usagePriceOverrides` entry for the model replaces the
//! table (and a provider-reported cost); otherwise the provider's rate name for
//! the model, then the model itself. A reported cost (Claude's `costUSD`, Pi's
//! `cost.total`) is the cost, split into categories in the list price's
//! proportions; `providerReported` without a rate. Reasoning tokens are part of
//! the output and not charged again. Cache savings are the cached tokens at the
//! input rate less the cache-read rate.
//!
//! Loading the table ([`RateService`]): the copy kept in
//! `<userdata>/usage-model-rates.json` (with `usage-model-rates.meta.json`: when
//! it was fetched, and the ETag and Last-Modified it came with) is read first;
//! it is fetched again after 24 hours, or a minute for `server.refreshUsageRates`
//! and for a summary that meets a model the table does not list (once per
//! model), never sooner than a minute after a failed fetch. A fetch tries
//! raw.githubusercontent.com, then GitHub's contents API, 10 seconds each, and
//! sends the kept copy's validators: a `304` keeps the table and counts as
//! fresh. `pricing.status` is `fresh` after a fetch in this process, `cached`
//! when only the kept copy (or a table from before a failed fetch) prices,
//! `unavailable` with none. `DOMOVOI_USAGE_RATES_URL` names another table (an
//! `http(s)://` or `file://` URL, a test's), with no fallback.
//!
//! Limits: Codex's fast tier (`service_tier` in its thread settings) is not
//! read, so Codex records are priced at standard speed; Claude advisor
//! `iterations` are not split into records of their own.

use domovoi_model::typed::{UsageCategoryCost, UsageCostSource, UsageModelPriceOverride, UsagePricing, UsagePricingStatus};
use serde::de::{Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub const LITELLM_RATES_URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const LITELLM_RATES_API_URL: &str = "https://api.github.com/repos/BerriAI/litellm/contents/model_prices_and_context_window.json?ref=main";
const RATES_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const RATES_REFRESH_FLOOR_MS: i64 = 60 * 1000;
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// The table is about 1.5 MB; a body past this is not it.
const MAX_TABLE_BYTES: usize = 64 * 1024 * 1024;
const TABLE_FILE: &str = "usage-model-rates.json";
const META_FILE: &str = "usage-model-rates.meta.json";

/// Per-token rates in USD (`TokenRates`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TokenRates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_creation: f64,
    /// Cache writes with a 1-hour TTL; the 5-minute rate when unpublished.
    pub cache_creation_1h: f64,
}

/// Standard rates and the faster tiers' (`SpeedRates`).
#[derive(Clone, Debug, PartialEq)]
struct SpeedRates {
    standard: TokenRates,
    fast: Option<TokenRates>,
    ultrafast: Option<TokenRates>,
}

/// A request whose prompt exceeds `threshold_tokens` bills every token at this tier (`ContextTier`).
#[derive(Clone, Debug, PartialEq)]
struct ContextTier {
    threshold_tokens: u64,
    rate: SpeedRates,
}

/// One model's rates (`ModelRate`); LiteLLM's flex and batch variants are left out, transcripts do not record them.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelRate {
    speeds: SpeedRates,
    /// By descending threshold.
    tiers: Vec<ContextTier>,
}

/// Rates by lower-cased model name (`RateTable`).
pub type RateTable = HashMap<String, Arc<ModelRate>>;

/// How fast a request was served (`UsageSpeed`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Speed {
    #[default]
    Standard,
    Fast,
    #[cfg_attr(not(test), expect(dead_code, reason = "priced like t3code's, but no log Domovoi reads records it yet (Codex's service tier is not read)"))]
    Ultrafast,
}

/// The tokens of one record, as pricing reads them.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tokens {
    pub uncached: u64,
    pub cached: u64,
    pub creation: u64,
    /// Of `creation`, written with a 1-hour TTL.
    pub creation_1h: u64,
    pub output: u64,
}

/// A record's price (`PricedUsage`).
#[derive(Clone, Debug, PartialEq)]
pub struct Priced {
    pub cost_usd: f64,
    pub cost_source: UsageCostSource,
    /// `[input, cacheRead, cacheWrite, output]`; none for a cost that is not split.
    pub category: Option<[f64; 4]>,
    pub speed_premium_usd: f64,
    pub cache_savings_usd: f64,
}

// ---- reading LiteLLM's document ----

/// A field's number, or none for anything else (LiteLLM's values are numbers,
/// strings, booleans, lists and objects by turns).
struct Num(Option<f64>);

impl<'de> Deserialize<'de> for Num {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Num;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("any value")
            }
            fn visit_f64<E>(self, v: f64) -> Result<Num, E> {
                Ok(Num(v.is_finite().then_some(v)))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Num, E> {
                Ok(Num(Some(v as f64)))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Num, E> {
                Ok(Num(Some(v as f64)))
            }
            fn visit_bool<E>(self, _: bool) -> Result<Num, E> {
                Ok(Num(None))
            }
            fn visit_str<E>(self, _: &str) -> Result<Num, E> {
                Ok(Num(None))
            }
            fn visit_unit<E>(self) -> Result<Num, E> {
                Ok(Num(None))
            }
            fn visit_none<E>(self) -> Result<Num, E> {
                Ok(Num(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Num, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Num(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Num, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Num(None))
            }
        }
        d.deserialize_any(V)
    }
}

/// A model's entry: its numeric `*cost*` fields, and `provider_specific_entry.fast`.
#[derive(Default)]
struct Entry {
    costs: HashMap<String, f64>,
    fast_multiplier: Option<f64>,
}

impl<'de> Deserialize<'de> for Entry {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        /// `provider_specific_entry`: only `fast` is read.
        struct Specific(Option<f64>);
        impl<'de> Deserialize<'de> for Specific {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct V;
                impl<'de> Visitor<'de> for V {
                    type Value = Specific;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str("any value")
                    }
                    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Specific, A::Error> {
                        let mut fast = None;
                        while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                            if key == "fast" {
                                fast = map.next_value::<Num>()?.0;
                            } else {
                                map.next_value::<IgnoredAny>()?;
                            }
                        }
                        Ok(Specific(fast))
                    }
                    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Specific, A::Error> {
                        while seq.next_element::<IgnoredAny>()?.is_some() {}
                        Ok(Specific(None))
                    }
                    fn visit_bool<E>(self, _: bool) -> Result<Specific, E> {
                        Ok(Specific(None))
                    }
                    fn visit_f64<E>(self, _: f64) -> Result<Specific, E> {
                        Ok(Specific(None))
                    }
                    fn visit_i64<E>(self, _: i64) -> Result<Specific, E> {
                        Ok(Specific(None))
                    }
                    fn visit_u64<E>(self, _: u64) -> Result<Specific, E> {
                        Ok(Specific(None))
                    }
                    fn visit_str<E>(self, _: &str) -> Result<Specific, E> {
                        Ok(Specific(None))
                    }
                    fn visit_unit<E>(self) -> Result<Specific, E> {
                        Ok(Specific(None))
                    }
                }
                d.deserialize_any(V)
            }
        }
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Entry;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a model entry")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Entry, A::Error> {
                let mut entry = Entry::default();
                while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                    if key == "provider_specific_entry" {
                        entry.fast_multiplier = map.next_value::<Specific>()?.0.filter(|m| *m > 0.0);
                    } else if key.contains("cost") {
                        if let Some(n) = map.next_value::<Num>()?.0 {
                            entry.costs.insert(key.into_owned(), n);
                        }
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(entry)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Entry, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Entry::default())
            }
            fn visit_bool<E>(self, _: bool) -> Result<Entry, E> {
                Ok(Entry::default())
            }
            fn visit_f64<E>(self, _: f64) -> Result<Entry, E> {
                Ok(Entry::default())
            }
            fn visit_i64<E>(self, _: i64) -> Result<Entry, E> {
                Ok(Entry::default())
            }
            fn visit_u64<E>(self, _: u64) -> Result<Entry, E> {
                Ok(Entry::default())
            }
            fn visit_str<E>(self, _: &str) -> Result<Entry, E> {
                Ok(Entry::default())
            }
            fn visit_unit<E>(self) -> Result<Entry, E> {
                Ok(Entry::default())
            }
        }
        d.deserialize_any(V)
    }
}

/// The document's entries in the order it lists them.
struct Document(Vec<(String, Entry)>);

impl<'de> Deserialize<'de> for Document {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Document;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object of models")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Document, A::Error> {
                let mut out = vec![];
                while let Some((name, entry)) = map.next_entry::<String, Entry>()? {
                    out.push((name, entry));
                }
                Ok(Document(out))
            }
        }
        d.deserialize_map(V)
    }
}

impl Entry {
    fn get(&self, key: &str) -> Option<f64> {
        self.costs.get(key).copied()
    }

    /// `readTokenRates`: a missing cache rate falls back to the input rate (or,
    /// beside `standard`, to its share of the standard input rate); a missing
    /// 1-hour write rate to the 5-minute one.
    fn token_rates(&self, suffix: &str, standard: Option<&TokenRates>, output_fallback: Option<f64>) -> Option<TokenRates> {
        let input = self.get(&format!("input_cost_per_token{suffix}"))?;
        let output = self.get(&format!("output_cost_per_token{suffix}")).or(output_fallback)?;
        let cache_rate = |name: &str, field: fn(&TokenRates) -> f64, fallback: f64| {
            self.get(&format!("{name}{suffix}")).unwrap_or_else(|| match standard {
                Some(s) if s.input > 0.0 => field(s) / s.input * input,
                _ => fallback,
            })
        };
        let cache_creation = cache_rate("cache_creation_input_token_cost", |r| r.cache_creation, input);
        Some(TokenRates {
            input,
            output,
            cache_read: cache_rate("cache_read_input_token_cost", |r| r.cache_read, input),
            cache_creation,
            cache_creation_1h: cache_rate("cache_creation_input_token_cost_above_1hr", |r| r.cache_creation_1h, cache_creation),
        })
    }

    /// `readSpeedRates`.
    fn speed_rates(&self, standard: TokenRates, suffix: &str) -> SpeedRates {
        let fast = match self.fast_multiplier {
            Some(m) => Some(scale(&standard, m)),
            None => self.token_rates(&format!("{suffix}_priority"), Some(&standard), None),
        };
        let ultrafast = self.token_rates(&format!("{suffix}_ultrafast"), Some(&standard), None);
        SpeedRates { standard, fast, ultrafast }
    }

    /// `readContextTiers`: fields a tier leaves out are the base rates'.
    fn context_tiers(&self, base: &TokenRates) -> Vec<ContextTier> {
        let mut thresholds: Vec<&str> = vec![];
        for key in self.costs.keys() {
            let Some(rest) = key.strip_prefix("input_cost_per_token_above_") else { continue };
            let rest = rest.strip_suffix("_priority").or_else(|| rest.strip_suffix("_ultrafast")).unwrap_or(rest);
            let Some(digits) = rest.strip_suffix("k_tokens") else { continue };
            if (1..=7).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()) && !thresholds.contains(&digits) {
                thresholds.push(digits);
            }
        }
        let mut tiers: Vec<ContextTier> = thresholds
            .into_iter()
            .map(|t| {
                let suffix = format!("_above_{t}k_tokens");
                let standard = self.token_rates(&suffix, Some(base), Some(base.output)).unwrap_or(*base);
                ContextTier { threshold_tokens: t.parse::<u64>().unwrap_or(0) * 1000, rate: self.speed_rates(standard, &suffix) }
            })
            .collect();
        tiers.sort_by_key(|t| std::cmp::Reverse(t.threshold_tokens));
        tiers
    }
}

fn scale(r: &TokenRates, m: f64) -> TokenRates {
    TokenRates { input: r.input * m, output: r.output * m, cache_read: r.cache_read * m, cache_creation: r.cache_creation * m, cache_creation_1h: r.cache_creation_1h * m }
}

/// LiteLLM's prefixes for the model makers' own APIs: their price beats a cloud's or reseller's for a bare name.
const FIRST_PARTY_PROVIDERS: [&str; 7] = ["anthropic", "deepseek", "gemini", "meta", "mistral", "openai", "xai"];

/// Synthetic messages are not billed; bare family names could be any generation.
const UNPRICEABLE_MODELS: [&str; 6] = ["<synthetic>", "synthetic", "opus", "sonnet", "haiku", "fable"];

fn normalize_key(model: &str) -> String {
    model.trim().to_lowercase()
}

fn bare_model_name(key: &str) -> &str {
    key.rsplit_once('/').map_or(key, |(_, bare)| bare)
}

/// Only Bedrock's base id gives a bare alias; its regional (`us.`, `eu.`) ids carry markups.
fn bedrock_base_model_name(key: &str) -> &str {
    key.strip_prefix("anthropic.").unwrap_or(key)
}

/// Claude's bracketed context variants (`[1m]`) price at the base tier, the only name the table has.
fn strip_variant_suffix(key: &str) -> &str {
    key.split_once('[').map_or(key, |(base, _)| base)
}

/// `parseRateTable`: the table of a LiteLLM document; empty for one that is not.
pub fn parse_rate_table(document: &[u8]) -> RateTable {
    let Ok(Document(entries)) = serde_json::from_slice::<Document>(document) else { return RateTable::new() };
    // In the document's order, a later duplicate replacing the earlier one in place.
    let mut order: Vec<String> = vec![];
    let mut table: RateTable = HashMap::new();
    for (name, entry) in entries {
        let Some(standard) = entry.token_rates("", None, None) else { continue };
        let key = normalize_key(&name);
        if key.is_empty() {
            continue;
        }
        let rate = ModelRate { speeds: entry.speed_rates(standard, ""), tiers: entry.context_tiers(&standard) };
        if table.insert(key.clone(), Arc::new(rate)).is_none() {
            order.push(key);
        }
    }
    // Bare names: `None` once two candidates of the same standing conflict.
    let mut aliases: Vec<String> = vec![];
    let mut candidates: HashMap<String, (Option<Arc<ModelRate>>, bool)> = HashMap::new();
    for key in &order {
        let Some(rate) = table.get(key) else { continue };
        let slash = key.find('/');
        let alias = if slash.is_none() { bedrock_base_model_name(key) } else { bare_model_name(key) };
        if alias.is_empty() || alias == key || table.contains_key(alias) {
            continue;
        }
        let first_party = slash.is_some_and(|i| FIRST_PARTY_PROVIDERS.contains(&&key[..i]));
        match candidates.get_mut(alias) {
            None => {
                aliases.push(alias.to_owned());
                candidates.insert(alias.to_owned(), (Some(rate.clone()), first_party));
            }
            Some(held) if first_party && !held.1 => *held = (Some(rate.clone()), first_party),
            Some(held) if held.0.is_some() && first_party == held.1 => {
                held.0 = held.0.as_ref().and_then(|h| merge_alias_rate(h, rate)).map(Arc::new);
            }
            Some(_) => {}
        }
    }
    for alias in aliases {
        if let Some((Some(rate), _)) = candidates.remove(&alias) {
            table.insert(alias, rate);
        }
    }
    table
}

fn same_rates(a: Option<&TokenRates>, b: Option<&TokenRates>) -> bool {
    a == b
}

/// Resellers often omit speed and context tiers: a missing one fills from the other entry instead of conflicting.
fn merge_speed_rates(a: &SpeedRates, b: &SpeedRates) -> Option<SpeedRates> {
    let fast = a.fast.or(b.fast);
    let ultrafast = a.ultrafast.or(b.ultrafast);
    let ok = a.standard == b.standard && same_rates(fast.as_ref(), b.fast.or(fast).as_ref()) && same_rates(ultrafast.as_ref(), b.ultrafast.or(ultrafast).as_ref());
    ok.then_some(SpeedRates { standard: a.standard, fast, ultrafast })
}

fn merge_context_tiers(a: &[ContextTier], b: &[ContextTier]) -> Option<Vec<ContextTier>> {
    if a.is_empty() {
        return Some(b.to_vec());
    }
    if b.is_empty() {
        return Some(a.to_vec());
    }
    if a.len() != b.len() {
        return None;
    }
    a.iter()
        .zip(b)
        .map(|(x, y)| (x.threshold_tokens == y.threshold_tokens).then(|| merge_speed_rates(&x.rate, &y.rate)).flatten().map(|rate| ContextTier { threshold_tokens: x.threshold_tokens, rate }))
        .collect()
}

fn merge_alias_rate(a: &ModelRate, b: &ModelRate) -> Option<ModelRate> {
    Some(ModelRate { speeds: merge_speed_rates(&a.speeds, &b.speeds)?, tiers: merge_context_tiers(&a.tiers, &b.tiers)? })
}

/// `createOverrideRateTable`: `usagePriceOverrides`, by the model as written (trimmed).
pub fn override_table(overrides: &BTreeMap<String, UsageModelPriceOverride>) -> RateTable {
    let per_token = |n: &serde_json::Number| n.as_f64().unwrap_or(0.0) / 1_000_000.0;
    overrides
        .iter()
        .map(|(model, p)| {
            let input = per_token(&p.input_cost_per_million_tokens);
            let cache_creation = p.cache_write_cost_per_million_tokens.as_ref().map_or(input, per_token);
            let standard = TokenRates {
                input,
                output: per_token(&p.output_cost_per_million_tokens),
                cache_read: p.cache_read_cost_per_million_tokens.as_ref().map_or(input, per_token),
                cache_creation,
                cache_creation_1h: cache_creation,
            };
            (model.trim().to_owned(), Arc::new(ModelRate { speeds: SpeedRates { standard, fast: None, ultrafast: None }, tiers: vec![] }))
        })
        .collect()
}

/// `resolveModelAliases`: each alias followed to the model it ends at; one that
/// runs into a cycle is dropped.
pub fn resolve_model_aliases(aliases: &BTreeMap<String, String>) -> HashMap<String, String> {
    // `None` marks both the walk in progress and a walk that ended in a cycle.
    let mut memo: HashMap<&str, Option<&str>> = HashMap::new();
    for model in aliases.keys() {
        if memo.contains_key(model.as_str()) {
            continue;
        }
        let mut path = vec![];
        let mut target = model.as_str();
        while let Some(next) = aliases.get(target) {
            if memo.contains_key(target) {
                break;
            }
            path.push(target);
            memo.insert(target, None);
            target = next;
        }
        let terminal = if aliases.contains_key(target) { memo.get(target).copied().flatten() } else { Some(target) };
        for alias in path {
            memo.insert(alias, terminal);
        }
    }
    aliases.keys().filter_map(|m| memo.get(m.as_str()).copied().flatten().map(|t| (m.clone(), t.to_owned()))).collect()
}

/// `resolveRate`: the table's rate for a model id.
pub fn lookup_rate(table: &RateTable, model: &str) -> Option<Arc<ModelRate>> {
    let key = normalize_key(model);
    let key = strip_variant_suffix(&key);
    let bare = bare_model_name(key);
    if bare.is_empty() || UNPRICEABLE_MODELS.contains(&bare) {
        return None;
    }
    table.get(key).cloned()
}

/// `isUnlistedModel`: a priceable model the table has no rate for; a newer table may list it.
pub fn is_unlisted_model(table: &RateTable, model: &str) -> bool {
    let key = normalize_key(model);
    let bare = bare_model_name(strip_variant_suffix(&key));
    !bare.is_empty() && !UNPRICEABLE_MODELS.contains(&bare) && lookup_rate(table, model).is_none()
}

/// `ratesAt`: the tier the prompt reaches, at the speed asked for; a tier
/// without that speed keeps the base tier's rate for it.
fn rates_at(rate: &ModelRate, speed: Speed, t: &Tokens) -> TokenRates {
    let prompt = t.uncached + t.cached + t.creation;
    let tier = rate.tiers.iter().find(|tier| prompt > tier.threshold_tokens).map(|t| &t.rate);
    let pick = |s: &SpeedRates| match speed {
        Speed::Standard => Some(s.standard),
        Speed::Fast => s.fast,
        Speed::Ultrafast => s.ultrafast,
    };
    match speed {
        Speed::Standard => tier.map_or(rate.speeds.standard, |t| t.standard),
        _ => tier.and_then(pick).or_else(|| pick(&rate.speeds)).unwrap_or_else(|| tier.map_or(rate.speeds.standard, |t| t.standard)),
    }
}

fn cost_by_category(t: &Tokens, r: &TokenRates) -> [f64; 4] {
    let one_hour = t.creation_1h.min(t.creation);
    [
        t.uncached as f64 * r.input,
        t.cached as f64 * r.cache_read,
        (t.creation - one_hour) as f64 * r.cache_creation + one_hour as f64 * r.cache_creation_1h,
        t.output as f64 * r.output,
    ]
}

/// `priceUsage` and `cacheSavingsUsd` of one record, with `rate` the one it
/// resolved to and `overridden` whether that came from `usagePriceOverrides`.
pub fn price(rate: Option<&ModelRate>, overridden: bool, t: &Tokens, speed: Speed, reported_usd: Option<f64>) -> Priced {
    let reported = reported_usd.filter(|r| !overridden && r.is_finite());
    let unsplit = |cost_usd: f64, cost_source: UsageCostSource, cache_savings_usd: f64| Priced { cost_usd, cost_source, category: None, speed_premium_usd: 0.0, cache_savings_usd };
    let Some(rate) = rate else {
        return match reported {
            None => unsplit(0.0, UsageCostSource::Unpriced, 0.0),
            Some(r) => unsplit(r, UsageCostSource::ProviderReported, 0.0),
        };
    };
    let rates = rates_at(rate, speed, t);
    let savings = t.cached as f64 * (rates.input - rates.cache_read);
    let list = cost_by_category(t, &rates);
    let list_usd: f64 = list.iter().sum();
    if let Some(r) = reported
        && list_usd <= 0.0
    {
        return unsplit(r, UsageCostSource::ProviderReported, savings);
    }
    let premium = match speed {
        Speed::Standard => 0.0,
        _ => list_usd - cost_by_category(t, &rates_at(rate, Speed::Standard, t)).iter().sum::<f64>(),
    };
    let scale = reported.map_or(1.0, |r| r / list_usd);
    Priced {
        cost_usd: reported.unwrap_or(list_usd),
        cost_source: if reported.is_some() { UsageCostSource::ProviderReported } else { UsageCostSource::ModelPriced },
        category: Some(list.map(|c| c * scale)),
        speed_premium_usd: premium * scale,
        cache_savings_usd: savings,
    }
}

/// `roundUsd`: to a millionth of a dollar.
pub fn round_usd(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

/// `UsageCategoryCost` of summed categories.
pub fn category_cost(c: [f64; 4]) -> UsageCategoryCost {
    let n = |v: f64| serde_json::Number::from_f64(round_usd(v)).unwrap_or_else(|| serde_json::Number::from(0));
    UsageCategoryCost { input: n(c[0]), cache_read: n(c[1]), cache_write: n(c[2]), output: n(c[3]) }
}

/// Antigravity's name for a model's rates (`antigravityRateModel`): effort and
/// routing variants share the base model's price.
pub fn antigravity_rate_model(model: &str) -> String {
    let base = ["-extra-low", "-low", "-medium", "-high", "-tiered", "-control", "-thinking", "-agent"].iter().find_map(|s| model.strip_suffix(s)).unwrap_or(model);
    let flash_variant = model.strip_prefix("gemini-3-flash-").is_some_and(|rest| matches!(rest, "agent" | "agent-a" | "agent-b" | "a" | "b"));
    if flash_variant {
        "gemini-3.5-flash".into()
    } else if model == "gemini-3.7-flash-safety-le" {
        "gemini-3.7-flash".into()
    } else if base == "gemini-3-flash" {
        "gemini-3-flash-preview".into()
    } else {
        base.to_owned()
    }
}

// ---- loading the table ----

/// What is kept beside the table's copy.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
struct Meta {
    fetched_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_modified: Option<String>,
}

struct State {
    table: Arc<RateTable>,
    meta: Option<Meta>,
    failed_at_ms: Option<i64>,
    status: UsagePricingStatus,
    loaded_saved: bool,
    /// Unlisted models a fresh table was fetched for already.
    checked_unlisted: HashSet<String>,
}

/// The table a summary prices with, and how it stands.
#[derive(Clone)]
pub struct Snapshot {
    pub table: Arc<RateTable>,
    pub pricing: UsagePricing,
}

/// Where the table comes from and is kept (`UsageService`'s rates).
pub struct RateService {
    dir: PathBuf,
    /// URL and `accept` header, tried in turn.
    urls: Vec<(String, &'static str)>,
    state: Mutex<State>,
    /// One fetch at a time (`ratesLock`).
    fetching: tokio::sync::Mutex<()>,
}

/// What a fetch came back with.
enum Fetched {
    Table { table: RateTable, body: Vec<u8>, etag: Option<String>, last_modified: Option<String> },
    NotModified,
}

fn now_ms() -> i64 {
    domovoi_contracts::time_fmt::now_millis()
}

/// Writes `bytes` to `path` through a temporary file, so a reader never sees half of it.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", crate::util::new_uuid()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

impl RateService {
    /// The server's: kept under `<userdata>`, fetched from LiteLLM (or `DOMOVOI_USAGE_RATES_URL`).
    pub fn global() -> &'static RateService {
        static SERVICE: OnceLock<RateService> = OnceLock::new();
        SERVICE.get_or_init(|| {
            let dir = crate::state::STATE.get().map_or_else(|| std::env::temp_dir().join("domovoi-usage-rates"), |s| s.paths.userdata.clone());
            let urls = match std::env::var("DOMOVOI_USAGE_RATES_URL").ok().filter(|u| !u.is_empty()) {
                Some(url) => vec![(url, "application/json")],
                None => vec![(LITELLM_RATES_URL.to_owned(), "application/json"), (LITELLM_RATES_API_URL.to_owned(), "application/vnd.github.raw+json")],
            };
            RateService::new(dir, urls)
        })
    }

    pub fn new(dir: PathBuf, urls: Vec<(String, &'static str)>) -> RateService {
        RateService {
            dir,
            urls,
            state: Mutex::new(State { table: Arc::new(RateTable::new()), meta: None, failed_at_ms: None, status: UsagePricingStatus::Unavailable, loaded_saved: false, checked_unlisted: HashSet::new() }),
            fetching: tokio::sync::Mutex::new(()),
        }
    }

    fn source(&self) -> String {
        self.urls.first().map_or_else(|| LITELLM_RATES_URL.to_owned(), |(u, _)| u.clone())
    }

    /// The table as it stands, the kept copy read first if it has not been.
    pub fn snapshot(&self) -> Snapshot {
        self.load_saved();
        let s = crate::util::locked(&self.state);
        Snapshot {
            table: s.table.clone(),
            pricing: UsagePricing {
                status: s.status.clone(),
                source: self.source(),
                fetched_at: s.meta.as_ref().map(|m| domovoi_contracts::time_fmt::iso_from_millis(m.fetched_at_ms).into()),
                known_models: s.table.len() as u64,
            },
        }
    }

    /// `loadSavedRates`: once, the copy kept from an earlier fetch.
    fn load_saved(&self) {
        if crate::util::locked(&self.state).loaded_saved {
            return;
        }
        let meta = std::fs::read(self.dir.join(META_FILE)).ok().and_then(|b| serde_json::from_slice::<Meta>(&b).ok());
        let table = meta.as_ref().and_then(|_| std::fs::read(self.dir.join(TABLE_FILE)).ok()).map(|b| parse_rate_table(&b)).filter(|t| !t.is_empty());
        let mut s = crate::util::locked(&self.state);
        if s.loaded_saved {
            return;
        }
        s.loaded_saved = true;
        if let (Some(meta), Some(table)) = (meta, table)
            && s.table.is_empty()
        {
            s.table = Arc::new(table);
            s.meta = Some(meta);
            s.status = UsagePricingStatus::Cached;
        }
    }

    /// `loadRates`: fetches the table when the one held is older than a day
    /// (a minute when `force`), unless a fetch failed within the minute.
    pub async fn ensure(&self, force: bool) -> Snapshot {
        let _one = self.fetching.lock().await;
        self.load_saved();
        let now = now_ms();
        let max_age = if force { RATES_REFRESH_FLOOR_MS } else { RATES_TTL_MS };
        let (fetched_at, failed_at, validators) = {
            let s = crate::util::locked(&self.state);
            let validators = s.meta.clone().filter(|_| !s.table.is_empty());
            (s.meta.as_ref().map(|m| m.fetched_at_ms), s.failed_at_ms, validators)
        };
        if fetched_at.is_some_and(|t| now - t < max_age) || failed_at.is_some_and(|t| now - t < RATES_REFRESH_FLOOR_MS) {
            return self.snapshot();
        }
        let mut fetched = None;
        for (url, accept) in &self.urls {
            fetched = fetch(url, accept, validators.as_ref()).await;
            if fetched.is_some() {
                break;
            }
        }
        let mut s = crate::util::locked(&self.state);
        match fetched {
            None => {
                s.failed_at_ms = Some(now_ms());
                if !s.table.is_empty() {
                    s.status = UsagePricingStatus::Cached;
                }
            }
            Some(Fetched::NotModified) => {
                let meta = Meta { fetched_at_ms: now, ..validators.unwrap_or_default() };
                if let Ok(bytes) = serde_json::to_vec(&meta) {
                    let _ = write_atomically(&self.dir.join(META_FILE), &bytes);
                }
                s.meta = Some(meta);
                s.failed_at_ms = None;
                s.status = UsagePricingStatus::Fresh;
            }
            Some(Fetched::Table { table, body, etag, last_modified }) => {
                let meta = Meta { fetched_at_ms: now, etag, last_modified };
                if write_atomically(&self.dir.join(TABLE_FILE), &body).is_ok()
                    && let Ok(bytes) = serde_json::to_vec(&meta)
                {
                    let _ = write_atomically(&self.dir.join(META_FILE), &bytes);
                }
                s.table = Arc::new(table);
                s.meta = Some(meta);
                s.failed_at_ms = None;
                s.status = UsagePricingStatus::Fresh;
            }
        }
        drop(s);
        self.snapshot()
    }

    /// `refreshForUnlistedModels`: a table is kept a day, so a model released
    /// after it was fetched would stay unpriced that long. Models it does not
    /// list fetch it again, once per model against a fresh table. True when the
    /// table may have changed.
    pub async fn refresh_for_unlisted(&self, models: &HashSet<String>) -> bool {
        let unlisted: Vec<String> = {
            let s = crate::util::locked(&self.state);
            models.iter().filter(|m| !s.checked_unlisted.contains(*m) && is_unlisted_model(&s.table, m)).cloned().collect()
        };
        if unlisted.is_empty() {
            return false;
        }
        let before = crate::util::locked(&self.state).table.clone();
        self.ensure(true).await;
        let mut s = crate::util::locked(&self.state);
        // A failed fetch leaves the models unchecked, so a later scan tries again.
        if s.meta.as_ref().is_some_and(|m| now_ms() - m.fetched_at_ms < RATES_REFRESH_FLOOR_MS) {
            s.checked_unlisted.extend(unlisted);
        }
        !Arc::ptr_eq(&before, &s.table)
    }
}

/// One GET of the table; `None` for anything but a table (or a `304` to the validators sent).
async fn fetch(url: &str, accept: &str, validators: Option<&Meta>) -> Option<Fetched> {
    let body = if let Some(path) = url.strip_prefix("file://") {
        tokio::fs::read(path).await.ok()?
    } else {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder().timeout(FETCH_TIMEOUT).build().ok()?;
        let mut req = client.get(url).header(reqwest::header::ACCEPT, accept);
        if let Some(v) = validators {
            if let Some(etag) = &v.etag {
                req = req.header(reqwest::header::IF_NONE_MATCH, etag);
            }
            if let Some(lm) = &v.last_modified {
                req = req.header(reqwest::header::IF_MODIFIED_SINCE, lm);
            }
        }
        let mut res = req.send().await.ok()?;
        if res.status() == reqwest::StatusCode::NOT_MODIFIED && validators.is_some() {
            return Some(Fetched::NotModified);
        }
        if !res.status().is_success() {
            return None;
        }
        let header = |name: reqwest::header::HeaderName| res.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
        let (etag, last_modified) = (header(reqwest::header::ETAG), header(reqwest::header::LAST_MODIFIED));
        let mut body = vec![];
        while let Some(chunk) = tokio::time::timeout(FETCH_TIMEOUT, res.chunk()).await.ok()?.ok()? {
            if body.len() + chunk.len() > MAX_TABLE_BYTES {
                return None;
            }
            body.extend_from_slice(&chunk);
        }
        return parsed(body, etag, last_modified).await;
    };
    parsed(body, None, None).await
}

async fn parsed(body: Vec<u8>, etag: Option<String>, last_modified: Option<String>) -> Option<Fetched> {
    let (table, body) = tokio::task::spawn_blocking(move || (parse_rate_table(&body), body)).await.ok()?;
    (!table.is_empty()).then_some(Fetched::Table { table, body, etag, last_modified })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn table(doc: serde_json::Value) -> RateTable {
        parse_rate_table(doc.to_string().as_bytes())
    }

    fn tokens(uncached: u64, cached: u64, creation: u64, output: u64) -> Tokens {
        Tokens { uncached, cached, creation, creation_1h: 0, output }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12 * a.abs().max(b.abs()).max(1.0)
    }

    #[test]
    fn prices_input_output_and_cache_tokens() {
        let t = table(json!({
            "claude-x": { "input_cost_per_token": 3e-6, "output_cost_per_token": 15e-6, "cache_read_input_token_cost": 0.3e-6,
                "cache_creation_input_token_cost": 3.75e-6, "cache_creation_input_token_cost_above_1hr": 6e-6, "max_tokens": 8192, "mode": "chat" },
            "no-output": { "input_cost_per_token": 1e-6 },
            "sample_spec": { "input_cost_per_token": "0.0", "output_cost_per_token": "x" },
        }));
        assert_eq!(t.len(), 1, "entries without input and output rates are left out");
        let rate = lookup_rate(&t, " Claude-X[1m] ").expect("case, spaces and the variant suffix do not matter");
        let mut tok = tokens(1000, 2000, 300, 400);
        tok.creation_1h = 100;
        let p = price(Some(&rate), false, &tok, Speed::Standard, None);
        let expect = [1000.0 * 3e-6, 2000.0 * 0.3e-6, 200.0 * 3.75e-6 + 100.0 * 6e-6, 400.0 * 15e-6];
        let cat = p.category.unwrap();
        assert!(cat.iter().zip(expect).all(|(a, b)| close(*a, b)), "{cat:?}");
        assert!(close(p.cost_usd, expect.iter().sum()));
        assert_eq!(p.cost_source, UsageCostSource::ModelPriced);
        assert!(close(p.cache_savings_usd, 2000.0 * (3e-6 - 0.3e-6)));
        // Without cache rates, cache tokens cost as input.
        let t = table(json!({ "m": { "input_cost_per_token": 2e-6, "output_cost_per_token": 4e-6 } }));
        let p = price(lookup_rate(&t, "m").as_deref(), false, &tokens(1, 1, 1, 1), Speed::Standard, None);
        assert!(close(p.cost_usd, 2e-6 * 3.0 + 4e-6));
        assert_eq!(p.cache_savings_usd, 0.0);
    }

    #[test]
    fn context_tiers_and_speeds() {
        let t = table(json!({ "g": {
            "input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6, "cache_read_input_token_cost": 0.1e-6,
            "input_cost_per_token_above_200k_tokens": 2e-6, "output_cost_per_token_above_200k_tokens": 3e-6,
            "input_cost_per_token_priority": 1.5e-6, "output_cost_per_token_priority": 3e-6,
        } }));
        let r = lookup_rate(&t, "g").unwrap();
        let small = price(Some(&r), false, &tokens(100, 0, 0, 10), Speed::Standard, None);
        assert!(close(small.cost_usd, 100.0 * 1e-6 + 10.0 * 2e-6));
        // A prompt past the threshold bills every token at the tier; its cache rate keeps the base's share of input.
        let big = price(Some(&r), false, &tokens(150_000, 60_000, 0, 10), Speed::Standard, None);
        assert!(close(big.cost_usd, 150_000.0 * 2e-6 + 60_000.0 * 0.2e-6 + 10.0 * 3e-6), "{big:?}");
        // Fast: the priority rates, and the premium is what they add.
        let fast = price(Some(&r), false, &tokens(100, 0, 0, 10), Speed::Fast, None);
        assert!(close(fast.cost_usd, 100.0 * 1.5e-6 + 10.0 * 3e-6));
        assert!(close(fast.speed_premium_usd, fast.cost_usd - small.cost_usd));
        // A tier without the speed falls back to the base tier's fast rate.
        let fast_big = price(Some(&r), false, &tokens(250_000, 0, 0, 0), Speed::Fast, None);
        assert!(close(fast_big.cost_usd, 250_000.0 * 1.5e-6));
        // `provider_specific_entry.fast` multiplies the standard rates.
        let t = table(json!({ "o": { "input_cost_per_token": 1e-6, "output_cost_per_token": 1e-6, "provider_specific_entry": { "fast": 2 } } }));
        let p = price(lookup_rate(&t, "o").as_deref(), false, &tokens(10, 0, 0, 10), Speed::Fast, None);
        assert!(close(p.cost_usd, 40e-6));
        // Ultrafast with no rate: the standard one.
        let p = price(lookup_rate(&t, "o").as_deref(), false, &tokens(10, 0, 0, 10), Speed::Ultrafast, None);
        assert!(close(p.cost_usd, 20e-6) && p.speed_premium_usd == 0.0);
    }

    #[test]
    fn bare_names_prefer_the_model_maker() {
        let rate = |i: f64| json!({ "input_cost_per_token": i, "output_cost_per_token": i });
        let t = table(json!({
            "openrouter/vendor/model-a": rate(5e-6),
            "openai/model-a": rate(1e-6),
            "azure/model-b": rate(1e-6),
            "vertex/model-b": rate(2e-6),
            "bedrock/model-c": rate(3e-6),
            "other/model-c": rate(3e-6),
            "anthropic.claude-z": rate(7e-6),
            "us.anthropic.claude-z": rate(9e-6),
            "model-d": rate(4e-6),
            "x/model-d": rate(8e-6),
        }));
        let input = |m: &str| lookup_rate(&t, m).map(|r| r.speeds.standard.input);
        assert_eq!(input("model-a"), Some(1e-6), "the first-party price wins over a reseller's");
        assert_eq!(input("model-b"), None, "two clouds that disagree: no bare price");
        assert_eq!(input("model-c"), Some(3e-6), "equal prices merge");
        assert_eq!(input("claude-z"), Some(7e-6), "Bedrock's base id, not a regional one");
        assert_eq!(input("model-d"), Some(4e-6), "a listed bare name keeps its own price");
        assert_eq!(input("OpenAI/Model-A"), Some(1e-6));
    }

    #[test]
    fn unknown_and_unpriceable_models_stay_unpriced() {
        let t = table(json!({ "sonnet": { "input_cost_per_token": 1e-6, "output_cost_per_token": 1e-6 }, "m": { "input_cost_per_token": 1e-6, "output_cost_per_token": 1e-6 } }));
        assert!(lookup_rate(&t, "sonnet").is_none() && lookup_rate(&t, "<synthetic>").is_none());
        assert!(!is_unlisted_model(&t, "sonnet") && !is_unlisted_model(&t, "m") && is_unlisted_model(&t, "claude-new-9"));
        let p = price(lookup_rate(&t, "claude-new-9").as_deref(), false, &tokens(5, 5, 5, 5), Speed::Standard, None);
        assert_eq!((p.cost_usd, p.cost_source, p.category), (0.0, UsageCostSource::Unpriced, None));
        // A reported cost is the cost, with or without a rate (then split like the list price).
        let p = price(None, false, &tokens(5, 0, 0, 5), Speed::Standard, Some(0.25));
        assert_eq!((p.cost_usd, p.cost_source, p.category), (0.25, UsageCostSource::ProviderReported, None));
        let r = lookup_rate(&t, "m").unwrap();
        let p = price(Some(&r), false, &tokens(30, 0, 0, 10), Speed::Standard, Some(0.004));
        assert_eq!(p.cost_source, UsageCostSource::ProviderReported);
        assert!(close(p.cost_usd, 0.004) && close(p.category.unwrap()[0], 0.003) && close(p.category.unwrap()[3], 0.001));
        assert!(parse_rate_table(b"[1,2]").is_empty() && parse_rate_table(b"not json").is_empty());
    }

    #[test]
    fn overrides_replace_the_table_and_a_reported_cost() {
        let o: BTreeMap<String, UsageModelPriceOverride> =
            serde_json::from_value(json!({ " my-model ": { "inputCostPerMillionTokens": 2, "outputCostPerMillionTokens": 10, "cacheReadCostPerMillionTokens": 0.5 } })).unwrap();
        let ot = override_table(&o);
        let r = ot.get("my-model").expect("by the trimmed name");
        assert_eq!(r.speeds.standard, TokenRates { input: 2e-6, output: 10e-6, cache_read: 0.5e-6, cache_creation: 2e-6, cache_creation_1h: 2e-6 });
        let p = price(Some(r), true, &tokens(1_000_000, 1_000_000, 0, 0), Speed::Standard, Some(99.0));
        assert_eq!(p.cost_source, UsageCostSource::ModelPriced, "an override beats the provider's own figure");
        assert!(close(p.cost_usd, 2.5) && close(p.cache_savings_usd, 1.5));
    }

    #[test]
    fn aliases_follow_chains_and_drop_cycles() {
        let a: BTreeMap<String, String> = [("a", "b"), ("b", "c"), ("x", "y"), ("y", "x"), ("z", "x"), ("p", "q")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let r = resolve_model_aliases(&a);
        assert_eq!(r.get("a").map(String::as_str), Some("c"));
        assert_eq!(r.get("b").map(String::as_str), Some("c"));
        assert_eq!(r.get("p").map(String::as_str), Some("q"));
        assert!(!r.contains_key("x") && !r.contains_key("y") && !r.contains_key("z"), "{r:?}");
    }

    #[test]
    fn antigravity_variants_share_a_rate() {
        assert_eq!(antigravity_rate_model("gemini-3.8-flash-high"), "gemini-3.8-flash");
        assert_eq!(antigravity_rate_model("gemini-3-flash-agent-a"), "gemini-3.5-flash");
        assert_eq!(antigravity_rate_model("gemini-3-flash-low"), "gemini-3-flash-preview");
        assert_eq!(antigravity_rate_model("claude-opus-5-5"), "claude-opus-5-5");
    }

    /// A one-connection-at-a-time HTTP server answering from `answer(request head)`.
    async fn serve(answer: impl Fn(&str) -> String + Send + Sync + 'static) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/rates.json", listener.local_addr().unwrap());
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let _ = sock.write_all(answer(&head).as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (url, hits)
    }

    #[tokio::test]
    async fn fetches_keeps_and_revalidates_the_table() {
        let dir = std::env::temp_dir().join(format!("domovoi-rates-{}", crate::util::new_uuid()));
        let body = json!({ "m": { "input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6 } }).to_string();
        let (url, hits) = serve(move |head| {
            if head.contains("if-none-match: \"v1\"") {
                "HTTP/1.1 304 Not Modified\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_owned()
            } else {
                format!("HTTP/1.1 200 OK\r\netag: \"v1\"\r\nlast-modified: Tue, 24 Nov 2026 10:00:00 GMT\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
            }
        })
        .await;
        let s = RateService::new(dir.clone(), vec![(url.clone(), "application/json")]);
        assert_eq!(s.snapshot().pricing.status, UsagePricingStatus::Unavailable);
        let snap = s.ensure(false).await;
        assert_eq!((snap.pricing.status, snap.pricing.known_models, snap.pricing.source.as_str()), (UsagePricingStatus::Fresh, 1, url.as_str()));
        assert!(snap.pricing.fetched_at.is_some());
        // Within the day, no new fetch.
        s.ensure(false).await;
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        // A new process reads the kept copy, as `cached`; a forced fetch past the minute sends the ETag and gets a 304.
        let s = RateService::new(dir.clone(), vec![(url.clone(), "application/json")]);
        let snap = s.snapshot();
        assert_eq!((snap.pricing.status, snap.pricing.known_models), (UsagePricingStatus::Cached, 1));
        {
            let mut st = crate::util::locked(&s.state);
            if let Some(m) = st.meta.as_mut() {
                m.fetched_at_ms -= 2 * RATES_REFRESH_FLOOR_MS;
            }
        }
        let snap = s.ensure(true).await;
        assert_eq!((snap.pricing.status, snap.pricing.known_models), (UsagePricingStatus::Fresh, 1));
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
        // Unreachable: the kept table still prices, as `cached`, and a failure is not retried within the minute.
        let s = RateService::new(dir.clone(), vec![("http://127.0.0.1:9/none".to_owned(), "application/json")]);
        {
            let _ = s.snapshot();
            let mut st = crate::util::locked(&s.state);
            if let Some(m) = st.meta.as_mut() {
                m.fetched_at_ms -= 2 * RATES_TTL_MS;
            }
        }
        let snap = s.ensure(false).await;
        assert_eq!((snap.pricing.status, snap.pricing.known_models), (UsagePricingStatus::Cached, 1));
        assert!(crate::util::locked(&s.state).failed_at_ms.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn an_unlisted_model_fetches_once() {
        let dir = std::env::temp_dir().join(format!("domovoi-rates-unlisted-{}", crate::util::new_uuid()));
        let file = dir.join("table.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, json!({ "m": { "input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6 } }).to_string()).unwrap();
        let s = RateService::new(dir.join("kept"), vec![(format!("file://{}", file.display()), "application/json")]);
        s.ensure(false).await;
        let models: HashSet<String> = ["m".to_owned(), "new-model".to_owned()].into();
        // Fetched within the minute: the table is not fetched again, but the model counts as checked.
        assert!(!s.refresh_for_unlisted(&models).await);
        assert!(crate::util::locked(&s.state).checked_unlisted.contains("new-model"));
        assert!(!s.refresh_for_unlisted(&models).await);
        let _ = std::fs::remove_dir_all(dir);
    }
}

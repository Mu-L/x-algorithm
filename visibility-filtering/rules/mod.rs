mod author_rules;
mod context;
#[cfg(test)]
pub(crate) mod fixtures;
#[cfg(test)]
mod golden_corpus;
pub mod metrics;
pub mod registry;
mod rule_spec;
mod tweet_rules;

#[cfg(test)]
use crate::models::{HydratedTweetCandidate, ViewerFeatures};
#[cfg(test)]
use crate::params::NsfwGatingCountries;
use context::RuleContext;
pub use registry::{Evaluation, RuleEngine, SafetyLevel};
#[cfg(test)]
use rule_spec::Predicate;

#[cfg(test)]
pub(crate) fn test_context<'a>(
    viewer: &'a ViewerFeatures,
    candidate: &'a HydratedTweetCandidate,
) -> RuleContext<'a> {
    use std::sync::LazyLock;

    static NSFW_GATING_COUNTRIES: LazyLock<NsfwGatingCountries> =
        LazyLock::new(NsfwGatingCountries::starting_at_default);
    RuleContext::new(viewer, candidate, &NSFW_GATING_COUNTRIES)
}

#[cfg(test)]
fn holds_narrowed(
    predicate: Predicate,
    viewer: &ViewerFeatures,
    candidate: &HydratedTweetCandidate,
) -> bool {
    predicate.holds(&test_context(viewer, candidate).hydrated_by(predicate.hydrators()))
}

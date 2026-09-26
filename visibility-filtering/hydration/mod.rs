pub mod batch;
mod decode;
mod execute;
pub(crate) mod fallback_cache;
pub mod metrics;
pub(crate) mod plan;
pub(crate) mod sources;
mod store;
pub mod tes_composite;

use crate::models::{HydratedTweetCandidate, PureCore, RawCandidate, TweetId, ViewerFeatures};
use batch::TweetHydrationBatch;
pub(crate) use decode::author::fallback_cache as author_fallback_cache;
pub(crate) use decode::tweet::pure_core_fallback_cache;
pub(crate) use plan::HydrationPlan;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::Arc;
use std::time::Duration;
use xai_visibility_filtering_proto as vf_pb;

pub(crate) const HYDRATION_TIMEOUT: Duration = Duration::from_secs(1);
pub(crate) const INBOUND_ALLOWANCE: Duration = Duration::from_millis(10);

pub(crate) fn request_context(
    entered: tokio::time::Instant,
    grpc_timeout: Option<Duration>,
) -> xai_x_rpc::CallContext {
    xai_x_rpc::CallContext {
        deadline: Some(
            entered
                + grpc_timeout
                    .unwrap_or(HYDRATION_TIMEOUT)
                    .saturating_sub(INBOUND_ALLOWANCE),
        ),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::IntoStaticStr, strum::VariantArray)]
#[strum(serialize_all = "snake_case")]
#[repr(u8)]
pub enum Hydrator {
    PureCore,
    Tweet,
    ConversationControl,
    TweetSafetyLabels,
    ViewerProfile,
    AuthorSafety,
    AuthorLabels,
    Follows,
    Blocks,
    Mutes,
    MuteRetweets,
    BlockedByAuthor,
    BlockedByReplyRoot,
    SuperFollowsExclusive,
    RootFollowsViewer,
    RootFollowsViewerSecondDegree,
    SuperFollowsRoot,
    ViewerCountry,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Hydrators(u32);

impl Hydrators {
    pub const fn empty() -> Self {
        Self(0)
    }

    #[cfg(test)]
    pub fn all() -> Self {
        Self(u32::MAX >> (u32::BITS as usize - <Hydrator as strum::VariantArray>::VARIANTS.len()))
    }

    pub const fn of(hydrator: Hydrator) -> Self {
        Self(1 << hydrator as u8)
    }

    pub const fn with(self, hydrator: Hydrator) -> Self {
        self.union(Self::of(hydrator))
    }

    #[cfg(test)]
    pub const fn without(self, hydrator: Hydrator) -> Self {
        Self(self.0 & !Self::of(hydrator).0)
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, hydrator: Hydrator) -> bool {
        self.0 & Self::of(hydrator).0 != 0
    }

    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

pub(crate) struct HydrationRequest<'a> {
    viewer_id: Option<u64>,
    country_code: Option<String>,
    raw_candidates: &'a [RawCandidate],
}

impl<'a> HydrationRequest<'a> {
    pub(crate) fn new(
        viewer_id: Option<u64>,
        country_code: Option<String>,
        raw_candidates: &'a [RawCandidate],
    ) -> Self {
        Self {
            viewer_id,
            country_code,
            raw_candidates,
        }
    }
}

pub(crate) fn candidate_count_by_key<K: Eq + Hash>(
    keys: impl Iterator<Item = K>,
) -> HashMap<K, usize> {
    let mut candidate_count_by_key = HashMap::with_capacity(keys.size_hint().0);
    for key in keys {
        *candidate_count_by_key.entry(key).or_default() += 1;
    }
    candidate_count_by_key
}

pub(crate) struct HydrationOutput {
    pub(crate) viewer_features: ViewerFeatures,
    pub(crate) candidates: Vec<HydratedTweetCandidate>,
    pub(crate) safety_labels: HashMap<TweetId, Arc<vf_pb::SafetyLabelMap>>,
    pub(crate) failed_ids: HashSet<TweetId>,
    pub(crate) pure_cores: TweetHydrationBatch<PureCore>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_context_budget_is_header_or_hang_guard_minus_allowance() {
        let entered = tokio::time::Instant::now();

        let header = Duration::from_millis(400);
        assert_eq!(
            request_context(entered, Some(header)).deadline,
            Some(entered + header - INBOUND_ALLOWANCE)
        );
        assert_eq!(
            request_context(entered, None).deadline,
            Some(entered + HYDRATION_TIMEOUT - INBOUND_ALLOWANCE)
        );
    }
}

use crate::models::VfAction;
use crate::rules::{Rule, RuleContext};
use xai_visibility_filtering::models::FilteredReason;

#[derive(Clone)]
pub struct TweetFlagDropRule {
    name: &'static str,
    flag: fn(&RuleContext<'_>) -> bool,
    reason: FilteredReason,
}

impl TweetFlagDropRule {
    pub const fn new(
        name: &'static str,
        flag: fn(&RuleContext<'_>) -> bool,
        reason: FilteredReason,
    ) -> Self {
        Self { name, flag, reason }
    }
}

impl Rule for TweetFlagDropRule {
    fn name(&self) -> &'static str {
        self.name
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if (self.flag)(context) {
            return VfAction::Drop(self.reason.clone());
        }
        VfAction::Allow
    }
}

pub const TWEET_NSFW_USER_DROP: TweetFlagDropRule = TweetFlagDropRule::new(
    "TweetNsfwUserDropRule",
    |context| context.has_tweet_nsfw_user_flag(),
    FilteredReason::ContainNsfwMedia,
);
pub const TWEET_NSFW_ADMIN_DROP: TweetFlagDropRule = TweetFlagDropRule::new(
    "TweetNsfwAdminDropRule",
    |context| context.has_tweet_nsfw_admin_flag(),
    FilteredReason::ContainNsfwMedia,
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{HydratedTweetCandidate, NsfwFeature, TweetFeatures};
    use crate::rules::fixtures::{author_viewer, candidate, viewer, VIEWER_ID};

    fn candidate_with_nsfw_flags(user: bool, admin: bool) -> HydratedTweetCandidate {
        candidate()
            .with_tweet_features(TweetFeatures {
                nsfw: NsfwFeature { user, admin },
                ..Default::default()
            })
            .build()
    }

    #[test]
    fn tweet_nsfw_user_drops() {
        let c = candidate_with_nsfw_flags(true, false);
        assert!(matches!(
            TWEET_NSFW_USER_DROP.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(FilteredReason::ContainNsfwMedia)
        ));
    }

    #[test]
    fn tweet_nsfw_user_unset_allows() {
        let c = candidate_with_nsfw_flags(false, false);
        assert!(matches!(
            TWEET_NSFW_USER_DROP.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn tweet_nsfw_user_drops_even_self_view() {
        let c = candidate_with_nsfw_flags(true, false);
        assert!(matches!(
            TWEET_NSFW_USER_DROP.evaluate(&crate::rules::test_context(&author_viewer(), &c)),
            VfAction::Drop(FilteredReason::ContainNsfwMedia)
        ));
    }

    #[test]
    fn tweet_nsfw_admin_drops() {
        let c = candidate_with_nsfw_flags(false, true);
        assert!(matches!(
            TWEET_NSFW_ADMIN_DROP.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(FilteredReason::ContainNsfwMedia)
        ));
    }

    #[test]
    fn tweet_nsfw_admin_unset_allows() {
        let c = candidate_with_nsfw_flags(false, false);
        assert!(matches!(
            TWEET_NSFW_ADMIN_DROP.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn tweet_nsfw_admin_drops_even_self_view() {
        let c = candidate_with_nsfw_flags(false, true);
        assert!(matches!(
            TWEET_NSFW_ADMIN_DROP.evaluate(&crate::rules::test_context(&author_viewer(), &c)),
            VfAction::Drop(FilteredReason::ContainNsfwMedia)
        ));
    }
}

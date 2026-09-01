use crate::models::VfAction;
use crate::rules::{Rule, RuleContext};
use xai_visibility_filtering::models::FilteredReason;

pub struct NullcastedTweetDropRule;

impl Rule for NullcastedTweetDropRule {
    fn name(&self) -> &'static str {
        "NullcastedTweetDropRule"
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if context.tweet().is_nullcast()
            && !context.tweet().is_retweet()
            && !context.tweet().is_community_tweet()
        {
            return VfAction::Drop(FilteredReason::TweetIsNullcast);
        }
        VfAction::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{HydratedTweetCandidate, TweetFeatures};
    use crate::rules::fixtures::{candidate, viewer, VIEWER_ID};

    fn nullcast_candidate() -> HydratedTweetCandidate {
        candidate()
            .with_tweet_features(TweetFeatures {
                is_nullcast: true,
                ..Default::default()
            })
            .build()
    }

    #[test]
    fn nullcast_non_retweet_drops() {
        let rule = NullcastedTweetDropRule;
        let c = nullcast_candidate();
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn nullcast_community_tweet_allows() {
        let rule = NullcastedTweetDropRule;
        let mut c = nullcast_candidate();
        c.tweet_features.is_community_tweet = true;
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn nullcast_retweet_allows() {
        let rule = NullcastedTweetDropRule;
        let mut c = nullcast_candidate();
        c.tweet_features.core.source_tweet_id = Some(99);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn non_nullcast_allows() {
        let rule = NullcastedTweetDropRule;
        let c = candidate().build();
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }
}

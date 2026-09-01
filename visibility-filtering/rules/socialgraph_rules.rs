use crate::models::VfAction;
use crate::rules::{Rule, RuleContext};
use xai_visibility_filtering::models::FilteredReason;

pub struct ViewerBlocksAuthorRule;

impl Rule for ViewerBlocksAuthorRule {
    fn name(&self) -> &'static str {
        "ViewerBlocksAuthorRule"
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if context.viewer().is_logged_out() {
            return VfAction::Allow;
        }
        if context.viewer().blocks_author() {
            return VfAction::Drop(FilteredReason::AuthorBlockViewer);
        }
        VfAction::Allow
    }
}

pub struct MutedRetweetsRule;

impl Rule for MutedRetweetsRule {
    fn name(&self) -> &'static str {
        "MutedRetweetsRule"
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if context.viewer().is_logged_out() {
            return VfAction::Allow;
        }
        if context.tweet().is_retweet() && context.viewer().mutes_retweets_from_author() {
            return VfAction::Drop(FilteredReason::UnspecifiedReason);
        }
        VfAction::Allow
    }
}

pub struct ViewerMutesAuthorRule;

impl Rule for ViewerMutesAuthorRule {
    fn name(&self) -> &'static str {
        "ViewerMutesAuthorRule"
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if context.viewer().is_logged_out() {
            return VfAction::Allow;
        }
        if context.viewer().mutes_author() {
            return VfAction::Drop(FilteredReason::ViewerMutesAuthor);
        }
        VfAction::Allow
    }
}

pub struct DropExclusiveTweetContentRule;

impl Rule for DropExclusiveTweetContentRule {
    fn name(&self) -> &'static str {
        "DropExclusiveTweetContentRule"
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if !context.tweet().is_exclusive() {
            return VfAction::Allow;
        }

        if context.viewer().is_logged_out() {
            return VfAction::Drop(FilteredReason::ExclusiveTweet);
        }

        if context.viewer().is_conversation_author() {
            return VfAction::Allow;
        }

        if context.viewer().super_follows_author() {
            return VfAction::Allow;
        }

        if !context.tweet().is_retweet() && context.viewer().is_author() {
            return VfAction::Allow;
        }

        VfAction::Drop(FilteredReason::ExclusiveTweet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        ExclusiveContentFeatures, HydratedTweetCandidate, ViewerAuthorRelationship,
    };
    use crate::rules::fixtures::{author_viewer, candidate, logged_out_viewer, viewer, VIEWER_ID};

    fn exclusive_candidate(
        tweet_id: u64,
        author_id: u64,
        root_author_id: u64,
    ) -> HydratedTweetCandidate {
        let mut c = candidate().tweet_id(tweet_id).author_id(author_id).build();
        c.exclusive_content = Some(ExclusiveContentFeatures {
            conversation_author_id: root_author_id,
            viewer_super_follows_author: false,
        });
        c
    }

    fn candidate_with_relationship(
        relationship: ViewerAuthorRelationship,
    ) -> HydratedTweetCandidate {
        candidate().with_relationship(relationship).build()
    }

    #[test]
    fn viewer_blocks_author_drops() {
        let c = candidate_with_relationship(ViewerAuthorRelationship {
            viewer_blocks_author: true,
            ..Default::default()
        });
        assert!(matches!(
            ViewerBlocksAuthorRule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(FilteredReason::AuthorBlockViewer)
        ));
    }

    #[test]
    fn viewer_does_not_block_author_allows() {
        let c = candidate_with_relationship(ViewerAuthorRelationship::default());
        assert!(matches!(
            ViewerBlocksAuthorRule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn blocks_rule_allows_logged_out_viewer() {
        let c = candidate_with_relationship(ViewerAuthorRelationship {
            viewer_blocks_author: true,
            ..Default::default()
        });
        assert!(matches!(
            ViewerBlocksAuthorRule.evaluate(&crate::rules::test_context(&logged_out_viewer(), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn viewer_mutes_author_drops() {
        let c = candidate_with_relationship(ViewerAuthorRelationship {
            viewer_mutes_author: true,
            ..Default::default()
        });
        assert!(matches!(
            ViewerMutesAuthorRule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(FilteredReason::ViewerMutesAuthor)
        ));
    }

    #[test]
    fn viewer_does_not_mute_author_allows() {
        let c = candidate_with_relationship(ViewerAuthorRelationship::default());
        assert!(matches!(
            ViewerMutesAuthorRule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn mutes_rule_allows_logged_out_viewer() {
        let c = candidate_with_relationship(ViewerAuthorRelationship {
            viewer_mutes_author: true,
            ..Default::default()
        });
        assert!(matches!(
            ViewerMutesAuthorRule.evaluate(&crate::rules::test_context(&logged_out_viewer(), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn muted_retweets_drops_retweet_from_muting_viewer() {
        let c = candidate()
            .with_relationship(ViewerAuthorRelationship {
                viewer_mutes_retweets_from_author: true,
                ..Default::default()
            })
            .retweet_of(99)
            .build();
        assert!(matches!(
            MutedRetweetsRule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(FilteredReason::UnspecifiedReason)
        ));
    }

    #[test]
    fn muted_retweets_allows_non_retweet() {
        let c = candidate_with_relationship(ViewerAuthorRelationship {
            viewer_mutes_retweets_from_author: true,
            ..Default::default()
        });
        assert!(matches!(
            MutedRetweetsRule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn muted_retweets_allows_logged_out_viewer() {
        let c = candidate()
            .with_relationship(ViewerAuthorRelationship {
                viewer_mutes_retweets_from_author: true,
                ..Default::default()
            })
            .retweet_of(99)
            .build();
        assert!(matches!(
            MutedRetweetsRule.evaluate(&crate::rules::test_context(&logged_out_viewer(), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn non_exclusive_tweet_is_allowed() {
        let rule = DropExclusiveTweetContentRule;
        let action = rule.evaluate(&crate::rules::test_context(
            &viewer(VIEWER_ID),
            &candidate().build(),
        ));
        assert!(matches!(action, VfAction::Allow));
    }

    #[test]
    fn logged_out_viewer_drops_exclusive() {
        let rule = DropExclusiveTweetContentRule;
        let candidate = exclusive_candidate(1, 100, 100);
        let action = rule.evaluate(&crate::rules::test_context(
            &logged_out_viewer(),
            &candidate,
        ));
        assert!(matches!(
            action,
            VfAction::Drop(FilteredReason::ExclusiveTweet)
        ));
    }

    #[test]
    fn root_author_can_see_own_exclusive() {
        let rule = DropExclusiveTweetContentRule;
        let candidate = exclusive_candidate(1, 100, 100);
        let action = rule.evaluate(&crate::rules::test_context(&author_viewer(), &candidate));
        assert!(matches!(action, VfAction::Allow));
    }

    #[test]
    fn super_follower_can_see_exclusive() {
        let rule = DropExclusiveTweetContentRule;
        let mut candidate = exclusive_candidate(1, 100, 100);
        candidate
            .exclusive_content
            .as_mut()
            .unwrap()
            .viewer_super_follows_author = true;
        let action = rule.evaluate(&crate::rules::test_context(&viewer(200), &candidate));
        assert!(matches!(action, VfAction::Allow));
    }

    #[test]
    fn non_super_follower_drops_exclusive() {
        let rule = DropExclusiveTweetContentRule;
        let candidate = exclusive_candidate(1, 100, 100);
        let action = rule.evaluate(&crate::rules::test_context(&viewer(200), &candidate));
        assert!(matches!(
            action,
            VfAction::Drop(FilteredReason::ExclusiveTweet)
        ));
    }

    #[test]
    fn reply_author_can_see_own_reply_in_exclusive_convo() {
        let rule = DropExclusiveTweetContentRule;
        let candidate = exclusive_candidate(2, 200, 100);
        let action = rule.evaluate(&crate::rules::test_context(&viewer(200), &candidate));
        assert!(matches!(action, VfAction::Allow));
    }

    #[test]
    fn retweet_author_cannot_self_view_exclusive() {
        let rule = DropExclusiveTweetContentRule;
        let mut candidate = exclusive_candidate(2, 200, 100);
        candidate.tweet_features.core.source_tweet_id = Some(99);
        let action = rule.evaluate(&crate::rules::test_context(&viewer(200), &candidate));
        assert!(matches!(
            action,
            VfAction::Drop(FilteredReason::ExclusiveTweet)
        ));
    }
}

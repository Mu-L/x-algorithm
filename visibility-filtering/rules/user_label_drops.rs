use crate::models::VfAction;
use crate::rules::{Rule, RuleContext};
use xai_visibility_filtering::models::FilteredReason;
use xai_x_thrift::user_labels::LabelValue;

#[derive(Clone)]
pub struct UserSafetyLabelDropRule {
    name: &'static str,
    label: LabelValue,
    reason: FilteredReason,
    require_non_follower: bool,
}

impl UserSafetyLabelDropRule {
    pub const fn new(
        name: &'static str,
        label: LabelValue,
        reason: FilteredReason,
        require_non_follower: bool,
    ) -> Self {
        Self {
            name,
            label,
            reason,
            require_non_follower,
        }
    }
}

impl Rule for UserSafetyLabelDropRule {
    fn name(&self) -> &'static str {
        self.name
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if context.is_author_viewer() {
            return VfAction::Allow;
        }
        if !context.author_has_user_label(self.label) {
            return VfAction::Allow;
        }
        if self.require_non_follower
            && !context.viewer_is_logged_out()
            && context.viewer_follows_author()
        {
            return VfAction::Allow;
        }
        VfAction::Drop(self.reason.clone())
    }
}

pub const NSFW_HIGH_RECALL_USER_DROP: UserSafetyLabelDropRule = UserSafetyLabelDropRule::new(
    "NsfwHighRecallUserLabelRule",
    LabelValue::NSFW_HIGH_RECALL,
    FilteredReason::UnspecifiedReason,
    false,
);
pub const NSFW_HIGH_PRECISION_USER_DROP: UserSafetyLabelDropRule = UserSafetyLabelDropRule::new(
    "NsfwHighPrecisionUserLabelRule",
    LabelValue::NSFW_HIGH_PRECISION,
    FilteredReason::UnspecifiedReason,
    false,
);
pub const SPAM_HIGH_RECALL_USER_DROP: UserSafetyLabelDropRule = UserSafetyLabelDropRule::new(
    "SpamHighRecallUserLabelRule",
    LabelValue::SPAM_HIGH_RECALL,
    FilteredReason::UnspecifiedReason,
    false,
);
pub const COMPROMISED_USER_DROP: UserSafetyLabelDropRule = UserSafetyLabelDropRule::new(
    "CompromisedUserLabelRule",
    LabelValue::COMPROMISED,
    FilteredReason::UnspecifiedReason,
    false,
);
pub const READ_ONLY_USER_DROP: UserSafetyLabelDropRule = UserSafetyLabelDropRule::new(
    "ReadOnlyUserLabelRule",
    LabelValue::READ_ONLY,
    FilteredReason::UnspecifiedReason,
    false,
);
pub const IMPERSONATION_HIGH_PRECISION_USER_DROP: UserSafetyLabelDropRule =
    UserSafetyLabelDropRule::new(
        "ImpersonationHighPrecisionUserLabelRule",
        LabelValue::IMPERSONATION_HIGH_PRECISION,
        FilteredReason::UnspecifiedReason,
        false,
    );
pub const NSFW_AVATAR_IMAGE_USER_DROP: UserSafetyLabelDropRule = UserSafetyLabelDropRule::new(
    "NsfwAvatarImageRule",
    LabelValue::NSFW_AVATAR_IMAGE,
    FilteredReason::UnspecifiedReason,
    false,
);
pub const NSFW_BANNER_IMAGE_USER_DROP: UserSafetyLabelDropRule = UserSafetyLabelDropRule::new(
    "NsfwBannerImageRule",
    LabelValue::NSFW_BANNER_IMAGE,
    FilteredReason::UnspecifiedReason,
    false,
);
pub const ABUSIVE_HIGH_RECALL_USER_DROP: UserSafetyLabelDropRule = UserSafetyLabelDropRule::new(
    "AbusiveHighRecallRule",
    LabelValue::ABUSIVE_HIGH_RECALL,
    FilteredReason::UnspecifiedReason,
    true,
);
pub const NSFW_NEAR_PERFECT_USER_DROP: UserSafetyLabelDropRule = UserSafetyLabelDropRule::new(
    "NsfwNearPerfectAuthorRule",
    LabelValue::NSFW_NEAR_PERFECT,
    FilteredReason::UnspecifiedReason,
    false,
);
pub const DO_NOT_AMPLIFY_NON_FOLLOWER_USER_DROP: UserSafetyLabelDropRule =
    UserSafetyLabelDropRule::new(
        "DoNotAmplifyNonFollowerRule",
        LabelValue::DO_NOT_AMPLIFY,
        FilteredReason::UnspecifiedReason,
        true,
    );

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::HydratedTweetCandidate;
    use crate::rules::fixtures::{author_viewer, candidate, logged_out_viewer, viewer, VIEWER_ID};

    fn candidate_with_user_label(label: LabelValue) -> HydratedTweetCandidate {
        candidate().with_author_user_label(label).build()
    }

    #[test]
    fn drops_when_author_has_label() {
        let c = candidate_with_user_label(LabelValue::NSFW_HIGH_RECALL);
        assert!(matches!(
            NSFW_HIGH_RECALL_USER_DROP
                .evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(FilteredReason::UnspecifiedReason)
        ));

        let c = candidate_with_user_label(LabelValue::COMPROMISED);
        assert!(matches!(
            COMPROMISED_USER_DROP.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(FilteredReason::UnspecifiedReason)
        ));

        let c = candidate_with_user_label(LabelValue::SPAM_HIGH_RECALL);
        assert!(matches!(
            SPAM_HIGH_RECALL_USER_DROP
                .evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(FilteredReason::UnspecifiedReason)
        ));
    }

    #[test]
    fn allows_author_self_view() {
        let c = candidate_with_user_label(LabelValue::READ_ONLY);
        assert!(matches!(
            READ_ONLY_USER_DROP.evaluate(&crate::rules::test_context(&author_viewer(), &c)),
            VfAction::Allow
        ));

        let c = candidate_with_user_label(LabelValue::NSFW_AVATAR_IMAGE);
        assert!(matches!(
            NSFW_AVATAR_IMAGE_USER_DROP.evaluate(&crate::rules::test_context(&author_viewer(), &c)),
            VfAction::Allow
        ));

        let c = candidate_with_user_label(LabelValue::ABUSIVE_HIGH_RECALL);
        assert!(matches!(
            ABUSIVE_HIGH_RECALL_USER_DROP
                .evaluate(&crate::rules::test_context(&author_viewer(), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn allows_when_label_absent() {
        let c = candidate().build();
        assert!(matches!(
            IMPERSONATION_HIGH_PRECISION_USER_DROP
                .evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn different_label_does_not_match() {
        let c = candidate_with_user_label(LabelValue::LOW_QUALITY);
        assert!(matches!(
            NSFW_HIGH_PRECISION_USER_DROP
                .evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn avatar_banner_blacklist_drop_with_mapped_reason() {
        let c = candidate_with_user_label(LabelValue::NSFW_AVATAR_IMAGE);
        assert!(matches!(
            NSFW_AVATAR_IMAGE_USER_DROP
                .evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(FilteredReason::UnspecifiedReason)
        ));

        let c = candidate_with_user_label(LabelValue::NSFW_BANNER_IMAGE);
        assert!(matches!(
            NSFW_BANNER_IMAGE_USER_DROP
                .evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(FilteredReason::UnspecifiedReason)
        ));
    }

    #[test]
    fn abusive_high_recall_drops_non_followers_and_logged_out() {
        let c = candidate_with_user_label(LabelValue::ABUSIVE_HIGH_RECALL);
        assert!(matches!(
            ABUSIVE_HIGH_RECALL_USER_DROP
                .evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(FilteredReason::UnspecifiedReason)
        ));

        assert!(matches!(
            ABUSIVE_HIGH_RECALL_USER_DROP
                .evaluate(&crate::rules::test_context(&logged_out_viewer(), &c)),
            VfAction::Drop(FilteredReason::UnspecifiedReason)
        ));
    }

    #[test]
    fn abusive_high_recall_allows_follower() {
        let mut c = candidate_with_user_label(LabelValue::ABUSIVE_HIGH_RECALL);
        c.relationship.viewer_follows_author = true;
        assert!(matches!(
            ABUSIVE_HIGH_RECALL_USER_DROP
                .evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }
}

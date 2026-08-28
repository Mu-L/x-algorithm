use crate::models::VfAction;
use crate::rules::{Rule, RuleContext};
use xai_visibility_filtering::models::FilteredReason;

#[derive(Clone)]
pub struct AuthorFlagDropRule {
    name: &'static str,
    flag: fn(&RuleContext<'_>) -> bool,
    reason: FilteredReason,
}

impl AuthorFlagDropRule {
    pub const fn new(
        name: &'static str,
        flag: fn(&RuleContext<'_>) -> bool,
        reason: FilteredReason,
    ) -> Self {
        Self { name, flag, reason }
    }
}

impl Rule for AuthorFlagDropRule {
    fn name(&self) -> &'static str {
        self.name
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if (self.flag)(context) && !context.is_author_viewer() {
            return VfAction::Drop(self.reason.clone());
        }
        VfAction::Allow
    }
}

pub const SUSPENDED_AUTHOR_DROP: AuthorFlagDropRule = AuthorFlagDropRule::new(
    "SuspendedAuthorRule",
    |context| context.author_is_suspended(),
    FilteredReason::AuthorIsSuspended,
);
pub const DEACTIVATED_AUTHOR_DROP: AuthorFlagDropRule = AuthorFlagDropRule::new(
    "DeactivatedAuthorRule",
    |context| context.author_is_deactivated(),
    FilteredReason::AuthorIsDeactivated,
);
pub const ERASED_AUTHOR_DROP: AuthorFlagDropRule = AuthorFlagDropRule::new(
    "ErasedAuthorRule",
    |context| context.author_is_erased(),
    FilteredReason::AuthorAccountIsInactive,
);
pub const OFFBOARDED_AUTHOR_DROP: AuthorFlagDropRule = AuthorFlagDropRule::new(
    "OffboardedAuthorRule",
    |context| context.author_is_offboarded(),
    FilteredReason::AuthorAccountIsInactive,
);
pub const NSFW_USER_AUTHOR_DROP: AuthorFlagDropRule = AuthorFlagDropRule::new(
    "DropNsfwUserAuthorRule",
    |context| context.author_is_nsfw_user(),
    FilteredReason::ContainNsfwMedia,
);
pub const NSFW_ADMIN_AUTHOR_DROP: AuthorFlagDropRule = AuthorFlagDropRule::new(
    "DropNsfwAdminAuthorRule",
    |context| context.author_is_nsfw_admin(),
    FilteredReason::ContainNsfwMedia,
);

pub struct ProtectedAuthorDropRule;

impl Rule for ProtectedAuthorDropRule {
    fn name(&self) -> &'static str {
        "ProtectedAuthorDropRule"
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if context.author_is_protected()
            && !context.is_author_viewer()
            && (context.viewer_is_logged_out() || !context.viewer_follows_author())
        {
            return VfAction::Drop(FilteredReason::AuthorIsProtected);
        }
        VfAction::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AuthorFeatures, HydratedTweetCandidate};
    use crate::rules::fixtures::{author_viewer, candidate, logged_out_viewer, viewer, VIEWER_ID};

    fn candidate_with_author(
        suspended: bool,
        deactivated: bool,
        protected: bool,
    ) -> HydratedTweetCandidate {
        candidate()
            .with_author_features(AuthorFeatures {
                is_suspended: suspended,
                is_deactivated: deactivated,
                is_protected: protected,
                ..Default::default()
            })
            .build()
    }

    #[test]
    fn suspended_author_drops() {
        let rule = SUSPENDED_AUTHOR_DROP;
        let c = candidate_with_author(true, false, false);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn suspended_author_self_view_allows() {
        let rule = SUSPENDED_AUTHOR_DROP;
        let c = candidate_with_author(true, false, false);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&author_viewer(), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn deactivated_author_drops() {
        let rule = DEACTIVATED_AUTHOR_DROP;
        let c = candidate_with_author(false, true, false);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn protected_author_drops_non_follower() {
        let rule = ProtectedAuthorDropRule;
        let c = candidate_with_author(false, false, true);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn protected_author_allows_follower() {
        let rule = ProtectedAuthorDropRule;
        let mut c = candidate_with_author(false, false, true);
        c.relationship.viewer_follows_author = true;
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn protected_author_drops_logged_out_viewer() {
        let rule = ProtectedAuthorDropRule;
        let c = candidate_with_author(false, false, true);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&logged_out_viewer(), &c)),
            VfAction::Drop(FilteredReason::AuthorIsProtected)
        ));
    }

    #[test]
    fn protected_author_allows_self_view() {
        let rule = ProtectedAuthorDropRule;
        let c = candidate_with_author(false, false, true);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&author_viewer(), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn erased_and_offboarded_drops_wire_flag_name_and_reason() {
        let cases: [(&AuthorFlagDropRule, &str, AuthorFeatures); 2] = [
            (
                &ERASED_AUTHOR_DROP,
                "ErasedAuthorRule",
                AuthorFeatures {
                    is_erased: true,
                    ..Default::default()
                },
            ),
            (
                &OFFBOARDED_AUTHOR_DROP,
                "OffboardedAuthorRule",
                AuthorFeatures {
                    is_offboarded: true,
                    ..Default::default()
                },
            ),
        ];
        for (rule, name, author_features) in cases {
            assert_eq!(rule.name(), name);
            let flagged = candidate().with_author_features(author_features).build();
            assert!(matches!(
                rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &flagged)),
                VfAction::Drop(FilteredReason::AuthorAccountIsInactive)
            ));
            let unflagged = candidate().build();
            assert!(matches!(
                rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &unflagged)),
                VfAction::Allow
            ));
        }
    }

    #[test]
    fn normal_author_allows() {
        let rule = SUSPENDED_AUTHOR_DROP;
        let c = candidate_with_author(false, false, false);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    fn candidate_with_nsfw_author(
        is_nsfw_user: bool,
        is_nsfw_admin: bool,
    ) -> HydratedTweetCandidate {
        candidate()
            .with_author_features(AuthorFeatures {
                is_nsfw_user,
                is_nsfw_admin,
                ..Default::default()
            })
            .build()
    }

    #[test]
    fn nsfw_user_author_drops() {
        let rule = NSFW_USER_AUTHOR_DROP;
        let c = candidate_with_nsfw_author(true, false);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn nsfw_user_author_self_view_allows() {
        let rule = NSFW_USER_AUTHOR_DROP;
        let c = candidate_with_nsfw_author(true, false);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&author_viewer(), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn nsfw_admin_author_drops() {
        let rule = NSFW_ADMIN_AUTHOR_DROP;
        let c = candidate_with_nsfw_author(false, true);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn nsfw_admin_author_self_view_allows() {
        let rule = NSFW_ADMIN_AUTHOR_DROP;
        let c = candidate_with_nsfw_author(false, true);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&author_viewer(), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn non_nsfw_author_allows() {
        let rule = NSFW_USER_AUTHOR_DROP;
        let c = candidate_with_nsfw_author(false, false);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }
}

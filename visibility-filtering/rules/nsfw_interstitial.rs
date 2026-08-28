use crate::models::{SafetyLabelType, VfAction};
use crate::rules::{Rule, RuleContext};
use xai_visibility_filtering::models::FilteredReason;

#[derive(Clone, Copy)]
pub struct NsfwMediaInterstitialRule {
    label: SafetyLabelType,
    name: &'static str,
}

impl NsfwMediaInterstitialRule {
    pub const fn new(label: SafetyLabelType, name: &'static str) -> Self {
        Self { label, name }
    }
}

impl Rule for NsfwMediaInterstitialRule {
    fn name(&self) -> &'static str {
        self.name
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if context.has_tweet_safety_label(self.label)
            && !context.is_author_viewer()
            && !context.viewer_allows_sensitive_media()
        {
            return VfAction::Interstitial(FilteredReason::ContainNsfwMedia);
        }
        VfAction::Allow
    }
}

pub static NSFW_HIGH_PRECISION_INTERSTITIAL: NsfwMediaInterstitialRule =
    NsfwMediaInterstitialRule::new(
        SafetyLabelType::NSFW_HIGH_PRECISION,
        "NsfwHighPrecisionInterstitialRule",
    );

pub static GORE_AND_VIOLENCE_INTERSTITIAL: NsfwMediaInterstitialRule =
    NsfwMediaInterstitialRule::new(
        SafetyLabelType::GORE_AND_VIOLENCE_HIGH_PRECISION,
        "GoreAndViolenceInterstitialRule",
    );

pub static NSFW_CARD_IMAGE_INTERSTITIAL: NsfwMediaInterstitialRule = NsfwMediaInterstitialRule::new(
    SafetyLabelType::NSFW_CARD_IMAGE,
    "NsfwCardImageInterstitialRule",
);

pub struct NsfwAuthorInterstitialRule;

impl Rule for NsfwAuthorInterstitialRule {
    fn name(&self) -> &'static str {
        "NsfwAuthorInterstitialRule"
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if context.is_nsfw_flagged()
            && context.has_media()
            && !context.is_author_viewer()
            && !context.viewer_allows_sensitive_media()
        {
            return VfAction::Interstitial(FilteredReason::ContainNsfwMedia);
        }
        VfAction::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AuthorFeatures, HydratedTweetCandidate, NsfwFeature};
    use crate::rules::fixtures::{
        author_viewer, candidate, sensitive_opt_in_viewer, viewer, VIEWER_ID,
    };

    #[test]
    fn interstitial_blurs_non_opt_in() {
        let c = candidate()
            .with_label(SafetyLabelType::NSFW_HIGH_PRECISION)
            .build();
        assert!(matches!(
            NSFW_HIGH_PRECISION_INTERSTITIAL
                .evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Interstitial(_)
        ));
    }

    #[test]
    fn interstitial_allows_opt_in() {
        let c = candidate()
            .with_label(SafetyLabelType::NSFW_HIGH_PRECISION)
            .build();
        assert!(matches!(
            NSFW_HIGH_PRECISION_INTERSTITIAL
                .evaluate(&crate::rules::test_context(&sensitive_opt_in_viewer(), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn interstitial_allows_self_view() {
        let c = candidate()
            .with_label(SafetyLabelType::NSFW_HIGH_PRECISION)
            .build();
        assert!(matches!(
            NSFW_HIGH_PRECISION_INTERSTITIAL
                .evaluate(&crate::rules::test_context(&author_viewer(), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn interstitial_allows_no_label() {
        let c = candidate().build();
        assert!(matches!(
            NSFW_HIGH_PRECISION_INTERSTITIAL
                .evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    fn nsfw_author_candidate() -> HydratedTweetCandidate {
        candidate()
            .with_media()
            .with_author_features(AuthorFeatures {
                is_nsfw_user: true,
                ..Default::default()
            })
            .build()
    }

    #[test]
    fn author_interstitial_blurs_non_opt_in() {
        assert!(matches!(
            NsfwAuthorInterstitialRule.evaluate(&crate::rules::test_context(
                &viewer(VIEWER_ID),
                &nsfw_author_candidate()
            )),
            VfAction::Interstitial(_)
        ));
    }

    #[test]
    fn author_interstitial_allows_opt_in() {
        assert!(matches!(
            NsfwAuthorInterstitialRule.evaluate(&crate::rules::test_context(
                &sensitive_opt_in_viewer(),
                &nsfw_author_candidate()
            )),
            VfAction::Allow
        ));
    }

    #[test]
    fn author_interstitial_allows_when_no_media() {
        let mut c = nsfw_author_candidate();
        c.tweet_features.media.has_media = false;
        assert!(matches!(
            NsfwAuthorInterstitialRule
                .evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    fn nsfw_tweet_flag_candidate() -> HydratedTweetCandidate {
        let mut c = nsfw_author_candidate();
        c.author_features = AuthorFeatures::default();
        c.tweet_features.nsfw = NsfwFeature {
            user: true,
            admin: false,
        };
        c
    }

    #[test]
    fn tweet_flag_interstitial_blurs_non_opt_in() {
        assert!(matches!(
            NsfwAuthorInterstitialRule.evaluate(&crate::rules::test_context(
                &viewer(VIEWER_ID),
                &nsfw_tweet_flag_candidate()
            )),
            VfAction::Interstitial(_)
        ));
    }

    #[test]
    fn tweet_admin_flag_interstitial_blurs_non_opt_in() {
        let mut c = nsfw_tweet_flag_candidate();
        c.tweet_features.nsfw = NsfwFeature {
            user: false,
            admin: true,
        };
        assert!(matches!(
            NsfwAuthorInterstitialRule
                .evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Interstitial(_)
        ));
    }

    #[test]
    fn both_flag_sources_interstitial_blurs_non_opt_in() {
        let mut c = nsfw_tweet_flag_candidate();
        c.author_features.is_nsfw_admin = true;
        assert!(matches!(
            NsfwAuthorInterstitialRule
                .evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Interstitial(_)
        ));
    }

    #[test]
    fn no_flags_allows() {
        let mut c = nsfw_tweet_flag_candidate();
        c.tweet_features.nsfw = NsfwFeature::default();
        assert!(matches!(
            NsfwAuthorInterstitialRule
                .evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn tweet_flag_interstitial_allows_opt_in() {
        assert!(matches!(
            NsfwAuthorInterstitialRule.evaluate(&crate::rules::test_context(
                &sensitive_opt_in_viewer(),
                &nsfw_tweet_flag_candidate()
            )),
            VfAction::Allow
        ));
    }

    #[test]
    fn tweet_flag_interstitial_allows_self_view() {
        let c = nsfw_tweet_flag_candidate();
        assert!(matches!(
            NsfwAuthorInterstitialRule.evaluate(&crate::rules::test_context(&author_viewer(), &c)),
            VfAction::Allow
        ));
    }
}

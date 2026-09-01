use crate::models::{SafetyLabelType, VfAction};
use crate::params::NsfwGatingCountries;
use crate::rules::{Rule, RuleContext};
use std::sync::Arc;
use xai_visibility_filtering::models::FilteredReason;

fn nsfw_base_condition(context: &RuleContext<'_>) -> bool {
    let tweet = context.tweet();
    !context.viewer().is_author()
        && tweet.has_media()
        && (tweet.has_safety_label(SafetyLabelType::NSFW_HIGH_PRECISION)
            || tweet.has_safety_label(SafetyLabelType::NSFW_HIGH_RECALL)
            || (tweet.is_nsfw_flagged() && !tweet.is_retweet()))
}

fn graphic_base_condition(context: &RuleContext<'_>) -> bool {
    let tweet = context.tweet();
    !context.viewer().is_author()
        && tweet.has_media()
        && tweet.has_safety_label(SafetyLabelType::GORE_AND_VIOLENCE_HIGH_PRECISION)
}

fn nsfw_no_media_label_condition(context: &RuleContext<'_>) -> bool {
    let tweet = context.tweet();
    !context.viewer().is_author()
        && (tweet.has_safety_label(SafetyLabelType::NSFW_TEXT)
            || tweet.has_safety_label(SafetyLabelType::NSFW_CARD_IMAGE))
}

fn sensitive_base_condition(context: &RuleContext<'_>) -> bool {
    nsfw_base_condition(context)
        || graphic_base_condition(context)
        || nsfw_no_media_label_condition(context)
}

pub struct SensitiveViewerLoggedOutDropRule;

impl Rule for SensitiveViewerLoggedOutDropRule {
    fn name(&self) -> &'static str {
        "SensitiveViewerLoggedOutDropRule"
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if context.viewer().is_logged_out() && sensitive_base_condition(context) {
            VfAction::Drop(FilteredReason::ContainNsfwMedia)
        } else {
            VfAction::Allow
        }
    }
}

pub struct SensitiveViewerUnderageDropRule;

impl Rule for SensitiveViewerUnderageDropRule {
    fn name(&self) -> &'static str {
        "SensitiveViewerUnderageDropRule"
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if context.viewer().is_underage() && sensitive_base_condition(context) {
            VfAction::Drop(FilteredReason::ContainNsfwMedia)
        } else {
            VfAction::Allow
        }
    }
}

pub struct SensitiveViewerNoStatedAgeDropRule {
    gating_countries: Arc<NsfwGatingCountries>,
}

impl SensitiveViewerNoStatedAgeDropRule {
    pub fn new(gating_countries: Arc<NsfwGatingCountries>) -> Self {
        Self { gating_countries }
    }
}

impl Rule for SensitiveViewerNoStatedAgeDropRule {
    fn name(&self) -> &'static str {
        "SensitiveViewerNoStatedAgeDropRule"
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if context.viewer().has_no_stated_age()
            && context
                .viewer()
                .country()
                .is_some_and(|c| self.gating_countries.contains(c))
            && sensitive_base_condition(context)
        {
            VfAction::Drop(FilteredReason::ContainNsfwMedia)
        } else {
            VfAction::Allow
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        AuthorFeatures, HydratedTweetCandidate, NsfwFeature, Viewer, ViewerAge, ViewerFeatures,
    };
    use crate::rules::fixtures::{candidate, viewer, VIEWER_ID};

    fn gating_viewer(age: ViewerAge) -> ViewerFeatures {
        ViewerFeatures {
            viewer_age: age,
            country_code: Some("de".into()),
            ..viewer(VIEWER_ID)
        }
    }

    fn media_candidate_with_label(label: SafetyLabelType) -> HydratedTweetCandidate {
        candidate().with_label(label).with_media().build()
    }

    fn no_stated_age_rule() -> SensitiveViewerNoStatedAgeDropRule {
        SensitiveViewerNoStatedAgeDropRule::new(Arc::new(NsfwGatingCountries::new()))
    }

    fn nsfw_author_media_candidate() -> HydratedTweetCandidate {
        candidate()
            .with_media()
            .with_author_features(AuthorFeatures {
                is_nsfw_user: true,
                ..Default::default()
            })
            .build()
    }

    #[test]
    fn underage_drops_nsfw_label_media() {
        let c = media_candidate_with_label(SafetyLabelType::NSFW_HIGH_PRECISION);
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn underage_drops_nsfw_high_recall_media() {
        let c = media_candidate_with_label(SafetyLabelType::NSFW_HIGH_RECALL);
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn underage_drops_gore_label_media() {
        let c = media_candidate_with_label(SafetyLabelType::GORE_AND_VIOLENCE_HIGH_PRECISION);
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn underage_drop_even_when_opted_in() {
        let c = media_candidate_with_label(SafetyLabelType::NSFW_HIGH_PRECISION);
        let v = ViewerFeatures {
            allows_sensitive_media: true,
            ..gating_viewer(ViewerAge::Known(15))
        };
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(&v, &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn adult_does_not_drop() {
        let c = media_candidate_with_label(SafetyLabelType::NSFW_HIGH_PRECISION);
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(18)),
                &c
            )),
            VfAction::Allow
        ));
    }

    #[test]
    fn unknown_age_fails_open() {
        let c = media_candidate_with_label(SafetyLabelType::NSFW_HIGH_PRECISION);
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Unknown),
                &c
            )),
            VfAction::Allow
        ));
        assert!(matches!(
            no_stated_age_rule().evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Unknown),
                &c
            )),
            VfAction::Allow
        ));
    }

    fn no_media_candidate_with_label(label: SafetyLabelType) -> HydratedTweetCandidate {
        let mut candidate = media_candidate_with_label(label);
        candidate.tweet_features.media.has_media = false;
        candidate
    }

    #[test]
    fn underage_drops_nsfw_text_without_media() {
        let c = no_media_candidate_with_label(SafetyLabelType::NSFW_TEXT);
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn underage_drops_nsfw_card_image_without_media() {
        let c = no_media_candidate_with_label(SafetyLabelType::NSFW_CARD_IMAGE);
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn no_stated_age_drops_nsfw_text_in_jurisdiction() {
        let c = no_media_candidate_with_label(SafetyLabelType::NSFW_TEXT);
        assert!(matches!(
            no_stated_age_rule().evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::NotStated),
                &c
            )),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn no_stated_age_allows_nsfw_text_outside_jurisdiction() {
        let c = no_media_candidate_with_label(SafetyLabelType::NSFW_TEXT);
        let v = ViewerFeatures {
            country_code: Some("us".into()),
            ..gating_viewer(ViewerAge::NotStated)
        };
        assert!(matches!(
            no_stated_age_rule().evaluate(&crate::rules::test_context(&v, &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn logged_out_drops_nsfw_text_without_media() {
        let c = no_media_candidate_with_label(SafetyLabelType::NSFW_TEXT);
        let v = ViewerFeatures {
            viewer: Viewer::LoggedOut,
            ..gating_viewer(ViewerAge::Unknown)
        };
        assert!(matches!(
            SensitiveViewerLoggedOutDropRule.evaluate(&crate::rules::test_context(&v, &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn logged_out_drops_nsfw_card_image_without_media() {
        let c = no_media_candidate_with_label(SafetyLabelType::NSFW_CARD_IMAGE);
        let v = ViewerFeatures {
            viewer: Viewer::LoggedOut,
            ..gating_viewer(ViewerAge::Unknown)
        };
        assert!(matches!(
            SensitiveViewerLoggedOutDropRule.evaluate(&crate::rules::test_context(&v, &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn adult_allows_nsfw_text() {
        let c = no_media_candidate_with_label(SafetyLabelType::NSFW_TEXT);
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(18)),
                &c
            )),
            VfAction::Allow
        ));
    }

    #[test]
    fn unknown_age_allows_nsfw_text() {
        let c = no_media_candidate_with_label(SafetyLabelType::NSFW_TEXT);
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Unknown),
                &c
            )),
            VfAction::Allow
        ));
        assert!(matches!(
            no_stated_age_rule().evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Unknown),
                &c
            )),
            VfAction::Allow
        ));
    }

    #[test]
    fn nsfw_text_self_view_is_exempt() {
        let mut c = no_media_candidate_with_label(SafetyLabelType::NSFW_TEXT);
        c.author_id = VIEWER_ID;
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Allow
        ));
    }

    #[test]
    fn label_rule_requires_media() {
        let mut c = media_candidate_with_label(SafetyLabelType::NSFW_HIGH_PRECISION);
        c.tweet_features.media.has_media = false;
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Allow
        ));
    }

    #[test]
    fn self_view_is_exempt() {
        let mut c = media_candidate_with_label(SafetyLabelType::NSFW_HIGH_PRECISION);
        c.author_id = VIEWER_ID;
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Allow
        ));
    }

    #[test]
    fn no_stated_age_drops_in_jurisdiction() {
        let c = media_candidate_with_label(SafetyLabelType::NSFW_HIGH_PRECISION);
        assert!(matches!(
            no_stated_age_rule().evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::NotStated),
                &c
            )),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn no_stated_age_allows_outside_jurisdiction() {
        let c = media_candidate_with_label(SafetyLabelType::NSFW_HIGH_PRECISION);
        let v = ViewerFeatures {
            country_code: Some("us".into()),
            ..gating_viewer(ViewerAge::NotStated)
        };
        assert!(matches!(
            no_stated_age_rule().evaluate(&crate::rules::test_context(&v, &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn no_stated_age_allows_missing_country() {
        let c = media_candidate_with_label(SafetyLabelType::NSFW_HIGH_PRECISION);
        let v = ViewerFeatures {
            country_code: None,
            ..gating_viewer(ViewerAge::NotStated)
        };
        assert!(matches!(
            no_stated_age_rule().evaluate(&crate::rules::test_context(&v, &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn no_stated_age_allows_non_gating_account_country_over_gating_request() {
        let c = media_candidate_with_label(SafetyLabelType::NSFW_HIGH_PRECISION);
        let v = ViewerFeatures {
            country_code: Some("de".into()),
            account_country_code: Some("us".into()),
            ..gating_viewer(ViewerAge::NotStated)
        };
        assert!(matches!(
            no_stated_age_rule().evaluate(&crate::rules::test_context(&v, &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn no_stated_age_drops_gating_account_country() {
        let c = media_candidate_with_label(SafetyLabelType::NSFW_HIGH_PRECISION);
        let v = ViewerFeatures {
            country_code: Some("us".into()),
            account_country_code: Some("kr".into()),
            ..gating_viewer(ViewerAge::NotStated)
        };
        assert!(matches!(
            no_stated_age_rule().evaluate(&crate::rules::test_context(&v, &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn no_stated_age_falls_back_to_request_country_when_account_absent() {
        let c = media_candidate_with_label(SafetyLabelType::NSFW_HIGH_PRECISION);
        let v = ViewerFeatures {
            country_code: Some("de".into()),
            account_country_code: None,
            ..gating_viewer(ViewerAge::NotStated)
        };
        assert!(matches!(
            no_stated_age_rule().evaluate(&crate::rules::test_context(&v, &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn underage_drops_nsfw_author_media() {
        let c = nsfw_author_media_candidate();
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn underage_drops_nsfw_admin_author_media() {
        let mut c = nsfw_author_media_candidate();
        c.author_features = AuthorFeatures {
            is_nsfw_user: false,
            is_nsfw_admin: true,
            ..Default::default()
        };
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Drop(_)
        ));
    }

    fn nsfw_tweet_flag_media_candidate() -> HydratedTweetCandidate {
        let mut c = nsfw_author_media_candidate();
        c.author_features = AuthorFeatures::default();
        c.tweet_features.nsfw = NsfwFeature {
            user: true,
            admin: false,
        };
        c
    }

    #[test]
    fn underage_drops_nsfw_tweet_flag_media() {
        let c = nsfw_tweet_flag_media_candidate();
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn underage_drops_nsfw_admin_tweet_flag_media() {
        let mut c = nsfw_tweet_flag_media_candidate();
        c.tweet_features.nsfw = NsfwFeature {
            user: false,
            admin: true,
        };
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn underage_drops_when_both_flag_sources_set() {
        let mut c = nsfw_tweet_flag_media_candidate();
        c.author_features.is_nsfw_user = true;
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn underage_allows_no_flags_no_labels() {
        let mut c = nsfw_tweet_flag_media_candidate();
        c.tweet_features.nsfw = NsfwFeature::default();
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Allow
        ));
    }

    #[test]
    fn nsfw_tweet_flag_retweet_not_dropped() {
        let mut c = nsfw_tweet_flag_media_candidate();
        c.tweet_features.core.source_tweet_id = Some(42);
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Allow
        ));
    }

    #[test]
    fn nsfw_tweet_flag_self_view_exempt() {
        let mut c = nsfw_tweet_flag_media_candidate();
        c.author_id = VIEWER_ID;
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Allow
        ));
    }

    #[test]
    fn nsfw_author_retweet_not_dropped() {
        let mut c = nsfw_author_media_candidate();
        c.tweet_features.core.source_tweet_id = Some(42);
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Allow
        ));
    }

    #[test]
    fn nsfw_author_without_media_not_dropped() {
        let mut c = nsfw_author_media_candidate();
        c.tweet_features.media.has_media = false;
        assert!(matches!(
            SensitiveViewerUnderageDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Allow
        ));
    }

    #[test]
    fn logged_out_drops_nsfw_label_media() {
        let c = media_candidate_with_label(SafetyLabelType::NSFW_HIGH_PRECISION);
        let v = ViewerFeatures {
            viewer: Viewer::LoggedOut,
            ..gating_viewer(ViewerAge::Unknown)
        };
        assert!(matches!(
            SensitiveViewerLoggedOutDropRule.evaluate(&crate::rules::test_context(&v, &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn logged_out_drops_nsfw_author_media() {
        let c = nsfw_author_media_candidate();
        let v = ViewerFeatures {
            viewer: Viewer::LoggedOut,
            ..gating_viewer(ViewerAge::Unknown)
        };
        assert!(matches!(
            SensitiveViewerLoggedOutDropRule.evaluate(&crate::rules::test_context(&v, &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn logged_out_requires_media() {
        let mut c = media_candidate_with_label(SafetyLabelType::NSFW_HIGH_PRECISION);
        c.tweet_features.media.has_media = false;
        let v = ViewerFeatures {
            viewer: Viewer::LoggedOut,
            ..gating_viewer(ViewerAge::Unknown)
        };
        assert!(matches!(
            SensitiveViewerLoggedOutDropRule.evaluate(&crate::rules::test_context(&v, &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn logged_in_not_handled_by_logged_out_rule() {
        let c = media_candidate_with_label(SafetyLabelType::NSFW_HIGH_PRECISION);
        assert!(matches!(
            SensitiveViewerLoggedOutDropRule.evaluate(&crate::rules::test_context(
                &gating_viewer(ViewerAge::Known(15)),
                &c
            )),
            VfAction::Allow
        ));
    }
}

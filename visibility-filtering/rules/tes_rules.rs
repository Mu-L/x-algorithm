use crate::models::VfAction;
use crate::rules::{Rule, RuleContext};
use xai_visibility_filtering::models::FilteredReason;

pub struct DropStaleTweetsRule;

impl Rule for DropStaleTweetsRule {
    fn name(&self) -> &'static str {
        "DropStaleTweetsRule"
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if context.tweet().is_stale() && !context.tweet().is_retweet() {
            return VfAction::Drop(FilteredReason::UnspecifiedReason);
        }
        VfAction::Allow
    }
}

pub struct DropLegalTakendownPostRule;

impl Rule for DropLegalTakendownPostRule {
    fn name(&self) -> &'static str {
        "DropLegalTakendownPostRule"
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if !context.viewer().is_author() && context.takedown().legal_in_viewer_country() {
            return VfAction::Drop(FilteredReason::UnspecifiedReason);
        }
        VfAction::Allow
    }
}

pub struct DropLocalLawsTakendownPostRule;

impl Rule for DropLocalLawsTakendownPostRule {
    fn name(&self) -> &'static str {
        "DropLocalLawsTakendownPostRule"
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if !context.viewer().is_author() && context.takedown().local_laws_in_viewer_country() {
            return VfAction::Drop(FilteredReason::UnspecifiedReason);
        }
        VfAction::Allow
    }
}

pub struct DropTweetsWithGeoRestrictedMediaRule;

impl Rule for DropTweetsWithGeoRestrictedMediaRule {
    fn name(&self) -> &'static str {
        "DropTweetsWithGeoRestrictedMediaRule"
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if context.takedown().media_restricted_in_viewer_country() {
            return VfAction::Drop(FilteredReason::UnspecifiedReason);
        }
        VfAction::Allow
    }
}

pub struct DropTweetsWithDmcaMediaRule;

impl Rule for DropTweetsWithDmcaMediaRule {
    fn name(&self) -> &'static str {
        "DropTweetsWithDmcaMediaRule"
    }

    fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
        if context.tweet().has_dmca_media() {
            return VfAction::Drop(FilteredReason::UnspecifiedReason);
        }
        VfAction::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        HydratedTweetCandidate, MediaFeature, TakedownFeature, TweetFeatures, ViewerFeatures,
    };
    use crate::rules::fixtures::{candidate, viewer, VIEWER_ID};
    use xai_core_entities::entities::{EditControl, EditControlInitial, TakedownReason};

    fn takedown_candidate(reasons: Vec<TakedownReason>) -> HydratedTweetCandidate {
        candidate()
            .with_tweet_features(TweetFeatures {
                takedown: TakedownFeature {
                    reasons,
                    ..Default::default()
                },
                ..Default::default()
            })
            .build()
    }

    fn stale_edit_control() -> Option<EditControl> {
        Some(EditControl::Initial(EditControlInitial {
            edit_tweet_ids: vec![1, 2],
            ..Default::default()
        }))
    }

    #[test]
    fn stale_edit_drops() {
        let rule = DropStaleTweetsRule;
        let c = candidate()
            .with_tweet_features(TweetFeatures {
                edit_control: stale_edit_control(),
                ..Default::default()
            })
            .build();
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn non_stale_tweet_allows() {
        let rule = DropStaleTweetsRule;
        let c = candidate().build();
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn stale_retweet_allows() {
        let rule = DropStaleTweetsRule;
        let c = candidate()
            .with_tweet_features(TweetFeatures {
                edit_control: stale_edit_control(),
                ..Default::default()
            })
            .retweet_of(99)
            .build();
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    fn viewer_with_country(country: &str) -> ViewerFeatures {
        ViewerFeatures {
            country_code: Some(country.to_string()),
            ..viewer(VIEWER_ID)
        }
    }

    #[test]
    fn takedown_drops_in_matching_country() {
        let rule = DropLegalTakendownPostRule;
        let c = takedown_candidate(vec![
            TakedownReason::LegalRequest {
                country_code: "de".to_string(),
            },
            TakedownReason::UnspecifiedReason {
                country_code: "fr".to_string(),
            },
        ]);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer_with_country("de"), &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn takedown_allows_in_non_matching_country() {
        let rule = DropLegalTakendownPostRule;
        let c = takedown_candidate(vec![TakedownReason::LegalRequest {
            country_code: "de".to_string(),
        }]);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer_with_country("us"), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn takedown_allows_when_no_viewer_country() {
        let rule = DropLegalTakendownPostRule;
        let c = takedown_candidate(vec![TakedownReason::LegalRequest {
            country_code: "de".to_string(),
        }]);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn legal_rule_ignores_local_laws_countries() {
        let rule = DropLegalTakendownPostRule;
        let c = takedown_candidate(vec![TakedownReason::BystanderReport {
            country_code: "de".to_string(),
        }]);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer_with_country("de"), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn local_laws_drops_in_matching_country() {
        let rule = DropLocalLawsTakendownPostRule;
        let c = takedown_candidate(vec![
            TakedownReason::BystanderReport {
                country_code: "de".to_string(),
            },
            TakedownReason::BystanderReport {
                country_code: "fr".to_string(),
            },
        ]);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer_with_country("fr"), &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn local_laws_allows_in_non_matching_country() {
        let rule = DropLocalLawsTakendownPostRule;
        let c = takedown_candidate(vec![TakedownReason::BystanderReport {
            country_code: "de".to_string(),
        }]);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer_with_country("us"), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn local_laws_ignores_legal_countries() {
        let rule = DropLocalLawsTakendownPostRule;
        let c = takedown_candidate(vec![TakedownReason::LegalRequest {
            country_code: "de".to_string(),
        }]);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer_with_country("de"), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn legal_allows_author_viewing_own_withheld_post() {
        let rule = DropLegalTakendownPostRule;
        let mut c = takedown_candidate(vec![TakedownReason::LegalRequest {
            country_code: "de".to_string(),
        }]);
        c.author_id = VIEWER_ID;
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer_with_country("de"), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn local_laws_allows_author_viewing_own_withheld_post() {
        let rule = DropLocalLawsTakendownPostRule;
        let mut c = takedown_candidate(vec![TakedownReason::BystanderReport {
            country_code: "de".to_string(),
        }]);
        c.author_id = VIEWER_ID;
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer_with_country("de"), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn takedown_rules_ignore_non_country_reasons() {
        let c = takedown_candidate(vec![
            TakedownReason::Dmca,
            TakedownReason::HatefulImagery,
            TakedownReason::Unknown,
        ]);
        assert!(matches!(
            DropLegalTakendownPostRule
                .evaluate(&crate::rules::test_context(&viewer_with_country("de"), &c)),
            VfAction::Allow
        ));
        assert!(matches!(
            DropLocalLawsTakendownPostRule
                .evaluate(&crate::rules::test_context(&viewer_with_country("de"), &c)),
            VfAction::Allow
        ));
    }

    fn geo_candidate(allow: &[&str], deny: &[&str]) -> HydratedTweetCandidate {
        candidate()
            .with_tweet_features(TweetFeatures {
                media: MediaFeature {
                    geo_allow_list: allow.iter().map(|s| s.to_string()).collect(),
                    geo_deny_list: deny.iter().map(|s| s.to_string()).collect(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .build()
    }

    #[test]
    fn geo_restricted_no_restrictions_allows() {
        let rule = DropTweetsWithGeoRestrictedMediaRule;
        let c = geo_candidate(&[], &[]);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer_with_country("us"), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn geo_restricted_denylist_drops_matching_country() {
        let rule = DropTweetsWithGeoRestrictedMediaRule;
        let c = geo_candidate(&[], &["de", "fr"]);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer_with_country("de"), &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn geo_restricted_denylist_allows_other_country() {
        let rule = DropTweetsWithGeoRestrictedMediaRule;
        let c = geo_candidate(&[], &["de", "fr"]);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer_with_country("us"), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn geo_restricted_allowlist_allows_listed_country() {
        let rule = DropTweetsWithGeoRestrictedMediaRule;
        let c = geo_candidate(&["us", "gb"], &[]);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer_with_country("us"), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn geo_restricted_allowlist_drops_unlisted_country() {
        let rule = DropTweetsWithGeoRestrictedMediaRule;
        let c = geo_candidate(&["us", "gb"], &[]);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer_with_country("de"), &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn geo_restricted_country_matching_is_case_insensitive() {
        let rule = DropTweetsWithGeoRestrictedMediaRule;
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(
                &viewer_with_country("us"),
                &geo_candidate(&["US"], &[])
            )),
            VfAction::Allow
        ));
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(
                &viewer_with_country("de"),
                &geo_candidate(&[], &["DE"])
            )),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn geo_restricted_missing_country_fails_nonempty_allowlist() {
        let rule = DropTweetsWithGeoRestrictedMediaRule;
        let c = geo_candidate(&["us"], &[]);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn geo_restricted_missing_country_matches_xx_denylist() {
        let rule = DropTweetsWithGeoRestrictedMediaRule;
        let c = geo_candidate(&[], &["xx"]);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn geo_restricted_missing_country_not_in_denylist_allows() {
        let rule = DropTweetsWithGeoRestrictedMediaRule;
        let c = geo_candidate(&[], &["de"]);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Allow
        ));
    }

    #[test]
    fn geo_restricted_drops_even_for_author() {
        let rule = DropTweetsWithGeoRestrictedMediaRule;
        let mut c = geo_candidate(&[], &["de"]);
        c.author_id = VIEWER_ID;
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer_with_country("de"), &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn geo_restricted_drops_retweets_too() {
        let rule = DropTweetsWithGeoRestrictedMediaRule;
        let mut c = geo_candidate(&[], &["de"]);
        c.tweet_features.core.source_tweet_id = Some(99);
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer_with_country("de"), &c)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn dmca_drops() {
        let rule = DropTweetsWithDmcaMediaRule;
        let c = candidate()
            .with_tweet_features(TweetFeatures {
                media: MediaFeature {
                    has_dmca_media: true,
                    ..Default::default()
                },
                ..Default::default()
            })
            .build();
        assert!(matches!(
            rule.evaluate(&crate::rules::test_context(&viewer(VIEWER_ID), &c)),
            VfAction::Drop(_)
        ));
    }
}

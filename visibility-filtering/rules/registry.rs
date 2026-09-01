use crate::models::{HydratedTweetCandidate, ViewerFeatures};
use crate::params::NsfwGatingCountries;
use crate::rules::rule_spec::RuleSpec;
use crate::rules::{author_rules, tweet_rules};
use crate::rules::{evaluate_rules, Rule, RuleContext, Verdict};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SafetyLevel {
    FilterAll,
    TimelineHome,
    TimelineHomeRecommendations,
}

impl SafetyLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            SafetyLevel::FilterAll => "filter_all",
            SafetyLevel::TimelineHome => "timeline_home",
            SafetyLevel::TimelineHomeRecommendations => "timeline_home_recommendations",
        }
    }
}

pub struct Policies {
    filter_all: Vec<Box<dyn Rule>>,
    timeline_home: Vec<Box<dyn Rule>>,
    timeline_home_recommendations: Vec<Box<dyn Rule>>,
    nsfw_gating_countries: Arc<NsfwGatingCountries>,
}

impl Policies {
    pub fn new() -> Self {
        Self::with_nsfw_gating_countries(Arc::new(NsfwGatingCountries::new()))
    }

    pub fn with_nsfw_gating_countries(gating_countries: Arc<NsfwGatingCountries>) -> Self {
        Self {
            filter_all: rule_specs(tweet_rules::FILTER_ALL).collect(),
            timeline_home: timeline_home_policy(),
            timeline_home_recommendations: timeline_home_recommendations_policy(),
            nsfw_gating_countries: gating_countries,
        }
    }

    fn select(&self, level: SafetyLevel) -> &[Box<dyn Rule>] {
        match level {
            SafetyLevel::FilterAll => &self.filter_all,
            SafetyLevel::TimelineHome => &self.timeline_home,
            SafetyLevel::TimelineHomeRecommendations => &self.timeline_home_recommendations,
        }
    }

    pub fn evaluate(
        &self,
        level: SafetyLevel,
        viewer: &ViewerFeatures,
        candidate: &HydratedTweetCandidate,
    ) -> Verdict {
        let context = RuleContext::new(level, viewer, candidate, &self.nsfw_gating_countries);
        evaluate_rules(self.select(level), &context)
    }

    #[cfg(test)]
    pub(crate) fn wired_rule_names(&self, level: SafetyLevel) -> Vec<&'static str> {
        self.select(level).iter().map(|rule| rule.name()).collect()
    }

    pub fn rule_counts(&self) -> (usize, usize) {
        (
            self.timeline_home.len(),
            self.timeline_home_recommendations.len(),
        )
    }
}

impl Default for Policies {
    fn default() -> Self {
        Self::new()
    }
}

fn rule_specs(specs: &'static [RuleSpec]) -> impl Iterator<Item = Box<dyn Rule>> {
    specs
        .iter()
        .map(|spec| Box::new(spec.clone()) as Box<dyn Rule>)
}

fn base_home_rules() -> Vec<Box<dyn Rule>> {
    let mut rules: Vec<Box<dyn Rule>> = Vec::new();
    rules.extend(rule_specs(author_rules::AUTHOR_STATE_DROPS));
    rules.extend(rule_specs(author_rules::SOCIALGRAPH_DROPS));
    rules.extend(rule_specs(tweet_rules::TWEET_LABEL_DROPS));
    rules.extend(rule_specs(tweet_rules::NULLCAST_DROP));
    rules.extend(rule_specs(tweet_rules::TES_HOME_DROPS));
    rules.extend(rule_specs(tweet_rules::SENSITIVE_VIEWER_DROPS));
    rules.extend(rule_specs(tweet_rules::EXCLUSIVE_TWEET_DROP));
    rules.extend(rule_specs(tweet_rules::NSFW_MEDIA_INTERSTITIALS));
    rules.extend(rule_specs(tweet_rules::NSFW_AUTHOR_INTERSTITIAL));
    rules
}

fn timeline_home_policy() -> Vec<Box<dyn Rule>> {
    base_home_rules()
}

fn timeline_home_recommendations_policy() -> Vec<Box<dyn Rule>> {
    let mut rules = base_home_rules();
    rules.extend(rule_specs(tweet_rules::RECS_MEDIA_DROPS));
    rules.extend(rule_specs(author_rules::OON_NSFW_AUTHOR_DROPS));
    rules.extend(rule_specs(tweet_rules::OON_TWEET_FLAG_DROPS));
    rules.extend(rule_specs(tweet_rules::OON_TWEET_LABEL_DROPS));
    rules.extend(rule_specs(author_rules::OON_USER_LABEL_DROPS));
    rules
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        HydratedTweetCandidate, MediaFeature, TweetFeatures, VfAction, ViewerFeatures,
    };
    use crate::rules::fixtures::{author_viewer, candidate, viewer, VIEWER_ID};
    use xai_visibility_filtering::models::FilteredReason;

    struct RecommendationsOnlyRule;

    impl Rule for RecommendationsOnlyRule {
        fn name(&self) -> &'static str {
            "RecommendationsOnlyRule"
        }

        fn evaluate(&self, context: &RuleContext<'_>) -> VfAction {
            match context.safety_level() {
                SafetyLevel::TimelineHomeRecommendations => {
                    VfAction::Drop(FilteredReason::UnspecifiedReason)
                }
                SafetyLevel::FilterAll | SafetyLevel::TimelineHome => VfAction::Allow,
            }
        }
    }

    #[test]
    fn policies_evaluate_uses_selected_safety_level_in_context() {
        let policies = Policies {
            filter_all: vec![Box::new(RecommendationsOnlyRule)],
            timeline_home: vec![Box::new(RecommendationsOnlyRule)],
            timeline_home_recommendations: vec![Box::new(RecommendationsOnlyRule)],
            nsfw_gating_countries: Arc::new(NsfwGatingCountries::new()),
        };
        let viewer = ViewerFeatures::default();
        let candidate = HydratedTweetCandidate::default();

        assert!(matches!(
            policies
                .evaluate(SafetyLevel::TimelineHome, &viewer, &candidate)
                .action,
            VfAction::Allow
        ));
        assert!(matches!(
            policies
                .evaluate(
                    SafetyLevel::TimelineHomeRecommendations,
                    &viewer,
                    &candidate
                )
                .action,
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn refreshed_config_country_reaches_the_wired_rule() {
        let gating_countries = Arc::new(NsfwGatingCountries::new());
        let policies = Policies::with_nsfw_gating_countries(Arc::clone(&gating_countries));
        let candidate = candidate()
            .with_label(crate::models::SafetyLabelType::NSFW_HIGH_PRECISION)
            .with_media()
            .build();
        let viewer = ViewerFeatures {
            viewer_age: crate::models::ViewerAge::NotStated,
            country_code: Some("us".into()),
            ..viewer(VIEWER_ID)
        };

        let verdict = policies.evaluate(SafetyLevel::TimelineHome, &viewer, &candidate);
        assert!(!matches!(verdict.action, VfAction::Drop(_)));

        gating_countries.refresh_from(
            &xai_feature_switches::FeatureSwitches::load_string(
                r#"
rust_vf:
  parameters:
    rust_vf_nsfw_gating_countries:
      type: array
      default:
      - "us"
"#,
            )
            .unwrap(),
        );
        let verdict = policies.evaluate(SafetyLevel::TimelineHome, &viewer, &candidate);
        assert!(matches!(verdict.action, VfAction::Drop(_)));
        assert_eq!(
            verdict.decided_by,
            Some("SensitiveViewerNoStatedAgeDropRule")
        );
    }

    #[test]
    fn filter_all_rule_drops_even_self_view() {
        let candidate = candidate().build();
        let viewer = author_viewer();
        let spec = &tweet_rules::FILTER_ALL[0];
        assert!(matches!(
            spec.evaluate(&crate::rules::test_context(&viewer, &candidate)),
            VfAction::Drop(_)
        ));
    }

    #[test]
    fn wired_rule_order_matches_pre_migration_sequence() {
        let policies = Policies::new();
        assert_eq!(
            policies.wired_rule_names(SafetyLevel::FilterAll),
            vec!["FilterAllRule"]
        );
        let home = policies.wired_rule_names(SafetyLevel::TimelineHome);
        assert_eq!(
            home,
            vec![
                "SuspendedAuthorRule",
                "DeactivatedAuthorRule",
                "ErasedAuthorRule",
                "OffboardedAuthorRule",
                "ProtectedAuthorDropRule",
                "ViewerBlocksAuthorRule",
                "ViewerMutesAuthorRule",
                "MutedRetweetsRule",
                "PdnaTweetLabelRule",
                "BounceTweetLabelRule",
                "SpamTweetLabelRule",
                "ForEmergencyUseOnlyDropRule",
                "FosnrHatefulConductDropRule",
                "FosnrViolentSpeechDropRule",
                "FosnrAbuseDropRule",
                "FosnrCivicIntegrityDropRule",
                "NullcastedTweetDropRule",
                "DropStaleTweetsRule",
                "DropLegalTakendownPostRule",
                "DropLocalLawsTakendownPostRule",
                "SensitiveViewerLoggedOutDropRule",
                "SensitiveViewerUnderageDropRule",
                "SensitiveViewerNoStatedAgeDropRule",
                "DropExclusiveTweetContentRule",
                "NsfwHighPrecisionInterstitialRule",
                "GoreAndViolenceInterstitialRule",
                "NsfwCardImageInterstitialRule",
                "NsfwAuthorInterstitialRule",
            ]
        );
        let mut recs = home.clone();
        recs.extend([
            "DropTweetsWithDmcaMediaRule",
            "DropTweetsWithGeoRestrictedMediaRule",
            "DropNsfwUserAuthorRule",
            "DropNsfwAdminAuthorRule",
            "TweetNsfwUserDropRule",
            "TweetNsfwAdminDropRule",
            "NsfwHighRecallDropRule",
            "NsfwHighPrecisionOonDropRule",
            "GoreAndViolenceOonDropRule",
            "NsfwCardImageOonDropRule",
            "DoNotAmplifyOonDropRule",
            "MaliciousUrlOonDropRule",
            "SpamHighRecallDropRule",
            "NsfwTextTweetLabelDropRule",
            "FosnrAbuseInsultsOonDropRule",
            "NsfwHighRecallUserLabelRule",
            "NsfwHighPrecisionUserLabelRule",
            "SpamHighRecallUserLabelRule",
            "CompromisedUserLabelRule",
            "ReadOnlyUserLabelRule",
            "ImpersonationHighPrecisionUserLabelRule",
            "NsfwAvatarImageRule",
            "NsfwBannerImageRule",
            "AbusiveHighRecallRule",
            "NsfwNearPerfectAuthorRule",
            "DoNotAmplifyNonFollowerRule",
        ]);
        assert_eq!(
            policies.wired_rule_names(SafetyLevel::TimelineHomeRecommendations),
            recs
        );
    }

    #[test]
    fn filter_all_policy_drops_pristine_candidate() {
        let policies = Policies::new();
        let candidate = candidate().build();
        let verdict = policies.evaluate(
            SafetyLevel::FilterAll,
            &ViewerFeatures::default(),
            &candidate,
        );
        assert!(matches!(verdict.action, VfAction::Drop(_)));

        let verdict = policies.evaluate(
            SafetyLevel::TimelineHome,
            &ViewerFeatures::default(),
            &candidate,
        );
        assert!(matches!(verdict.action, VfAction::Allow));
    }

    #[test]
    fn dmca_media_drops_recommendations_only() {
        let policies = Policies::new();
        let candidate = candidate()
            .with_tweet_features(TweetFeatures {
                media: MediaFeature {
                    has_dmca_media: true,
                    ..Default::default()
                },
                ..Default::default()
            })
            .build();

        let timeline_home = policies.evaluate(
            SafetyLevel::TimelineHome,
            &ViewerFeatures::default(),
            &candidate,
        );
        assert!(matches!(timeline_home.action, VfAction::Allow));

        let recommendations = policies.evaluate(
            SafetyLevel::TimelineHomeRecommendations,
            &ViewerFeatures::default(),
            &candidate,
        );
        assert!(matches!(recommendations.action, VfAction::Drop(_)));
    }

    #[test]
    fn tweet_nsfw_flag_drops_recommendations_only() {
        use crate::models::NsfwFeature;
        let policies = Policies::new();
        let candidate = candidate()
            .with_tweet_features(TweetFeatures {
                nsfw: NsfwFeature {
                    user: true,
                    admin: false,
                },
                ..Default::default()
            })
            .build();
        let viewer = viewer(VIEWER_ID);

        let timeline_home = policies
            .evaluate(SafetyLevel::TimelineHome, &viewer, &candidate)
            .action;
        assert!(
            matches!(timeline_home, VfAction::Allow),
            "in-network tweet nsfw_user flag should allow, got {timeline_home:?}"
        );

        let recommendations = policies.evaluate(
            SafetyLevel::TimelineHomeRecommendations,
            &viewer,
            &candidate,
        );
        assert!(matches!(recommendations.action, VfAction::Drop(_)));
        assert_eq!(recommendations.decided_by, Some("TweetNsfwUserDropRule"));
    }

    #[test]
    fn nsfw_author_interstitials_in_network_but_drops_oon() {
        use crate::models::AuthorFeatures;
        let policies = Policies::new();
        let candidate = candidate()
            .with_media()
            .with_author_features(AuthorFeatures {
                is_nsfw_user: true,
                ..Default::default()
            })
            .build();
        let viewer = viewer(VIEWER_ID);

        let in_network = policies
            .evaluate(SafetyLevel::TimelineHome, &viewer, &candidate)
            .action;
        assert!(
            matches!(in_network, VfAction::Interstitial(_)),
            "in-network NSFW author should interstitial, got {in_network:?}"
        );

        let oon = policies
            .evaluate(
                SafetyLevel::TimelineHomeRecommendations,
                &viewer,
                &candidate,
            )
            .action;
        assert!(
            matches!(oon, VfAction::Drop(_)),
            "OON NSFW author should drop, got {oon:?}"
        );
    }

    #[test]
    fn egregious_nsfw_does_not_drop() {
        use crate::models::SafetyLabelType;
        use xai_x_thrift::user_labels::LabelValue;
        let policies = Policies::new();

        let tweet_candidate = candidate()
            .with_label(SafetyLabelType::EGREGIOUS_NSFW)
            .build();
        let user_candidate = candidate_with_author_user_label(LabelValue::EGREGIOUS_NSFW, false);
        let viewer = viewer(VIEWER_ID);

        for candidate in [&tweet_candidate, &user_candidate] {
            let in_network = policies
                .evaluate(SafetyLevel::TimelineHome, &viewer, candidate)
                .action;
            assert!(
                matches!(in_network, VfAction::Allow),
                "in-network EgregiousNsfw should allow after rule removal, got {in_network:?}"
            );
            let oon = policies
                .evaluate(SafetyLevel::TimelineHomeRecommendations, &viewer, candidate)
                .action;
            assert!(
                matches!(oon, VfAction::Allow),
                "OON EgregiousNsfw should allow after rule removal, got {oon:?}"
            );
        }
    }

    fn fosnr_candidate(
        label: crate::models::SafetyLabelType,
        follows: bool,
    ) -> HydratedTweetCandidate {
        let mut c = candidate().with_label(label).build();
        c.relationship.viewer_follows_author = follows;
        c
    }

    #[test]
    fn fosnr_labels_drop_non_author_non_follower_on_both_surfaces() {
        use crate::models::SafetyLabelType;
        let policies = Policies::new();
        let viewer = viewer(VIEWER_ID);
        for label in [
            SafetyLabelType::FOSNR_HATEFUL_CONDUCT,
            SafetyLabelType::FOSNR_VIOLENT_SPEECH,
            SafetyLabelType::FOSNR_ABUSE,
            SafetyLabelType::FOSNR_CIVIC_INTEGRITY,
        ] {
            let candidate = fosnr_candidate(label, false);
            for level in [
                SafetyLevel::TimelineHome,
                SafetyLevel::TimelineHomeRecommendations,
            ] {
                let action = policies.evaluate(level, &viewer, &candidate).action;
                assert!(
                    matches!(action, VfAction::Drop(_)),
                    "{label:?} on {level:?} should drop non-follower, got {action:?}"
                );
            }
        }
    }

    #[test]
    fn fosnr_never_drops_author() {
        use crate::models::SafetyLabelType;
        let policies = Policies::new();
        let author = author_viewer();
        for label in [
            SafetyLabelType::FOSNR_HATEFUL_CONDUCT,
            SafetyLabelType::FOSNR_VIOLENT_SPEECH,
            SafetyLabelType::FOSNR_ABUSE,
            SafetyLabelType::FOSNR_CIVIC_INTEGRITY,
            SafetyLabelType::FOSNR_ABUSE_INSULTS,
        ] {
            let candidate = fosnr_candidate(label, false);
            for level in [
                SafetyLevel::TimelineHome,
                SafetyLevel::TimelineHomeRecommendations,
            ] {
                let action = policies.evaluate(level, &author, &candidate).action;
                assert!(
                    matches!(action, VfAction::Allow),
                    "{label:?} on {level:?} should allow author, got {action:?}"
                );
            }
        }
    }

    #[test]
    fn fosnr_abuse_insults_drops_oon_but_allows_in_network() {
        use crate::models::SafetyLabelType;
        let policies = Policies::new();
        let viewer = viewer(VIEWER_ID);
        let author = author_viewer();

        for follows in [true, false] {
            let candidate = fosnr_candidate(SafetyLabelType::FOSNR_ABUSE_INSULTS, follows);
            let in_network = policies
                .evaluate(SafetyLevel::TimelineHome, &viewer, &candidate)
                .action;
            assert!(
                matches!(in_network, VfAction::Allow),
                "in-network FosnrAbuseInsults should allow (follows={follows}), got {in_network:?}"
            );
        }

        let candidate = fosnr_candidate(SafetyLabelType::FOSNR_ABUSE_INSULTS, false);
        let oon = policies
            .evaluate(
                SafetyLevel::TimelineHomeRecommendations,
                &viewer,
                &candidate,
            )
            .action;
        assert!(
            matches!(oon, VfAction::Drop(_)),
            "OON FosnrAbuseInsults should drop non-author, got {oon:?}"
        );

        let oon_author = policies
            .evaluate(
                SafetyLevel::TimelineHomeRecommendations,
                &author,
                &candidate,
            )
            .action;
        assert!(
            matches!(oon_author, VfAction::Allow),
            "OON FosnrAbuseInsults should allow author, got {oon_author:?}"
        );
    }

    #[test]
    fn geo_restricted_media_drops_oon_but_allows_in_network() {
        let policies = Policies::new();
        let candidate = candidate()
            .with_tweet_features(TweetFeatures {
                media: MediaFeature {
                    geo_deny_list: vec!["de".to_string()],
                    ..Default::default()
                },
                ..Default::default()
            })
            .build();
        let viewer = ViewerFeatures {
            country_code: Some("de".to_string()),
            ..viewer(VIEWER_ID)
        };

        let in_network = policies
            .evaluate(SafetyLevel::TimelineHome, &viewer, &candidate)
            .action;
        assert!(
            matches!(in_network, VfAction::Allow),
            "in-network geo-restricted media should allow (Scala wires the rule in THR only), got {in_network:?}"
        );

        let oon = policies.evaluate(
            SafetyLevel::TimelineHomeRecommendations,
            &viewer,
            &candidate,
        );
        assert!(
            matches!(oon.action, VfAction::Drop(_)),
            "OON geo-restricted media should drop, got {:?}",
            oon.action
        );
        assert_eq!(oon.decided_by, Some("DropTweetsWithGeoRestrictedMediaRule"));
    }

    #[test]
    fn nsfw_text_drops_oon_but_allows_in_network() {
        use crate::models::SafetyLabelType;
        let policies = Policies::new();
        let candidate = candidate().with_label(SafetyLabelType::NSFW_TEXT).build();
        let viewer = viewer(VIEWER_ID);

        let in_network = policies
            .evaluate(SafetyLevel::TimelineHome, &viewer, &candidate)
            .action;
        assert!(
            matches!(in_network, VfAction::Allow),
            "in-network NsfwText should allow (Scala drops it OON only), got {in_network:?}"
        );

        let oon = policies
            .evaluate(
                SafetyLevel::TimelineHomeRecommendations,
                &viewer,
                &candidate,
            )
            .action;
        assert!(
            matches!(oon, VfAction::Drop(_)),
            "OON NsfwText should drop, got {oon:?}"
        );
    }

    fn candidate_with_author_user_label(
        label: xai_x_thrift::user_labels::LabelValue,
        follows: bool,
    ) -> HydratedTweetCandidate {
        let mut c = candidate().with_author_user_label(label).build();
        c.relationship.viewer_follows_author = follows;
        c
    }

    #[test]
    fn nsfw_avatar_user_label_drops_oon_but_allows_in_network() {
        use xai_x_thrift::user_labels::LabelValue;
        let policies = Policies::new();
        let candidate = candidate_with_author_user_label(LabelValue::NSFW_AVATAR_IMAGE, false);
        let viewer = viewer(VIEWER_ID);

        let in_network = policies
            .evaluate(SafetyLevel::TimelineHome, &viewer, &candidate)
            .action;
        assert!(
            matches!(in_network, VfAction::Allow),
            "in-network NsfwAvatarImage should allow, got {in_network:?}"
        );

        let oon = policies.evaluate(
            SafetyLevel::TimelineHomeRecommendations,
            &viewer,
            &candidate,
        );
        assert!(
            matches!(oon.action, VfAction::Drop(_)),
            "OON NsfwAvatarImage should drop, got {:?}",
            oon.action
        );
        assert_eq!(oon.decided_by, Some("NsfwAvatarImageRule"));
    }

    #[test]
    fn recommendations_blacklist_does_not_drop() {
        use xai_x_thrift::user_labels::LabelValue;
        let policies = Policies::new();
        let candidate =
            candidate_with_author_user_label(LabelValue::RECOMMENDATIONS_BLACKLIST, false);
        let viewer = viewer(VIEWER_ID);

        let in_network = policies
            .evaluate(SafetyLevel::TimelineHome, &viewer, &candidate)
            .action;
        assert!(
            matches!(in_network, VfAction::Allow),
            "in-network RecommendationsBlacklist should allow, got {in_network:?}"
        );

        let oon = policies
            .evaluate(
                SafetyLevel::TimelineHomeRecommendations,
                &viewer,
                &candidate,
            )
            .action;
        assert!(
            matches!(oon, VfAction::Allow),
            "OON RecommendationsBlacklist should allow after rule removal, got {oon:?}"
        );
    }

    #[test]
    fn abusive_high_recall_allows_follower_on_both_surfaces() {
        use xai_x_thrift::user_labels::LabelValue;
        let policies = Policies::new();
        let candidate = candidate_with_author_user_label(LabelValue::ABUSIVE_HIGH_RECALL, true);
        let viewer = viewer(VIEWER_ID);

        for level in [
            SafetyLevel::TimelineHome,
            SafetyLevel::TimelineHomeRecommendations,
        ] {
            let action = policies.evaluate(level, &viewer, &candidate).action;
            assert!(
                matches!(action, VfAction::Allow),
                "AbusiveHighRecall follower on {level:?} should allow, got {action:?}"
            );
        }
    }

    #[test]
    fn abusive_high_recall_drops_oon_non_follower_but_allows_in_network() {
        use xai_x_thrift::user_labels::LabelValue;
        let policies = Policies::new();
        let candidate = candidate_with_author_user_label(LabelValue::ABUSIVE_HIGH_RECALL, false);
        let viewer = viewer(VIEWER_ID);

        let in_network = policies
            .evaluate(SafetyLevel::TimelineHome, &viewer, &candidate)
            .action;
        assert!(
            matches!(in_network, VfAction::Allow),
            "in-network AbusiveHighRecall should allow, got {in_network:?}"
        );

        let oon = policies.evaluate(
            SafetyLevel::TimelineHomeRecommendations,
            &viewer,
            &candidate,
        );
        assert!(
            matches!(oon.action, VfAction::Drop(_)),
            "OON AbusiveHighRecall non-follower should drop, got {:?}",
            oon.action
        );
        assert_eq!(oon.decided_by, Some("AbusiveHighRecallRule"));
    }
}

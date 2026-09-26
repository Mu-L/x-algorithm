use crate::hydration::{HydrationPlan, Hydrators};
use crate::models::{
    Decided, HydratedTweetCandidate, LimitedEngagement, MediaInterstitial, Verdict, ViewerFeatures,
    Withholding,
};
use crate::params::NsfwGatingCountries;
use crate::rules::rule_spec::{ActionSpec, RuleClause, Truth};
use crate::rules::RuleContext;
use crate::rules::{author_rules, tweet_rules};
use std::sync::Arc;
use strum::VariantArray;

#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::IntoStaticStr, strum::VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum SafetyLevel {
    FilterAll,
    TimelineHome,
    TimelineHomeRecommendations,
    TimelineHomeHydration,
    ImmersiveExpandedRecommendations,
}

pub struct Evaluation {
    pub verdict: Verdict,
    pub rested_on: Hydrators,
}

pub(super) struct Policy<'a> {
    rules: &'a [&'a [RuleClause]],
    additional_rules: &'a [&'a [RuleClause]],
    hydrators: Hydrators,
}

impl<'a> Policy<'a> {
    pub(super) const fn new(rules: &'a [&'a [RuleClause]]) -> Self {
        Self::with_additional(rules, &[])
    }

    const fn with_additional(
        rules: &'a [&'a [RuleClause]],
        additional_rules: &'a [&'a [RuleClause]],
    ) -> Self {
        Self {
            rules,
            additional_rules,
            hydrators: hydrators_of(rules).union(hydrators_of(additional_rules)),
        }
    }

    fn rules(&self) -> impl Iterator<Item = &'a RuleClause> + '_ {
        self.rules
            .iter()
            .chain(self.additional_rules)
            .copied()
            .flatten()
    }

    pub(super) fn evaluate(&self, context: &RuleContext<'_>) -> Evaluation {
        let mut media = None;
        let mut engagement = None;
        let mut withholding_rested_on = Hydrators::empty();
        let mut slot_rested_on = Hydrators::empty();

        for rule in self.rules() {
            let truth = rule.applies(context);
            let unknown_reads = match truth {
                Truth::Unknown { failed, .. } => failed,
                Truth::True | Truth::False => Hydrators::empty(),
            };
            match &rule.action {
                ActionSpec::Drop(reason) => {
                    withholding_rested_on = withholding_rested_on.union(unknown_reads);
                    if truth.resolves_true() {
                        return Evaluation {
                            verdict: Verdict::Withheld(Decided {
                                value: Withholding::Drop(reason.clone()),
                                by: rule.rule_name,
                            }),
                            rested_on: withholding_rested_on,
                        };
                    }
                }
                ActionSpec::Tombstone(reason) => {
                    withholding_rested_on = withholding_rested_on.union(unknown_reads);
                    if truth.resolves_true() {
                        return Evaluation {
                            verdict: Verdict::Withheld(Decided {
                                value: Withholding::Tombstone(*reason),
                                by: rule.rule_name,
                            }),
                            rested_on: withholding_rested_on,
                        };
                    }
                }
                ActionSpec::Interstitial {
                    legacy,
                    media: reason,
                } if media.is_none() => {
                    slot_rested_on = slot_rested_on.union(unknown_reads);
                    if truth.resolves_true() {
                        media = Some(Decided {
                            value: MediaInterstitial {
                                legacy: legacy.clone(),
                                reason: reason.clone(),
                            },
                            by: rule.rule_name,
                        });
                    }
                }
                ActionSpec::LimitedEngagement(reason) if engagement.is_none() => {
                    slot_rested_on = slot_rested_on.union(unknown_reads);
                    if truth.resolves_true() {
                        engagement = Some(Decided {
                            value: LimitedEngagement(*reason),
                            by: rule.rule_name,
                        });
                    }
                }
                ActionSpec::Interstitial { .. } | ActionSpec::LimitedEngagement(_) => {}
            }
        }

        Evaluation {
            verdict: Verdict::Shown { media, engagement },
            rested_on: withholding_rested_on.union(slot_rested_on),
        }
    }

    fn rule_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        let mut previous: Option<&'static str> = None;
        self.rules().filter_map(move |rule| {
            let repeat = previous == Some(rule.rule_name);
            previous = Some(rule.rule_name);
            (!repeat).then_some(rule.rule_name)
        })
    }

    fn len(&self) -> usize {
        self.rule_names().count()
    }
}

const fn hydrators_of(mut groups: &[&[RuleClause]]) -> Hydrators {
    let mut hydrators = Hydrators::empty();
    while let [group, tail @ ..] = groups {
        let mut rules = *group;
        while let [rule, rest @ ..] = rules {
            hydrators = hydrators.union(rule.hydrators());
            rules = rest;
        }
        groups = tail;
    }
    hydrators
}

static FILTER_ALL_POLICY: Policy = Policy::new(&[tweet_rules::FILTER_ALL]);

static TIMELINE_HOME_SHARED_RULES: [&[RuleClause]; 10] = [
    author_rules::AUTHOR_STATE_DROPS,
    author_rules::SOCIALGRAPH_DROPS,
    tweet_rules::TWEET_LABEL_DROPS,
    tweet_rules::NULLCAST_DROP,
    tweet_rules::STALE_TWEET_DROP,
    tweet_rules::TAKEDOWN_DROPS,
    tweet_rules::SENSITIVE_VIEWER_DROPS,
    tweet_rules::EXCLUSIVE_TWEET_DROP,
    tweet_rules::NSFW_MEDIA_INTERSTITIALS,
    tweet_rules::NSFW_AUTHOR_INTERSTITIAL,
];

static TIMELINE_HOME_RECOMMENDATION_ONLY_RULES: [&[RuleClause]; 9] = [
    tweet_rules::RECS_MEDIA_DROPS,
    author_rules::OON_NSFW_AUTHOR_DROPS,
    tweet_rules::OON_TWEET_FLAG_DROPS,
    tweet_rules::OON_GORE_DROP,
    tweet_rules::OON_NSFW_MEDIA_LABEL_DROPS,
    tweet_rules::OON_LOW_QUALITY_TWEET_LABEL_DROPS,
    tweet_rules::OON_TEXT_LABEL_DROPS,
    author_rules::OON_NSFW_USER_LABEL_DROPS,
    author_rules::OON_USER_LABEL_DROPS,
];

static TIMELINE_HOME_POLICY: Policy = Policy::new(&TIMELINE_HOME_SHARED_RULES);
static TIMELINE_HOME_RECOMMENDATIONS_POLICY: Policy = Policy::with_additional(
    &TIMELINE_HOME_SHARED_RULES,
    &TIMELINE_HOME_RECOMMENDATION_ONLY_RULES,
);

static TIMELINE_HOME_HYDRATION_POLICY: Policy = Policy::new(&[
    tweet_rules::TWEET_LABEL_DROPS,
    tweet_rules::EXCLUSIVE_TWEET_DROP,
    tweet_rules::TAKEDOWN_DROPS,
    tweet_rules::SENSITIVE_VIEWER_DROPS,
    tweet_rules::NSFW_MEDIA_INTERSTITIALS,
    tweet_rules::NSFW_AUTHOR_INTERSTITIAL,
    tweet_rules::BLOCKED_VIEWER_LIMITED_ACTIONS,
    tweet_rules::LIMIT_REPLIES_CONVERSATION_RULES,
    tweet_rules::READ_ONLY_VIEWER_LIMITED_ACTIONS,
]);

static IMMERSIVE_EXPANDED_RECOMMENDATIONS_POLICY: Policy = Policy::new(&[
    author_rules::AUTHOR_STATE_DROPS,
    author_rules::SOCIALGRAPH_DROPS,
    tweet_rules::TWEET_LABEL_DROPS,
    tweet_rules::STALE_TWEET_DROP,
    tweet_rules::TAKEDOWN_DROPS,
    tweet_rules::SENSITIVE_VIEWER_DROPS,
    tweet_rules::EXCLUSIVE_TWEET_DROP,
    tweet_rules::RECS_MEDIA_DROPS,
    tweet_rules::OON_GORE_DROP,
    tweet_rules::OON_LOW_QUALITY_TWEET_LABEL_DROPS,
    author_rules::OON_USER_LABEL_DROPS,
    tweet_rules::SENSITIVE_MEDIA_OPT_OUT_DROPS,
]);

pub struct RuleEngine {
    nsfw_gating_countries: Arc<NsfwGatingCountries>,
    plans: Vec<HydrationPlan>,
}

impl RuleEngine {
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self::with_nsfw_gating_countries(Arc::new(NsfwGatingCountries::starting_at_default()))
    }

    pub fn with_nsfw_gating_countries(gating_countries: Arc<NsfwGatingCountries>) -> Self {
        Self {
            nsfw_gating_countries: gating_countries,
            plans: SafetyLevel::VARIANTS
                .iter()
                .map(|&level| HydrationPlan::new(level, Self::select(level).hydrators))
                .collect(),
        }
    }

    fn select(level: SafetyLevel) -> &'static Policy<'static> {
        match level {
            SafetyLevel::FilterAll => &FILTER_ALL_POLICY,
            SafetyLevel::TimelineHome => &TIMELINE_HOME_POLICY,
            SafetyLevel::TimelineHomeRecommendations => &TIMELINE_HOME_RECOMMENDATIONS_POLICY,
            SafetyLevel::TimelineHomeHydration => &TIMELINE_HOME_HYDRATION_POLICY,
            SafetyLevel::ImmersiveExpandedRecommendations => {
                &IMMERSIVE_EXPANDED_RECOMMENDATIONS_POLICY
            }
        }
    }

    pub fn evaluate(
        &self,
        level: SafetyLevel,
        viewer: &ViewerFeatures,
        candidate: &HydratedTweetCandidate,
    ) -> Evaluation {
        let policy = Self::select(level);
        let context = RuleContext::new(viewer, candidate, &self.nsfw_gating_countries);
        #[cfg(test)]
        let context = context.hydrated_by(policy.hydrators);
        policy.evaluate(&context)
    }

    pub(crate) fn plan(&self, level: SafetyLevel) -> &HydrationPlan {
        #[expect(
            clippy::indexing_slicing,
            reason = "`plans` maps `SafetyLevel::VARIANTS`, which is in declaration order"
        )]
        let plan = &self.plans[level as usize];
        debug_assert_eq!(plan.level(), level);
        plan
    }

    #[cfg(test)]
    pub(crate) fn wired_rule_names(&self, level: SafetyLevel) -> Vec<&'static str> {
        Self::select(level).rule_names().collect()
    }

    pub fn rule_counts(&self) -> (usize, usize) {
        (
            TIMELINE_HOME_POLICY.len(),
            TIMELINE_HOME_RECOMMENDATIONS_POLICY.len(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hydration::Hydrator;
    use crate::models::{ViewerAge, ViewerProfile};
    use crate::rules::fixtures::{candidate, viewer, viewer_with_profile, VIEWER_ID};
    use crate::rules::rule_spec::Condition;
    use crate::rules::{holds_narrowed, test_context};
    use std::slice;

    #[test]
    fn refreshed_config_country_reaches_the_wired_rule() {
        let gating_countries = Arc::new(NsfwGatingCountries::starting_at_default());
        let rule_engine = RuleEngine::with_nsfw_gating_countries(Arc::clone(&gating_countries));
        let candidate = candidate()
            .with_label(crate::models::SafetyLabelType::NSFW_HIGH_PRECISION)
            .with_media()
            .build();
        let viewer = ViewerFeatures {
            country_code: Some("us".into()),
            ..viewer_with_profile(ViewerProfile {
                viewer_age: ViewerAge::NotStated,
                ..ViewerProfile::default()
            })
        };

        let verdict = rule_engine
            .evaluate(SafetyLevel::TimelineHome, &viewer, &candidate)
            .verdict;
        assert!(!matches!(verdict, Verdict::Withheld(_)));

        gating_countries.refresh_and_check_drift(
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
            "/nonexistent/rust_vf.yml",
        );
        let verdict = rule_engine
            .evaluate(SafetyLevel::TimelineHome, &viewer, &candidate)
            .verdict;
        assert!(matches!(
            verdict,
            Verdict::Withheld(Decided {
                value: Withholding::Drop(_),
                by: "SensitiveViewerNoStatedAgeDropRule",
            })
        ));
    }

    #[test]
    fn wired_rule_order_is_pinned() {
        let rule_engine = RuleEngine::for_tests();
        assert_eq!(
            rule_engine.wired_rule_names(SafetyLevel::FilterAll),
            vec!["FilterAllRule"]
        );
        let home = rule_engine.wired_rule_names(SafetyLevel::TimelineHome);
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
                "NsfwHighPrecisionAdultInterstitialRule",
                "NsfwHighPrecisionInterstitialRule",
                "GoreAndViolenceInterstitialRule",
                "NsfwCardImageInterstitialRule",
                "NsfwAdminInterstitialRule",
                "NsfwUserInterstitialRule",
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
            "GoreAndViolenceOonDropRule",
            "NsfwHighRecallDropRule",
            "NsfwHighPrecisionOonDropRule",
            "NsfwCardImageOonDropRule",
            "DoNotAmplifyOonDropRule",
            "MaliciousUrlOonDropRule",
            "SpamHighRecallDropRule",
            "FosnrAbuseInsultsOonDropRule",
            "NsfwHighRecallUserLabelRule",
            "NsfwHighPrecisionUserLabelRule",
            "NsfwAvatarImageRule",
            "NsfwBannerImageRule",
            "NsfwNearPerfectAuthorRule",
            "SpamHighRecallUserLabelRule",
            "CompromisedUserLabelRule",
            "ReadOnlyUserLabelRule",
            "ImpersonationHighPrecisionUserLabelRule",
            "AbusiveHighRecallRule",
            "DoNotAmplifyNonFollowerRule",
        ]);
        assert_eq!(
            rule_engine.wired_rule_names(SafetyLevel::TimelineHomeRecommendations),
            recs
        );
    }

    #[test]
    fn home_hydration_wires_only_its_ordered_baseline_rules() {
        assert_eq!(
            RuleEngine::for_tests().wired_rule_names(SafetyLevel::TimelineHomeHydration),
            vec![
                "PdnaTweetLabelRule",
                "BounceTweetLabelRule",
                "SpamTweetLabelRule",
                "ForEmergencyUseOnlyDropRule",
                "FosnrHatefulConductDropRule",
                "FosnrViolentSpeechDropRule",
                "FosnrAbuseDropRule",
                "FosnrCivicIntegrityDropRule",
                "DropExclusiveTweetContentRule",
                "DropLegalTakendownPostRule",
                "DropLocalLawsTakendownPostRule",
                "SensitiveViewerLoggedOutDropRule",
                "SensitiveViewerUnderageDropRule",
                "SensitiveViewerNoStatedAgeDropRule",
                "NsfwHighPrecisionAdultInterstitialRule",
                "NsfwHighPrecisionInterstitialRule",
                "GoreAndViolenceInterstitialRule",
                "NsfwCardImageInterstitialRule",
                "NsfwAdminInterstitialRule",
                "NsfwUserInterstitialRule",
                "BlockedViewerLimitedActionsRule",
                "RootAuthorBlocksViewerLimitedActionsRule",
                "LimitRepliesByInvitationConversationRule",
                "LimitRepliesCommunityConversationRule",
                "LimitRepliesSubscribersConversationRule",
                "LimitRepliesVerifiedConversationRule",
                "LimitRepliesMyNetworkConversationRule",
                "LimitRepliesCoConversationRule",
                "ReadOnlyViewerLimitedActionsRule",
            ]
        );
    }

    #[test]
    fn immersive_expanded_recommendations_wires_only_its_ordered_rules() {
        assert_eq!(
            RuleEngine::for_tests().wired_rule_names(SafetyLevel::ImmersiveExpandedRecommendations),
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
                "DropStaleTweetsRule",
                "DropLegalTakendownPostRule",
                "DropLocalLawsTakendownPostRule",
                "SensitiveViewerLoggedOutDropRule",
                "SensitiveViewerUnderageDropRule",
                "SensitiveViewerNoStatedAgeDropRule",
                "DropExclusiveTweetContentRule",
                "DropTweetsWithDmcaMediaRule",
                "DropTweetsWithGeoRestrictedMediaRule",
                "GoreAndViolenceOonDropRule",
                "DoNotAmplifyOonDropRule",
                "MaliciousUrlOonDropRule",
                "SpamHighRecallDropRule",
                "SpamHighRecallUserLabelRule",
                "CompromisedUserLabelRule",
                "ReadOnlyUserLabelRule",
                "ImpersonationHighPrecisionUserLabelRule",
                "AbusiveHighRecallRule",
                "DoNotAmplifyNonFollowerRule",
                "NsfwSensitiveViewerDropTweetRule",
                "NsfwSensitiveViewerDropUserRule",
            ]
        );
    }

    #[test]
    #[should_panic(expected = "a rule reads Follows")]
    fn a_rule_reading_an_underived_hydrator_panics_in_tests() {
        let viewer = viewer(VIEWER_ID);
        let candidate = candidate().build();
        let context = test_context(&viewer, &candidate)
            .hydrated_by(Hydrators::all().without(Hydrator::Follows));
        context.edge(Hydrator::Follows);
    }

    #[test]
    fn every_leaf_reads_only_the_hydrators_it_declares() {
        let viewer = viewer(VIEWER_ID);
        let candidate = candidate().build();
        for &level in SafetyLevel::VARIANTS {
            for condition in RuleEngine::select(level).rules().flat_map(|rule| rule.when) {
                let leaves = match condition {
                    Condition::Holds(leaf) | Condition::Not(leaf) => slice::from_ref(leaf),
                    Condition::AnyOf(leaves) => leaves,
                };
                for &leaf in leaves {
                    holds_narrowed(leaf, &viewer, &candidate);
                }
            }
        }
    }

    mod engine {
        use super::super::*;
        use crate::hydration::Hydrator;
        use crate::models::{
            HydratedTweetCandidate, LimitedEngagementReason, TombstoneReason, ViewerFeatures,
        };
        use crate::rules::fixtures::{allow, blurred};
        use crate::rules::rule_spec::{
            ActionSpec, Audience, Condition, Predicate, RelationshipPredicate, TweetPredicate,
            ViewerPredicate,
        };
        use crate::rules::test_context;
        use xai_visibility_filtering::models::FilteredReason;
        use xai_x_thrift::action::InterstitialReason;

        const fn always(name: &'static str, action: ActionSpec) -> RuleClause {
            RuleClause {
                rule_name: name,
                when: &[],
                applies_to: Audience::Everyone,
                action,
            }
        }

        const NEVER_LEAF: Predicate = Predicate::Tweet(TweetPredicate::CreatedAfter(u64::MAX));
        const NEVER: Condition = Condition::Holds(NEVER_LEAF);

        const FOLLOWS: Predicate =
            Predicate::Relationship(RelationshipPredicate::ViewerFollowsAuthor);
        const BLOCKS: Predicate =
            Predicate::Relationship(RelationshipPredicate::ViewerBlocksAuthor);
        const LOGGED_OUT: Predicate = Predicate::Viewer(ViewerPredicate::LoggedOut);

        const UNREACHABLE: Condition = Condition::Holds(FOLLOWS);

        const DROP_SUSPENDED: ActionSpec = ActionSpec::Drop(FilteredReason::AuthorIsSuspended);
        const TOMBSTONE: ActionSpec = ActionSpec::Tombstone(TombstoneReason::LocalRegulations);
        const INTERSTITIAL_NSFW: ActionSpec = ActionSpec::Interstitial {
            legacy: FilteredReason::ContainNsfwMedia,
            media: InterstitialReason::Sensitive(true),
        };
        const INTERSTITIAL_UNSPECIFIED: ActionSpec = ActionSpec::Interstitial {
            legacy: FilteredReason::UnspecifiedReason,
            media: InterstitialReason::Nudity(true),
        };
        const LIMIT: ActionSpec =
            ActionSpec::LimitedEngagement(LimitedEngagementReason::ConversationControl);

        fn context_inputs() -> (ViewerFeatures, HydratedTweetCandidate) {
            (ViewerFeatures::default(), HydratedTweetCandidate::default())
        }

        fn withheld(value: Withholding, by: &'static str) -> Verdict {
            Verdict::Withheld(Decided { value, by })
        }

        static SHORT_CIRCUIT_ROWS: [RuleClause; 3] = [
            RuleClause {
                rule_name: "allow",
                when: &[NEVER],
                applies_to: Audience::Everyone,
                action: DROP_SUSPENDED,
            },
            always("drop", DROP_SUSPENDED),
            RuleClause {
                rule_name: "after_drop",
                when: &[UNREACHABLE],
                applies_to: Audience::Everyone,
                action: TOMBSTONE,
            },
        ];
        static SHORT_CIRCUIT: Policy = Policy::new(&[&SHORT_CIRCUIT_ROWS]);

        static TOMBSTONE_FIRST_ROWS: [RuleClause; 2] = [
            always("tombstone", TOMBSTONE),
            always("drop", DROP_SUSPENDED),
        ];
        static TOMBSTONE_FIRST: Policy = Policy::new(&[&TOMBSTONE_FIRST_ROWS]);

        static RESTRICTION_ROWS: [RuleClause; 4] = [
            always("first_interstitial", INTERSTITIAL_NSFW),
            always("first_limit", LIMIT),
            always("second_interstitial", INTERSTITIAL_UNSPECIFIED),
            always("second_limit", LIMIT),
        ];
        static RESTRICTIONS: Policy = Policy::new(&[&RESTRICTION_ROWS]);

        #[test]
        fn first_terminal_returns_before_later_rules() {
            let (viewer, candidate) = context_inputs();
            let context = test_context(&viewer, &candidate)
                .hydrated_by(Hydrators::all().without(Hydrator::Follows));

            assert_eq!(
                SHORT_CIRCUIT.evaluate(&context).verdict,
                withheld(Withholding::Drop(FilteredReason::AuthorIsSuspended), "drop")
            );
            assert_eq!(
                TOMBSTONE_FIRST.evaluate(&context).verdict,
                withheld(
                    Withholding::Tombstone(TombstoneReason::LocalRegulations),
                    "tombstone"
                )
            );
        }

        #[test]
        fn each_slot_keeps_its_first_restriction() {
            let (viewer, candidate) = context_inputs();

            let verdict = RESTRICTIONS
                .evaluate(&test_context(&viewer, &candidate))
                .verdict;

            assert_eq!(
                verdict,
                Verdict::Shown {
                    media: Some(Decided {
                        value: MediaInterstitial {
                            legacy: FilteredReason::ContainNsfwMedia,
                            reason: InterstitialReason::Sensitive(true),
                        },
                        by: "first_interstitial",
                    }),
                    engagement: Some(Decided {
                        value: LimitedEngagement(LimitedEngagementReason::ConversationControl),
                        by: "first_limit",
                    }),
                }
            );
        }

        fn follows_and_blocks_failed() -> (ViewerFeatures, HydratedTweetCandidate) {
            let candidate = HydratedTweetCandidate {
                failed: Hydrators::of(Hydrator::Follows).with(Hydrator::Blocks),
                ..HydratedTweetCandidate::default()
            };
            (ViewerFeatures::default(), candidate)
        }

        #[test]
        fn clauses_combine_in_three_values_and_unknown_resolves_to_the_default() {
            let (viewer, candidate) = follows_and_blocks_failed();
            let follows = Hydrators::of(Hydrator::Follows);
            let dropped = withheld(Withholding::Drop(FilteredReason::AuthorIsSuspended), "rule");
            let rows: [(&[Condition], Verdict, Hydrators); 6] = [
                (&[Condition::Holds(FOLLOWS)], allow(), follows),
                (
                    &[Condition::Holds(FOLLOWS), NEVER],
                    allow(),
                    Hydrators::empty(),
                ),
                (&[Condition::Not(FOLLOWS)], dropped.clone(), follows),
                (
                    &[Condition::AnyOf(&[FOLLOWS, LOGGED_OUT])],
                    dropped,
                    Hydrators::empty(),
                ),
                (
                    &[Condition::AnyOf(&[FOLLOWS, NEVER_LEAF])],
                    allow(),
                    follows,
                ),
                (
                    &[
                        Condition::AnyOf(&[BLOCKS, LOGGED_OUT]),
                        Condition::Holds(FOLLOWS),
                    ],
                    allow(),
                    follows,
                ),
            ];
            for (index, (when, verdict, rested_on)) in rows.into_iter().enumerate() {
                let rules = [RuleClause {
                    rule_name: "rule",
                    when,
                    applies_to: Audience::Everyone,
                    action: DROP_SUSPENDED,
                }];
                let evaluation =
                    Policy::new(&[&rules]).evaluate(&test_context(&viewer, &candidate));
                assert_eq!(
                    (evaluation.verdict, evaluation.rested_on),
                    (verdict, rested_on),
                    "row {index}"
                );
            }
        }

        #[test]
        fn a_verdict_rests_on_the_unknown_clauses_that_could_have_changed_it() {
            let (viewer, candidate) = follows_and_blocks_failed();
            let follows = Hydrators::of(Hydrator::Follows);
            let unknown = |action| RuleClause {
                rule_name: "unknown",
                when: &[Condition::Holds(FOLLOWS)],
                applies_to: Audience::Everyone,
                action,
            };
            let blur = blurred(InterstitialReason::Sensitive(true), "blur");
            let rows = [
                (
                    [unknown(DROP_SUSPENDED), always("drop", DROP_SUSPENDED)],
                    withheld(Withholding::Drop(FilteredReason::AuthorIsSuspended), "drop"),
                    follows,
                ),
                (
                    [unknown(INTERSTITIAL_NSFW), always("drop", DROP_SUSPENDED)],
                    withheld(Withholding::Drop(FilteredReason::AuthorIsSuspended), "drop"),
                    Hydrators::empty(),
                ),
                (
                    [
                        always("blur", INTERSTITIAL_NSFW),
                        unknown(INTERSTITIAL_UNSPECIFIED),
                    ],
                    blur.clone(),
                    Hydrators::empty(),
                ),
                (
                    [always("blur", INTERSTITIAL_NSFW), unknown(LIMIT)],
                    blur,
                    follows,
                ),
            ];
            for (index, (rules, verdict, rested_on)) in rows.into_iter().enumerate() {
                let evaluation =
                    Policy::new(&[&rules]).evaluate(&test_context(&viewer, &candidate));
                assert_eq!(
                    (evaluation.verdict, evaluation.rested_on),
                    (verdict, rested_on),
                    "row {index}"
                );
            }
        }
    }
}

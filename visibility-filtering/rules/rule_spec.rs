use crate::hydration::{Hydrator, Hydrators};
use crate::models::region::allows_country;
use crate::models::{
    AuthorLabel, LimitedEngagementReason, SafetyLabelType, TombstoneReason, ViewerProfile,
};
use crate::rules::context::CoreFacts;
use crate::rules::RuleContext;
use std::ops::Not;
use xai_core_entities::entities::ConversationControlArm;
use xai_visibility_filtering::models::FilteredReason;
use xai_x_thrift::action::InterstitialReason;

pub(super) struct RuleClause {
    pub(super) rule_name: &'static str,
    pub(super) when: &'static [Condition],
    pub(super) applies_to: Audience,
    pub(super) action: ActionSpec,
}

pub(super) enum Condition {
    Holds(Predicate),
    Not(Predicate),
    AnyOf(&'static [Predicate]),
}

#[derive(Clone, Copy)]
pub(super) enum Predicate {
    Tweet(TweetPredicate),
    Author(AuthorPredicate),
    Viewer(ViewerPredicate),
    Relationship(RelationshipPredicate),
}

#[derive(Clone, Copy)]
pub(super) enum TweetPredicate {
    HasSafetyLabel(SafetyLabelType),
    CreatedAfter(u64),
    NsfwUserFlag,
    NsfwAdminFlag,
    HasMedia,
    HasDmcaMedia,
    IsRetweet,
    IsSupersededEdit,
    LegalTakedownInRequestCountry,
    LocalLawsTakedownInRequestCountry,
    MediaGeoRestrictedInRequestCountry,
    IsNullcast,
    IsCommunityTweet,
    HasExclusiveContent,
    HasConversationControl(ConversationControlArm),
}

#[derive(Clone, Copy)]
pub(super) enum AuthorPredicate {
    HasUserLabel(AuthorLabel),
    IsSuspended,
    IsDeactivated,
    IsErased,
    IsOffboarded,
    IsProtected,
    IsNsfwUser,
    IsNsfwAdmin,
}

#[derive(Clone, Copy)]
pub(super) enum ViewerPredicate {
    LoggedOut,
    Underage,
    NoStatedAge,
    AllowsSensitiveMedia,
    InNsfwGatingCountry,
    HasVerifiedBadge,
    ReadOnly,
}

#[derive(Clone, Copy)]
#[expect(
    clippy::enum_variant_names,
    reason = "the Viewer prefix identifies the acting subject of each relationship"
)]
pub(super) enum RelationshipPredicate {
    ViewerFollowsAuthor,
    ViewerBlocksAuthor,
    ViewerMutesAuthor,
    ViewerMutesRetweetsFromAuthor,
    ViewerIsConversationAuthor,
    ViewerSuperFollowsAuthor,
    ViewerIsConversationRootAuthor,
    ViewerIsInvitedToConversation,
    ViewerIsFollowedByConversationRootAuthor,
    ViewerIsInConversationRootAuthorNetwork,
    ViewerSuperFollowsConversationRootAuthor,
    ViewerIsBlockedByAuthor,
    ViewerIsBlockedByConversationRootAuthor,
    ViewerIsInAllowedCountry,
}

#[derive(Clone, Copy)]
pub(super) enum Audience {
    Everyone,
    ExceptAuthor,
}

pub(super) enum ActionSpec {
    Drop(FilteredReason),
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "constructed once a policy has a Tombstone clause")
    )]
    Tombstone(TombstoneReason),
    Interstitial {
        legacy: FilteredReason,
        media: InterstitialReason,
    },
    LimitedEngagement(LimitedEngagementReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Truth {
    True,
    False,
    Unknown { default: bool, failed: Hydrators },
}

impl Truth {
    #[inline]
    pub(super) fn resolves_true(self) -> bool {
        matches!(self, Truth::True | Truth::Unknown { default: true, .. })
    }

    #[inline]
    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Truth::False, _) | (_, Truth::False) => Truth::False,
            (Truth::True, other) | (other, Truth::True) => other,
            (
                Truth::Unknown {
                    default: a,
                    failed: x,
                },
                Truth::Unknown {
                    default: b,
                    failed: y,
                },
            ) => Truth::Unknown {
                default: a && b,
                failed: x.union(y),
            },
        }
    }

    #[inline]
    fn or(self, other: Self) -> Self {
        match (self, other) {
            (Truth::True, _) | (_, Truth::True) => Truth::True,
            (Truth::False, other) | (other, Truth::False) => other,
            (
                Truth::Unknown {
                    default: a,
                    failed: x,
                },
                Truth::Unknown {
                    default: b,
                    failed: y,
                },
            ) => Truth::Unknown {
                default: a || b,
                failed: x.union(y),
            },
        }
    }
}

impl Not for Truth {
    type Output = Self;

    #[inline]
    fn not(self) -> Self {
        match self {
            Truth::True => Truth::False,
            Truth::False => Truth::True,
            Truth::Unknown { default, failed } => Truth::Unknown {
                default: !default,
                failed,
            },
        }
    }
}

impl RuleClause {
    pub(super) fn applies(&self, context: &RuleContext<'_>) -> Truth {
        if !self.applies_to.admits(context.facts()) {
            return Truth::False;
        }
        let mut truth = Truth::True;
        for condition in self.when {
            truth = truth.and(condition.truth(context));
            if truth == Truth::False {
                break;
            }
        }
        truth
    }

    pub(super) const fn hydrators(&self) -> Hydrators {
        let mut hydrators = Hydrators::empty();
        let mut rest = self.when;
        while let [condition, tail @ ..] = rest {
            hydrators = hydrators.union(condition.hydrators());
            rest = tail;
        }
        hydrators
    }
}

impl Condition {
    pub(super) const fn hydrators(&self) -> Hydrators {
        match self {
            Condition::Holds(leaf) | Condition::Not(leaf) => leaf.hydrators(),
            Condition::AnyOf(leaves) => {
                let mut hydrators = Hydrators::empty();
                let mut rest = *leaves;
                while let [leaf, tail @ ..] = rest {
                    hydrators = hydrators.union(leaf.hydrators());
                    rest = tail;
                }
                hydrators
            }
        }
    }
}

impl Predicate {
    pub(super) const fn hydrators(self) -> Hydrators {
        match self {
            Predicate::Tweet(fact) => fact.hydrators(),
            Predicate::Author(fact) => fact.hydrators(),
            Predicate::Viewer(fact) => fact.hydrators(),
            Predicate::Relationship(fact) => fact.hydrators(),
        }
    }
}

impl Audience {
    #[inline]
    pub(super) fn admits(self, facts: CoreFacts<'_>) -> bool {
        match self {
            Audience::Everyone => true,
            Audience::ExceptAuthor => !facts.is_author_viewer(),
        }
    }
}

impl Condition {
    #[inline]
    fn truth(&self, context: &RuleContext<'_>) -> Truth {
        match self {
            Condition::Holds(leaf) => leaf.truth(context),
            Condition::Not(leaf) => !leaf.truth(context),
            Condition::AnyOf(leaves) => {
                let mut truth = Truth::False;
                for leaf in *leaves {
                    truth = truth.or(leaf.truth(context));
                    if truth == Truth::True {
                        break;
                    }
                }
                truth
            }
        }
    }
}

impl Predicate {
    #[inline]
    fn truth(self, context: &RuleContext<'_>) -> Truth {
        let value = self.holds(context);
        let failed = context.failed();
        let failed = if failed.is_empty() {
            failed
        } else {
            failed.intersection(self.hydrators())
        };
        match (failed.is_empty(), value) {
            (false, default) => Truth::Unknown { default, failed },
            (true, true) => Truth::True,
            (true, false) => Truth::False,
        }
    }

    #[inline]
    pub(super) fn holds(self, context: &RuleContext<'_>) -> bool {
        match self {
            Predicate::Tweet(fact) => fact.holds(context),
            Predicate::Author(fact) => fact.holds(context),
            Predicate::Viewer(fact) => fact.holds(context),
            Predicate::Relationship(fact) => fact.holds(context),
        }
    }
}

macro_rules! predicates {
    ($(
        $predicate:ident {
            $($variant:ident $(($($arg:ident),*))? reads $reads:tt
                => |$facts:pat_param, $value:pat_param| $body:expr),+ $(,)?
        }
    )+) => {$(
        impl $predicate {
            const fn hydrators(self) -> Hydrators {
                match self {
                    $(Self::$variant { .. } => predicates!(@declare $reads),)+
                }
            }

            #[inline]
            fn holds(self, context: &RuleContext<'_>) -> bool {
                match self {
                    $(Self::$variant $(($($arg),*))? => {
                        let $facts = context.facts();
                        let $value = predicates!(@read context $reads);
                        $body
                    })+
                }
            }
        }
    )+};
    (@declare ()) => { Hydrators::empty() };
    (@declare ($($node:ident),+)) => { Hydrators::empty()$(.with(Hydrator::$node))+ };
    (@declare $node:ident) => { Hydrators::of(Hydrator::$node) };
    (@read $context:ident ()) => { () };
    (@read $context:ident ($($node:ident),+)) => { ($(predicates!(@read $context $node)),+) };
    (@read $context:ident Tweet) => { $context.tweet_features() };
    (@read $context:ident ConversationControl) => { $context.conversation_control() };
    (@read $context:ident TweetSafetyLabels) => { $context.tweet_safety_labels() };
    (@read $context:ident ViewerProfile) => { $context.viewer_profile() };
    (@read $context:ident AuthorSafety) => { $context.author_features() };
    (@read $context:ident AuthorLabels) => { $context.author_labels() };
    (@read $context:ident ViewerCountry) => { $context.viewer_country() };
    (@read $context:ident $edge:ident) => {{
        const { assert!(Hydrator::$edge.is_edge()) };
        $context.edge(Hydrator::$edge)
    }};
}

predicates! {
    TweetPredicate {
        HasSafetyLabel(label) reads TweetSafetyLabels => |_, labels| labels.has_label(label),
        CreatedAfter(unix_ms) reads () => |facts, ()| facts.created_after(unix_ms),
        NsfwUserFlag reads Tweet => |_, tweet| tweet.nsfw.user,
        NsfwAdminFlag reads Tweet => |_, tweet| tweet.nsfw.admin,
        HasMedia reads Tweet => |_, tweet| tweet.has_media(),
        HasDmcaMedia reads Tweet => |_, tweet| tweet.has_dmca_media(),
        IsRetweet reads Tweet => |_, tweet| tweet.is_retweet(),
        IsSupersededEdit reads Tweet => |facts, tweet| tweet.is_superseded_edit(facts.tweet_id()),
        LegalTakedownInRequestCountry reads Tweet
            => |facts, tweet| tweet.legal_takedown_in(facts.request_country()),
        LocalLawsTakedownInRequestCountry reads Tweet
            => |facts, tweet| tweet.local_laws_takedown_in(facts.request_country()),
        MediaGeoRestrictedInRequestCountry reads Tweet
            => |facts, tweet| tweet.media_restricted_in(facts.request_country()),
        IsNullcast reads Tweet => |_, tweet| tweet.is_nullcast,
        IsCommunityTweet reads Tweet => |_, tweet| tweet.is_community_tweet,
        HasExclusiveContent reads Tweet
            => |_, tweet| tweet.exclusive_conversation_author_id.is_some(),
        HasConversationControl(arm) reads ConversationControl
            => |_, control| control.is_some_and(|control| control.arm == arm),
    }

    AuthorPredicate {
        HasUserLabel(label) reads AuthorLabels => |_, labels| labels.has_label(label),
        IsSuspended reads AuthorSafety => |_, author| author.is_suspended,
        IsDeactivated reads AuthorSafety => |_, author| author.is_deactivated,
        IsErased reads AuthorSafety => |_, author| author.is_erased,
        IsOffboarded reads AuthorSafety => |_, author| author.is_offboarded,
        IsProtected reads AuthorSafety => |_, author| author.is_protected,
        IsNsfwUser reads AuthorSafety => |_, author| author.is_nsfw_user,
        IsNsfwAdmin reads AuthorSafety => |_, author| author.is_nsfw_admin,
    }

    ViewerPredicate {
        LoggedOut reads () => |facts, ()| facts.viewer_id().is_none(),
        Underage reads ViewerProfile => |_, profile| profile.is_some_and(ViewerProfile::is_underage),
        NoStatedAge reads ViewerProfile
            => |_, profile| profile.is_some_and(ViewerProfile::has_no_stated_age),
        AllowsSensitiveMedia reads ViewerProfile
            => |_, profile| profile.is_some_and(|profile| profile.allows_sensitive_media),
        InNsfwGatingCountry reads ViewerProfile => |facts, profile| {
            profile
                .and_then(|profile| profile.account_country_code.as_deref())
                .or(facts.request_country())
                .is_some_and(|country| facts.nsfw_gating_country(country))
        },
        HasVerifiedBadge reads ViewerProfile
            => |_, profile| profile.is_some_and(|profile| profile.has_verified_badge),
        ReadOnly reads ViewerProfile
            => |_, profile| profile.is_some_and(|profile| profile.is_read_only),
    }

    RelationshipPredicate {
        ViewerFollowsAuthor reads Follows => |_, follows| follows,
        ViewerBlocksAuthor reads Blocks => |_, blocks| blocks,
        ViewerMutesAuthor reads Mutes => |_, mutes| mutes,
        ViewerMutesRetweetsFromAuthor reads MuteRetweets => |_, mutes| mutes,
        ViewerIsConversationAuthor reads Tweet => |facts, tweet| {
            tweet
                .exclusive_conversation_author_id
                .is_some_and(|author| facts.viewer_id() == Some(author))
        },
        ViewerSuperFollowsAuthor reads (Tweet, SuperFollowsExclusive)
            => |_, (tweet, super_follows)| {
                tweet.exclusive_conversation_author_id.is_some() && super_follows
            },
        ViewerIsConversationRootAuthor reads ConversationControl => |facts, control| {
            control
                .zip(facts.viewer_id())
                .is_some_and(|(control, viewer_id)| viewer_id == control.conversation_tweet_author_id)
        },
        ViewerIsInvitedToConversation reads ConversationControl => |facts, control| {
            control
                .zip(facts.viewer_id())
                .is_some_and(|(control, viewer_id)| control.invited_user_ids.contains(&viewer_id))
        },
        ViewerIsFollowedByConversationRootAuthor reads RootFollowsViewer
            => |_, follows| follows,
        ViewerIsInConversationRootAuthorNetwork reads (RootFollowsViewer, RootFollowsViewerSecondDegree)
            => |_, (first, second)| first || second,
        ViewerSuperFollowsConversationRootAuthor reads SuperFollowsRoot
            => |_, super_follows| super_follows,
        ViewerIsBlockedByAuthor reads BlockedByAuthor => |_, blocked| blocked,
        ViewerIsBlockedByConversationRootAuthor reads BlockedByReplyRoot => |_, blocked| blocked,
        ViewerIsInAllowedCountry reads (ConversationControl, ViewerCountry)
            => |_, (control, country)| {
                control.zip(country).is_some_and(|(control, country)| {
                    allows_country(&control.allowed_country_codes, country)
                })
            },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ConversationControlFeatures, HydratedTweetCandidate, ViewerFeatures};
    use crate::rules::fixtures::{candidate, logged_out_viewer, viewer, VIEWER_ID};
    use crate::rules::{holds_narrowed, test_context};
    use xai_core_entities::entities::ConversationControl;

    const ROOT_AUTHOR_ID: u64 = 4242;

    fn controlled(
        arm: ConversationControlArm,
        invited_user_ids: Vec<u64>,
        edges: &[Hydrator],
    ) -> HydratedTweetCandidate {
        edges
            .iter()
            .fold(candidate(), |candidate, &edge| candidate.with_edge(edge))
            .with_conversation_control(ConversationControlFeatures {
                control: ConversationControl {
                    arm,
                    conversation_tweet_author_id: ROOT_AUTHOR_ID,
                    invited_user_ids,
                    invite_via_mention: None,
                    allowed_country_codes: vec![],
                },
                viewer_country: None,
            })
            .build()
    }

    #[test]
    fn conversation_control_predicates_read_the_root_keyed_features() {
        use ConversationControlArm::{ByInvitation, Community, Subscribers};
        use RelationshipPredicate::{
            ViewerIsConversationRootAuthor, ViewerIsFollowedByConversationRootAuthor,
            ViewerIsInvitedToConversation, ViewerSuperFollowsConversationRootAuthor,
        };
        let community = controlled(Community, vec![], &[Hydrator::RootFollowsViewer]);
        let subscribers = controlled(Subscribers, vec![], &[Hydrator::SuperFollowsRoot]);
        let invitation = controlled(ByInvitation, vec![VIEWER_ID], &[]);
        let unrelated = controlled(Community, vec![], &[]);
        let uncontrolled = candidate().build();
        let root_author = viewer(ROOT_AUTHOR_ID);
        let viewer = viewer(VIEWER_ID);
        let logged_out = logged_out_viewer();
        for (predicate, viewer, candidate, expected) in [
            (
                Predicate::Tweet(TweetPredicate::HasConversationControl(Community)),
                &viewer,
                &community,
                true,
            ),
            (
                Predicate::Tweet(TweetPredicate::HasConversationControl(Community)),
                &viewer,
                &uncontrolled,
                false,
            ),
            (
                Predicate::Relationship(ViewerIsConversationRootAuthor),
                &root_author,
                &community,
                true,
            ),
            (
                Predicate::Relationship(ViewerIsConversationRootAuthor),
                &logged_out,
                &community,
                false,
            ),
            (
                Predicate::Relationship(ViewerIsInvitedToConversation),
                &viewer,
                &invitation,
                true,
            ),
            (
                Predicate::Relationship(ViewerIsFollowedByConversationRootAuthor),
                &viewer,
                &community,
                true,
            ),
            (
                Predicate::Relationship(ViewerIsFollowedByConversationRootAuthor),
                &viewer,
                &unrelated,
                false,
            ),
            (
                Predicate::Relationship(ViewerSuperFollowsConversationRootAuthor),
                &viewer,
                &subscribers,
                true,
            ),
            (
                Predicate::Relationship(ViewerSuperFollowsConversationRootAuthor),
                &viewer,
                &unrelated,
                false,
            ),
            (
                Predicate::Relationship(ViewerSuperFollowsConversationRootAuthor),
                &viewer,
                &uncontrolled,
                false,
            ),
        ] {
            assert_eq!(holds_narrowed(predicate, viewer, candidate), expected);
        }
    }

    #[test]
    fn created_after_is_strict_at_the_unix_millisecond_boundary() {
        const CUTOFF_MS: u64 = 1705536000000;
        const AT_CUTOFF: u64 = (CUTOFF_MS - 1288834974657) << 22;
        let viewer = ViewerFeatures::default();
        for (tweet_id, expected) in [
            (AT_CUTOFF - 1, false),
            (AT_CUTOFF, false),
            (AT_CUTOFF + (1 << 22) - 1, false),
            (AT_CUTOFF + (1 << 22), true),
            (0, false),
        ] {
            let candidate = HydratedTweetCandidate {
                tweet_id,
                ..Default::default()
            };
            let context = test_context(&viewer, &candidate);
            assert_eq!(
                TweetPredicate::CreatedAfter(CUTOFF_MS).holds(&context),
                expected,
                "{tweet_id}"
            );
        }
    }
}

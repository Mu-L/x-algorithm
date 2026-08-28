use crate::models::{HydratedTweetCandidate, SafetyLabelType, ViewerFeatures};
use crate::rules::registry::SafetyLevel;
use xai_core_entities::entities::TakedownReason;
use xai_x_thrift::user_labels::LabelValue;

pub struct RuleContext<'a> {
    safety_level: SafetyLevel,
    viewer: &'a ViewerFeatures,
    candidate: &'a HydratedTweetCandidate,
}

impl<'a> RuleContext<'a> {
    pub(super) fn new(
        safety_level: SafetyLevel,
        viewer: &'a ViewerFeatures,
        candidate: &'a HydratedTweetCandidate,
    ) -> Self {
        Self {
            safety_level,
            viewer,
            candidate,
        }
    }

    pub fn safety_level(&self) -> SafetyLevel {
        self.safety_level
    }

    pub fn viewer_is_logged_out(&self) -> bool {
        self.viewer.viewer_is_logged_out()
    }

    pub fn viewer_is_underage(&self) -> bool {
        self.viewer.viewer_is_underage()
    }

    pub fn viewer_has_no_stated_age(&self) -> bool {
        self.viewer.viewer_has_no_stated_age()
    }

    pub fn viewer_allows_sensitive_media(&self) -> bool {
        self.viewer.allows_sensitive_media
    }

    pub fn viewer_country_in(&self, countries: &[&str]) -> bool {
        self.viewer
            .account_country_code
            .as_deref()
            .or(self.viewer.country_code.as_deref())
            .is_some_and(|c| countries.contains(&c))
    }

    pub fn is_author_viewer(&self) -> bool {
        self.candidate.is_author_viewer(self.viewer.viewer)
    }

    pub fn viewer_follows_author(&self) -> bool {
        self.candidate.viewer_follows_author()
    }

    pub fn viewer_blocks_author(&self) -> bool {
        self.candidate.relationship.viewer_blocks_author
    }

    pub fn viewer_mutes_author(&self) -> bool {
        self.candidate.relationship.viewer_mutes_author
    }

    pub fn viewer_mutes_retweets_from_author(&self) -> bool {
        self.candidate
            .relationship
            .viewer_mutes_retweets_from_author
    }

    pub fn has_tweet_safety_label(&self, label: SafetyLabelType) -> bool {
        self.candidate.has_safety_label(label)
    }

    pub fn is_retweet(&self) -> bool {
        self.candidate.is_retweet()
    }

    pub fn is_stale_tweet(&self) -> bool {
        self.candidate.is_stale_tweet()
    }

    pub fn is_nullcast(&self) -> bool {
        self.candidate.is_nullcast()
    }

    pub fn is_community_tweet(&self) -> bool {
        self.candidate.is_community_tweet()
    }

    pub fn has_media(&self) -> bool {
        self.candidate.has_media()
    }

    pub fn has_dmca_media(&self) -> bool {
        self.candidate.has_dmca_media()
    }

    pub fn is_nsfw_flagged(&self) -> bool {
        self.candidate.is_nsfw_flagged()
    }

    pub fn has_tweet_nsfw_user_flag(&self) -> bool {
        self.candidate.tweet_features.nsfw.user
    }

    pub fn has_tweet_nsfw_admin_flag(&self) -> bool {
        self.candidate.tweet_features.nsfw.admin
    }

    pub fn legal_takedown_in_viewer_country(&self) -> bool {
        self.takedown_in_viewer_country(legal_takedown_country)
    }

    pub fn local_laws_takedown_in_viewer_country(&self) -> bool {
        self.takedown_in_viewer_country(local_laws_takedown_country)
    }

    fn takedown_in_viewer_country(&self, extractor: fn(&TakedownReason) -> Option<&str>) -> bool {
        let Some(viewer_country) = &self.viewer.country_code else {
            return false;
        };
        self.candidate
            .tweet_features
            .takedown
            .reasons
            .iter()
            .filter_map(extractor)
            .any(|c| c.eq_ignore_ascii_case(viewer_country))
    }

    pub fn media_restricted_in_viewer_country(&self) -> bool {
        let country = self
            .viewer
            .country_code
            .as_deref()
            .unwrap_or(WORLDWIDE_COUNTRY_CODE);
        let allow = &self.candidate.tweet_features.media.geo_allow_list;
        let deny = &self.candidate.tweet_features.media.geo_deny_list;
        (!allow.is_empty() && !allow.iter().any(|c| c.eq_ignore_ascii_case(country)))
            || deny.iter().any(|c| c.eq_ignore_ascii_case(country))
    }

    pub fn author_is_suspended(&self) -> bool {
        self.candidate.author_features.is_suspended
    }

    pub fn author_is_deactivated(&self) -> bool {
        self.candidate.author_features.is_deactivated
    }

    pub fn author_is_erased(&self) -> bool {
        self.candidate.author_features.is_erased
    }

    pub fn author_is_offboarded(&self) -> bool {
        self.candidate.author_features.is_offboarded
    }

    pub fn author_is_protected(&self) -> bool {
        self.candidate.author_features.is_protected
    }

    pub fn author_is_nsfw_user(&self) -> bool {
        self.candidate.author_features.is_nsfw_user
    }

    pub fn author_is_nsfw_admin(&self) -> bool {
        self.candidate.author_features.is_nsfw_admin
    }

    pub fn author_has_user_label(&self, label: LabelValue) -> bool {
        self.candidate.author_has_user_label(label)
    }

    pub fn is_exclusive_tweet(&self) -> bool {
        self.candidate.exclusive_content.is_some()
    }

    pub fn viewer_is_conversation_author(&self) -> bool {
        match (&self.candidate.exclusive_content, self.viewer.viewer_id()) {
            (Some(exclusive), Some(viewer_id)) => viewer_id == exclusive.conversation_author_id,
            _ => false,
        }
    }

    pub fn viewer_super_follows_author(&self) -> bool {
        self.candidate
            .exclusive_content
            .as_ref()
            .is_some_and(|exclusive| exclusive.viewer_super_follows_author)
    }
}

const WORLDWIDE_COUNTRY_CODE: &str = "xx";

fn legal_takedown_country(reason: &TakedownReason) -> Option<&str> {
    match reason {
        TakedownReason::LegalRequest { country_code }
        | TakedownReason::UnspecifiedReason { country_code } => Some(country_code),
        _ => None,
    }
}

fn local_laws_takedown_country(reason: &TakedownReason) -> Option<&str> {
    match reason {
        TakedownReason::BystanderReport { country_code } => Some(country_code),
        _ => None,
    }
}

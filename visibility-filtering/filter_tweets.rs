use crate::filter::{FilterOutcome, FilterRequest, FilterTweets};
use crate::models::{RawCandidate, TweetId, VfAction};
use crate::reference_compare::{ReferenceCompareHarness, TweetVerdict};
use crate::rules::metrics::{self as ft_metrics, RequestMetricsGuard};
use crate::rules::SafetyLevel;
use std::sync::Arc;
use std::time::Instant;
use tonic::{Request, Response, Status};
use tracing::info;
use xai_visibility_filtering_proto as vf_pb;

pub struct FilterTweetsEndpoint {
    filter_tweets: FilterTweets,
    reference_compare: Option<Arc<ReferenceCompareHarness>>,
}

impl FilterTweetsEndpoint {
    pub(crate) fn new(
        filter_tweets: FilterTweets,
        reference_compare: Option<Arc<ReferenceCompareHarness>>,
    ) -> Self {
        Self {
            filter_tweets,
            reference_compare,
        }
    }

    pub async fn handle(
        &self,
        request: Request<vf_pb::VisibilityFilterRequest>,
    ) -> Result<Response<vf_pb::VisibilityFilterResponse>, Status> {
        let request_metrics = RequestMetricsGuard::new();
        let start = Instant::now();
        let req = request.into_inner();
        ft_metrics::record_batch_size(req.tweets.len());
        let viewer_id = normalize_viewer_id(req.viewer_id);
        ft_metrics::record_viewer_state(req.viewer_id, viewer_id);

        let safety_level = match req.safety_level() {
            vf_pb::SafetyLevel::FilterAll => SafetyLevel::FilterAll,
            vf_pb::SafetyLevel::TimelineHome => SafetyLevel::TimelineHome,
            vf_pb::SafetyLevel::TimelineHomeRecommendations => {
                SafetyLevel::TimelineHomeRecommendations
            }
        };
        info!(
            viewer_id = req.viewer_id,
            tweet_count = req.tweets.len(),
            country_code = ?req.country_code,
            safety_level = ?safety_level,
            "VF request"
        );

        let candidates: Vec<RawCandidate> = req
            .tweets
            .iter()
            .map(|t| RawCandidate {
                tweet_id: TweetId(t.tweet_id),
                request_author_id: t.author_id,
            })
            .collect();

        let reference_compare = self.reference_compare.as_ref().and_then(|harness| {
            harness.begin_compare(
                req.viewer_id,
                req.country_code.clone(),
                safety_level,
                req.tweets.iter().map(|t| t.tweet_id).collect(),
            )
        });

        let response = self
            .filter_tweets
            .run(FilterRequest {
                viewer_id,
                country_code: req.country_code,
                safety_level,
                candidates,
            })
            .await;

        if let Some(verdicts) = reference_compare {
            verdicts.send(
                response
                    .outcomes
                    .iter()
                    .map(|outcome| TweetVerdict {
                        tweet_id: outcome.tweet_id.0,
                        verdict: outcome.verdict.clone(),
                    })
                    .collect(),
            );
        }

        let results = response
            .outcomes
            .into_iter()
            .map(to_visibility_result)
            .collect();

        info!(
            tweet_count = response.summary.tweet_count,
            drop_count = response.summary.drop_count,
            not_found_count = response.summary.unresolved_author_count,
            latency_ms = start.elapsed().as_millis(),
            "VF response"
        );

        request_metrics.mark_success();
        Ok(Response::new(vf_pb::VisibilityFilterResponse { results }))
    }
}

fn normalize_viewer_id(raw: Option<u64>) -> Option<u64> {
    raw.filter(|&id| id as i64 > 0)
}

fn to_visibility_result(outcome: FilterOutcome) -> vf_pb::TweetVisibilityResult {
    let (kind, filtered_reason) = match outcome.verdict.action {
        VfAction::Allow => (vf_pb::action::Kind::Allow(true), None),
        VfAction::Drop(reason) => (
            vf_pb::action::Kind::Drop(vf_pb::DropReason {}),
            Some(reason.into()),
        ),
        VfAction::Interstitial(reason) => {
            (vf_pb::action::Kind::Interstitial(true), Some(reason.into()))
        }
    };
    vf_pb::TweetVisibilityResult {
        tweet_id: outcome.tweet_id.0,
        action: Some(vf_pb::Action { kind: Some(kind) }),
        filtered_reason,
        safety_labels: outcome.safety_labels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Verdict;
    use xai_visibility_filtering::models::FilteredReason;

    fn outcome(tweet_id: u64, action: VfAction) -> FilterOutcome {
        FilterOutcome {
            tweet_id: TweetId(tweet_id),
            verdict: Verdict {
                action,
                decided_by: Some("test"),
            },
            safety_labels: None,
        }
    }

    async fn gizmoduck_calls(viewer_id: Option<u64>) -> usize {
        let gizmoduck = std::sync::Arc::new(
            xai_core_entities::gizmoduck_client::MockGizmoduckClient::default(),
        );
        let endpoint = FilterTweetsEndpoint::new(
            crate::filter::test_support::filter_tweets_with_gizmoduck(gizmoduck.clone()),
            None,
        );
        let response = endpoint
            .handle(Request::new(vf_pb::VisibilityFilterRequest {
                safety_level: vf_pb::SafetyLevel::TimelineHome.into(),
                tweets: vec![vf_pb::TweetInput {
                    tweet_id: 2,
                    author_id: Some(20),
                }],
                viewer_id,
                country_code: None,
            }))
            .await
            .unwrap();
        assert_eq!(response.into_inner().results.len(), 1);
        gizmoduck.call_count()
    }

    #[tokio::test]
    async fn endpoint_treats_zero_viewer_id_as_logged_out() {
        let logged_out = gizmoduck_calls(None).await;
        assert_eq!(gizmoduck_calls(Some(0)).await, logged_out);
        assert_eq!(gizmoduck_calls(Some(42)).await, logged_out + 1);
    }

    #[test]
    fn normalize_viewer_id_cases() {
        assert_eq!(normalize_viewer_id(Some(0)), None);
        assert_eq!(normalize_viewer_id(Some(u64::MAX)), None);
        assert_eq!(normalize_viewer_id(Some(42)), Some(42));
        assert_eq!(normalize_viewer_id(None), None);
    }

    #[test]
    fn allow_maps_to_proto_result() {
        let result = to_visibility_result(outcome(7, VfAction::Allow));

        assert_eq!(result.tweet_id, 7);
        assert!(matches!(
            result.action.unwrap().kind,
            Some(vf_pb::action::Kind::Allow(true))
        ));
        assert!(result.filtered_reason.is_none());
    }

    #[test]
    fn drop_maps_to_proto_result_with_labels() {
        let labels = vf_pb::SafetyLabelMap::default();
        let mut drop = outcome(8, VfAction::Drop(FilteredReason::ContainNsfwMedia));
        drop.safety_labels = Some(labels.clone());
        let result = to_visibility_result(drop);

        assert_eq!(result.tweet_id, 8);
        assert!(matches!(
            result.action.unwrap().kind,
            Some(vf_pb::action::Kind::Drop(_))
        ));
        assert!(matches!(
            result.filtered_reason.unwrap().reason,
            Some(vf_pb::filtered_reason::Reason::ContainNsfwMedia(true))
        ));
        assert_eq!(result.safety_labels, Some(labels));
    }

    #[test]
    fn interstitial_maps_to_proto_result() {
        let result = to_visibility_result(outcome(
            9,
            VfAction::Interstitial(FilteredReason::ContainNsfwMedia),
        ));

        assert_eq!(result.tweet_id, 9);
        assert!(matches!(
            result.action.unwrap().kind,
            Some(vf_pb::action::Kind::Interstitial(true))
        ));
        assert!(matches!(
            result.filtered_reason.unwrap().reason,
            Some(vf_pb::filtered_reason::Reason::ContainNsfwMedia(true))
        ));
    }
}

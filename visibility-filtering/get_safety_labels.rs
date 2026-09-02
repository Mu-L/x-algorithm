use crate::safety_label_source::metrics::{self, BatchStage, RequestMetricsGuard};
use crate::safety_label_source::types::FailureKind;
use crate::safety_label_source::{LookupError, SafetyLabelSource};
use enum_map::EnumMap;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tonic::{Request, Response, Status};
use xai_visibility_filtering_proto as vf_pb;

pub struct GetSafetyLabelsEndpoint {
    safety_label_source: Arc<SafetyLabelSource>,
}

impl GetSafetyLabelsEndpoint {
    pub fn new(safety_label_source: Arc<SafetyLabelSource>) -> Self {
        Self {
            safety_label_source,
        }
    }

    pub async fn handle(
        &self,
        request: Request<vf_pb::GetSafetyLabelsRequest>,
    ) -> Result<Response<vf_pb::GetSafetyLabelsResponse>, Status> {
        let request_metrics = RequestMetricsGuard::new();
        match self.handle_inner(request).await {
            Ok(response) => {
                request_metrics.mark_success();
                Ok(response)
            }
            Err(status) => {
                request_metrics.mark_failure();
                Err(status)
            }
        }
    }

    async fn handle_inner(
        &self,
        request: Request<vf_pb::GetSafetyLabelsRequest>,
    ) -> Result<Response<vf_pb::GetSafetyLabelsResponse>, Status> {
        let req = request.into_inner();

        let unique_ids: Vec<u64> = req
            .tweet_ids
            .into_iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        metrics::record_batch_size(BatchStage::Request, unique_ids.len());

        let resolved = self.safety_label_source.get(&unique_ids).await;
        let outcome = GetSafetyLabelsOutcome::try_from_resolved(resolved, unique_ids.len())?;
        let failed_count = outcome.failure_count();

        metrics::record_lookup_tweet_ids(outcome.success_count(), failed_count);
        for (kind, &count) in &outcome.failures {
            metrics::record_lookup_failures(kind, count);
        }

        tracing::info!(
            requested_count = outcome.requested_count(),
            success_count = outcome.success_count(),
            failure_count = failed_count,
            manhattan_fetch_failure_count = outcome.failures[FailureKind::ManhattanFetch],
            manhattan_decode_failure_count = outcome.failures[FailureKind::ManhattanDecode],
            other_failure_count = outcome.failures[FailureKind::Other],
            is_partial = outcome.is_partial_failure(),
            is_full_failure = outcome.is_full_failure(),
            "GetSafetyLabels lookup complete"
        );

        Ok(Response::new(vf_pb::GetSafetyLabelsResponse {
            results: outcome.results,
            failed_ids: outcome.failed_ids,
        }))
    }
}

#[derive(Debug)]
pub(crate) struct GetSafetyLabelsOutcome {
    requested_count: usize,
    pub(crate) results: HashMap<u64, vf_pb::SafetyLabelMap>,
    pub(crate) failed_ids: Vec<u64>,
    pub(crate) failures: EnumMap<FailureKind, usize>,
}

impl GetSafetyLabelsOutcome {
    pub(crate) fn try_from_resolved(
        resolved: HashMap<u64, Result<Arc<vf_pb::SafetyLabelMap>, LookupError>>,
        requested_count: usize,
    ) -> Result<Self, Status> {
        let mut results = HashMap::with_capacity(resolved.len());
        let mut failed_ids = Vec::new();
        let mut failures = EnumMap::default();

        for (id, lookup_result) in resolved {
            match lookup_result {
                Ok(label_map) => {
                    results.insert(id, Arc::unwrap_or_clone(label_map));
                }
                Err(e) => {
                    failed_ids.push(id);
                    failures[e.kind] += 1;
                }
            }
        }

        failed_ids.sort_unstable();

        let result = Self {
            requested_count,
            results,
            failed_ids,
            failures,
        };

        if result.accounted_count() != result.requested_count {
            return Err(Status::internal(format!(
                "GetSafetyLabels invariant violation: requested={} accounted={}",
                result.requested_count,
                result.accounted_count()
            )));
        }

        Ok(result)
    }

    pub(crate) fn requested_count(&self) -> usize {
        self.requested_count
    }

    pub(crate) fn success_count(&self) -> usize {
        self.results.len()
    }

    pub(crate) fn failure_count(&self) -> usize {
        self.failed_ids.len()
    }

    pub(crate) fn has_failures(&self) -> bool {
        !self.failed_ids.is_empty()
    }

    pub(crate) fn is_partial_failure(&self) -> bool {
        self.has_failures() && self.failure_count() < self.requested_count
    }

    pub(crate) fn is_full_failure(&self) -> bool {
        self.has_failures() && self.failure_count() == self.requested_count
    }

    fn accounted_count(&self) -> usize {
        self.results.len() + self.failed_ids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::Code;

    fn labels_with_entry(type_id: i32) -> vf_pb::SafetyLabelMap {
        vf_pb::SafetyLabelMap {
            labels: HashMap::from([(
                type_id,
                vf_pb::SafetyLabel {
                    score: Some(0.5),
                    applicable_users: Vec::new(),
                    holdback_experiment: None,
                    source: None,
                    created_at_msec: None,
                    expires_at_msec: None,
                    applicable_countries: Vec::new(),
                    safety_label_source: None,
                },
            )]),
        }
    }

    #[test]
    fn try_from_resolved_reports_all_success() {
        let outcome = GetSafetyLabelsOutcome::try_from_resolved(
            HashMap::from([
                (1, Ok(Arc::new(labels_with_entry(11)))),
                (2, Ok(Arc::new(labels_with_entry(22)))),
            ]),
            2,
        )
        .unwrap();

        assert_eq!(outcome.requested_count(), 2);
        assert_eq!(outcome.success_count(), 2);
        assert_eq!(outcome.failure_count(), 0);
        assert!(outcome.failed_ids.is_empty());
        assert!(outcome.failures.values().all(|&c| c == 0));
        assert!(!outcome.has_failures());
        assert!(!outcome.is_partial_failure());
        assert!(!outcome.is_full_failure());
    }

    #[test]
    fn try_from_resolved_reports_partial_failure() {
        let outcome = GetSafetyLabelsOutcome::try_from_resolved(
            HashMap::from([
                (1, Ok(Arc::new(labels_with_entry(11)))),
                (
                    2,
                    Err(LookupError::new(FailureKind::ManhattanDecode, "decode")),
                ),
                (
                    3,
                    Err(LookupError::new(FailureKind::ManhattanFetch, "mh down")),
                ),
            ]),
            3,
        )
        .unwrap();

        assert_eq!(outcome.requested_count(), 3);
        assert_eq!(outcome.success_count(), 1);
        assert_eq!(outcome.results.len(), 1);
        assert_eq!(outcome.failed_ids, vec![2, 3]);
        assert_eq!(outcome.failures[FailureKind::ManhattanFetch], 1);
        assert_eq!(outcome.failures[FailureKind::ManhattanDecode], 1);
        assert_eq!(outcome.failures[FailureKind::Other], 0);
        assert_eq!(outcome.failure_count(), 2);
        assert!(outcome.is_partial_failure());
        assert!(!outcome.is_full_failure());
    }

    #[test]
    fn try_from_resolved_reports_full_failure() {
        let outcome = GetSafetyLabelsOutcome::try_from_resolved(
            HashMap::from([
                (
                    4,
                    Err(LookupError::new(FailureKind::ManhattanFetch, "mh down")),
                ),
                (
                    5,
                    Err(LookupError::new(FailureKind::ManhattanFetch, "mh down")),
                ),
            ]),
            2,
        )
        .unwrap();

        assert_eq!(outcome.requested_count(), 2);
        assert_eq!(outcome.success_count(), 0);
        assert!(outcome.results.is_empty());
        assert_eq!(outcome.failed_ids, vec![4, 5]);
        assert_eq!(outcome.failures[FailureKind::ManhattanFetch], 2);
        assert_eq!(outcome.failures[FailureKind::ManhattanDecode], 0);
        assert_eq!(outcome.failures[FailureKind::Other], 0);
        assert_eq!(outcome.failure_count(), 2);
        assert!(!outcome.is_partial_failure());
        assert!(outcome.is_full_failure());
    }

    #[test]
    fn try_from_resolved_rejects_missing_ids() {
        let status = GetSafetyLabelsOutcome::try_from_resolved(
            HashMap::from([(7, Ok(Arc::new(labels_with_entry(22))))]),
            2,
        )
        .unwrap_err();

        assert_eq!(status.code(), Code::Internal);
        assert!(status.message().contains("requested=2 accounted=1"));
    }
}

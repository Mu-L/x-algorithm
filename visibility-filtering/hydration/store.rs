use crate::hydration::batch::{RawHydrationBatch, TweetHydrationBatch};
use crate::hydration::decode::author::DecodedAuthor;
use crate::hydration::decode::tweet::build_tweet_features;
use crate::hydration::execute::Reply;
use crate::hydration::plan::{Group, KeyOrigin};
use crate::hydration::tes_composite::TweetForVisibility;
use crate::hydration::{
    candidate_count_by_key, HydrationOutput, HydrationRequest, Hydrator, Hydrators,
};
use crate::models::{
    resolve_candidates, ConversationControlFeatures, HydratedTweetCandidate, PureCore,
    RawCandidate, SafetyLabelMap, TweetCandidateInput, TweetId, Viewer, ViewerFeatures,
    ViewerProfile,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use xai_core_entities::entities::{ConversationControl, ConversationControlArm};
use xai_visibility_filtering_proto as vf_pb;

#[derive(Default)]
pub(super) struct Store {
    pub(super) viewer_id: Option<u64>,
    pub(super) tweet_ids: Vec<TweetId>,
    pure_cores: TweetHydrationBatch<PureCore>,
    pub(super) candidates: Vec<TweetCandidateInput>,
    tweets: Option<TweetHydrationBatch<TweetForVisibility>>,
    controls: Option<TweetHydrationBatch<ConversationControl>>,
    labels: Option<TweetHydrationBatch<Arc<vf_pb::SafetyLabelMap>>>,
    viewer: Option<RawHydrationBatch<ViewerProfile>>,
    authors: Option<RawHydrationBatch<DecodedAuthor>>,
    edges: Vec<(Hydrators, RawHydrationBatch<Hydrators>)>,
    viewer_country: Option<RawHydrationBatch<Arc<str>>>,
    incomplete_keys: Vec<(Hydrators, HashSet<u64>)>,
    callable: Hydrators,
    pub(super) core_elapsed: Duration,
    pub(super) composite_elapsed: Option<Duration>,
}

impl Store {
    pub(super) fn new(
        viewer_id: Option<u64>,
        tweet_ids: Vec<TweetId>,
        callable: Hydrators,
    ) -> Self {
        Self {
            viewer_id,
            tweet_ids,
            callable,
            ..Self::default()
        }
    }

    pub(super) fn controls(&self) -> impl Iterator<Item = &ConversationControl> {
        self.tweet_ids
            .iter()
            .filter_map(|id| self.controls.as_ref()?.get(id))
    }

    pub(super) fn keys(&self, origin: KeyOrigin) -> Vec<u64> {
        match origin {
            KeyOrigin::RequestTweets => self.tweet_ids.iter().map(|id| id.0).collect(),
            KeyOrigin::Viewer => self.viewer_id.into_iter().collect(),
            KeyOrigin::ViewerForCoAllowedList => self
                .viewer_id
                .filter(|_| self.controls().any(lists_countries))
                .into_iter()
                .collect(),
            KeyOrigin::PureCoreAuthor => self
                .candidates
                .iter()
                .map(|candidate| candidate.author_id.get())
                .collect(),
            KeyOrigin::PureCoreReplyRoot => self
                .candidates
                .iter()
                .filter_map(|candidate| self.reply_root(candidate))
                .collect(),
            KeyOrigin::ExclusiveConversationAuthor => self
                .tweet_ids
                .iter()
                .filter_map(|id| self.exclusive_author(id))
                .collect(),
            KeyOrigin::ConversationRoot(arms) => self
                .controls()
                .filter(|control| arms.contains(&control.arm))
                .map(|control| control.conversation_tweet_author_id)
                .collect(),
            KeyOrigin::MyNetworkRootNotFollowingViewer => self
                .controls()
                .filter_map(|control| self.root_not_following_viewer(control))
                .collect(),
        }
    }

    pub(super) fn distinct_keys(&self, nodes: Hydrators) -> Vec<u64> {
        let mut keys: Vec<u64> = nodes
            .iter()
            .enumerate()
            .filter(|&(position, node)| {
                !nodes
                    .iter()
                    .take(position)
                    .any(|earlier| earlier.spec().key == node.spec().key)
            })
            .flat_map(|(_, node)| self.keys(node.spec().key))
            .collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    fn key(&self, origin: KeyOrigin, candidate: &TweetCandidateInput) -> Option<u64> {
        let control = || self.controls.as_ref()?.get(&candidate.tweet_id);
        match origin {
            KeyOrigin::RequestTweets => Some(candidate.tweet_id.0),
            KeyOrigin::Viewer => self.viewer_id,
            KeyOrigin::ViewerForCoAllowedList => self
                .viewer_id
                .filter(|_| control().is_some_and(lists_countries)),
            KeyOrigin::PureCoreAuthor => Some(candidate.author_id.get()),
            KeyOrigin::PureCoreReplyRoot => self.reply_root(candidate),
            KeyOrigin::ExclusiveConversationAuthor => self.exclusive_author(&candidate.tweet_id),
            KeyOrigin::ConversationRoot(arms) => control()
                .filter(|control| arms.contains(&control.arm))
                .map(|control| control.conversation_tweet_author_id),
            KeyOrigin::MyNetworkRootNotFollowingViewer => {
                control().and_then(|control| self.root_not_following_viewer(control))
            }
        }
    }

    fn root_not_following_viewer(&self, control: &ConversationControl) -> Option<u64> {
        if control.arm != ConversationControlArm::MyNetwork {
            return None;
        }
        let root = control.conversation_tweet_author_id;
        let node = Hydrator::RootFollowsViewer;
        let (_, answers) = self.edges.iter().find(|(nodes, _)| nodes.contains(node))?;
        (!answers.get(&root)?.contains(node)).then_some(root)
    }

    pub(super) fn candidate_count_by_key(&self, nodes: Hydrators) -> HashMap<u64, usize> {
        match nodes.iter().next().map(|node| node.spec().key) {
            Some(KeyOrigin::RequestTweets) => {
                return candidate_count_by_key(self.tweet_ids.iter().map(|id| id.0));
            }
            Some(origin @ (KeyOrigin::Viewer | KeyOrigin::ViewerForCoAllowedList)) => {
                return self
                    .keys(origin)
                    .into_iter()
                    .map(|viewer| (viewer, 1))
                    .collect();
            }
            _ => {}
        }
        let mut counts = HashMap::new();
        for candidate in &self.candidates {
            for (position, node) in nodes.iter().enumerate() {
                let Some(key) = self.key(node.spec().key, candidate) else {
                    continue;
                };
                let counted = nodes
                    .iter()
                    .take(position)
                    .any(|earlier| self.key(earlier.spec().key, candidate) == Some(key));
                if !counted {
                    *counts.entry(key).or_default() += 1;
                }
            }
        }
        counts
    }

    fn reply_root(&self, candidate: &TweetCandidateInput) -> Option<u64> {
        self.pure_cores
            .get(&candidate.tweet_id)?
            .direct_reply_root_author_id
            .map(|author| author.get())
    }

    fn exclusive_author(&self, id: &TweetId) -> Option<u64> {
        self.tweets
            .as_ref()?
            .get(id)?
            .exclusive_conversation_author_id
    }

    pub(super) fn write(
        &mut self,
        group: &Group,
        reply: Reply,
        raw: &[RawCandidate],
        started: Instant,
    ) {
        let elapsed = started.elapsed();
        self.incomplete_keys
            .push((group.nodes, reply.incomplete_keys()));
        match reply {
            Reply::PureCores(pure_cores) => {
                let pure_cores = pure_cores.map_keys(TweetId);
                self.core_elapsed = elapsed;
                self.candidates = resolve_candidates(raw, &pure_cores);
                self.pure_cores = pure_cores;
            }
            Reply::Tweets(tweets) => {
                self.composite_elapsed = Some(elapsed);
                self.tweets = Some(tweets.map_keys(TweetId));
            }
            Reply::Controls(controls) => self.controls = Some(controls.map_keys(TweetId)),
            Reply::Labels(labels) => self.labels = Some(labels.map_keys(TweetId)),
            Reply::Viewer(viewer) => self.viewer = Some(viewer),
            Reply::Authors(authors) => self.authors = Some(authors),
            Reply::Edges(answers) => self.edges.push((group.nodes, answers)),
            Reply::ViewerCountry(country) => self.viewer_country = Some(country),
        }
    }

    pub(super) fn assemble(self, request: HydrationRequest<'_>) -> HydrationOutput {
        let candidates: Vec<HydratedTweetCandidate> = self
            .candidates
            .iter()
            .map(|input| self.candidate(input))
            .collect();
        let failed_ids = candidates
            .iter()
            .filter(|candidate| !candidate.failed.is_empty())
            .map(|candidate| TweetId(candidate.tweet_id))
            .collect();
        let viewer = match request.viewer_id {
            None => Viewer::LoggedOut,
            Some(id) => Viewer::LoggedIn {
                id,
                profile: self
                    .viewer
                    .and_then(|viewer| viewer.into_hydrated().remove(&id)?.into_value())
                    .unwrap_or_default(),
            },
        };
        HydrationOutput {
            viewer_features: ViewerFeatures::from_request(viewer, request.country_code),
            candidates,
            safety_labels: self
                .labels
                .map(|labels| {
                    labels
                        .into_hydrated()
                        .into_iter()
                        .filter_map(|(id, labels)| Some((id, labels.into_value()?)))
                        .collect()
                })
                .unwrap_or_default(),
            failed_ids,
            pure_cores: self.pure_cores,
        }
    }

    fn failed(&self, input: &TweetCandidateInput) -> Hydrators {
        let answered_incompletely = self
            .incomplete_keys
            .iter()
            .filter(|(_, incomplete)| !incomplete.is_empty())
            .flat_map(|(nodes, incomplete)| {
                nodes.iter().filter(|node| {
                    self.key(node.spec().key, input)
                        .is_some_and(|key| incomplete.contains(&key))
                })
            })
            .fold(Hydrators::empty(), Hydrators::with);
        if answered_incompletely.is_empty() {
            return answered_incompletely;
        }
        self.callable
            .iter()
            .fold(answered_incompletely, |failed, node| match node.input() {
                Some(input_node)
                    if failed.contains(input_node)
                        && self.key(node.spec().key, input).is_none() =>
                {
                    failed.with(node)
                }
                _ => failed,
            })
    }

    fn candidate(&self, input: &TweetCandidateInput) -> HydratedTweetCandidate {
        let id = &input.tweet_id;
        let mut candidate = HydratedTweetCandidate {
            tweet_id: id.0,
            author_id: input.author_id.get(),
            edges: self
                .edges
                .iter()
                .flat_map(|(nodes, answers)| {
                    nodes.iter().filter(|&node| {
                        self.key(node.spec().key, input)
                            .and_then(|key| answers.get(&key))
                            .is_some_and(|holds| holds.contains(node))
                    })
                })
                .fold(Hydrators::empty(), Hydrators::with),
            failed: self.failed(input),
            ..Default::default()
        };
        if let Some(tweets) = &self.tweets {
            candidate.tweet_features = build_tweet_features(tweets.get(id));
        }
        if let Some(labels) = self.labels.as_ref().and_then(|labels| labels.get(id)) {
            candidate.safety_labels = SafetyLabelMap::from_proto_label_types(labels);
        }
        if let Some(author) = self
            .authors
            .as_ref()
            .and_then(|authors| authors.get(&input.author_id.get()))
        {
            (candidate.author_features, candidate.author_labels) = *author;
        }
        candidate.conversation_control = self
            .controls
            .as_ref()
            .and_then(|controls| controls.get(id))
            .cloned()
            .map(|control| ConversationControlFeatures {
                viewer_country: self
                    .viewer_country
                    .as_ref()
                    .zip(self.viewer_id)
                    .filter(|_| control.arm == ConversationControlArm::Co)
                    .and_then(|(country, viewer_id)| country.get(&viewer_id))
                    .cloned(),
                control,
            });
        candidate
    }
}

fn lists_countries(control: &ConversationControl) -> bool {
    control.arm == ConversationControlArm::Co && !control.allowed_country_codes.is_empty()
}

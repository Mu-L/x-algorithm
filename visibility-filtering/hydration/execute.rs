use crate::clients::socialgraph_client::EdgeQuery;
use crate::hydration::batch::{Hydrated, HydrationBatch, RawHydrationBatch};
use crate::hydration::decode::author::DecodedAuthor;
use crate::hydration::fallback_cache::FallbackCache;
use crate::hydration::metrics::{
    self, record_batch_size, record_flock_missing_keys, record_viewer_country,
    record_wingman_second_degree, timed_results,
};
use crate::hydration::plan::{Group, Source};
use crate::hydration::sources::Sources;
use crate::hydration::store::Store;
use crate::hydration::tes_composite::TweetForVisibility;
use crate::hydration::{
    HydrationOutput, HydrationPlan, HydrationRequest, Hydrator, Hydrators, HYDRATION_TIMEOUT,
};
use crate::models::{PureCore, ViewerProfile};
use crate::rules::SafetyLevel;
use futures::stream::{FuturesUnordered, StreamExt};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;
use xai_core_entities::entities::{ConversationControl, ConversationControlArm};
use xai_visibility_filtering_proto as vf_pb;

#[derive(Debug, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
enum ViewerCountry {
    NoAllowedList,
    Found,
    NoRow,
    Failed,
}

pub(super) enum Reply {
    PureCores(RawHydrationBatch<PureCore>),
    Tweets(RawHydrationBatch<TweetForVisibility>),
    Controls(RawHydrationBatch<ConversationControl>),
    Labels(RawHydrationBatch<Arc<vf_pb::SafetyLabelMap>>),
    Viewer(RawHydrationBatch<ViewerProfile>),
    Authors(RawHydrationBatch<DecodedAuthor>),
    Edges(RawHydrationBatch<Hydrators>),
    ViewerCountry(RawHydrationBatch<Arc<str>>),
}

impl Reply {
    pub(super) fn incomplete_keys(&self) -> HashSet<u64> {
        match self {
            Reply::PureCores(batch) => batch.incomplete_keys().copied().collect(),
            Reply::Tweets(batch) => batch.incomplete_keys().copied().collect(),
            Reply::Controls(batch) => batch.incomplete_keys().copied().collect(),
            Reply::Labels(batch) => batch.incomplete_keys().copied().collect(),
            Reply::Viewer(batch) => batch.incomplete_keys().copied().collect(),
            Reply::Authors(batch) => batch.incomplete_keys().copied().collect(),
            Reply::Edges(batch) => batch.incomplete_keys().copied().collect(),
            Reply::ViewerCountry(batch) => batch.incomplete_keys().copied().collect(),
        }
    }
}

type Call<'a> = Pin<Box<dyn Future<Output = (&'a Group, Reply)> + Send + 'a>>;

struct Timed {
    client: String,
    method: String,
    level: SafetyLevel,
    counts: HashMap<u64, usize>,
}

impl Timed {
    async fn run<V>(
        &self,
        call: impl Future<Output = RawHydrationBatch<V>>,
    ) -> RawHydrationBatch<V> {
        timed_results(
            &self.client,
            &self.method,
            self.level,
            &self.counts,
            HYDRATION_TIMEOUT,
            call,
        )
        .await
    }
}

impl HydrationPlan {
    pub(crate) async fn hydrate(
        &self,
        sources: &dyn Sources,
        request: HydrationRequest<'_>,
    ) -> HydrationOutput {
        let started = Instant::now();
        let mut store = Store::new(
            request.viewer_id,
            request.raw_candidates.iter().map(|c| c.tweet_id).collect(),
            self.callable(request.viewer_id),
        );
        let mut running: FuturesUnordered<Call<'_>> = FuturesUnordered::new();
        let mut ready: Vec<&Group> = self.groups().filter(|g| g.input.is_none()).collect();
        loop {
            while let Some(group) = ready.pop() {
                match self.start(group, &store, sources) {
                    Some(call) => running.push(call),
                    None => ready.extend(self.waiting_on(group)),
                }
            }
            let Some((group, reply)) = running.next().await else {
                break;
            };
            store.write(group, reply, request.raw_candidates, started);
            ready.extend(self.waiting_on(group));
        }
        metrics::record_tes_join_latency(
            self.level(),
            store
                .composite_elapsed
                .map_or(store.core_elapsed, |composite| {
                    composite.max(store.core_elapsed)
                }),
        );
        store.assemble(request)
    }

    fn waiting_on<'a>(&'a self, done: &'a Group) -> impl Iterator<Item = &'a Group> + 'a {
        self.groups()
            .filter(move |group| group.input.is_some_and(|input| done.nodes.contains(input)))
    }

    fn start<'a>(
        &'a self,
        group: &'a Group,
        store: &Store,
        sources: &'a dyn Sources,
    ) -> Option<Call<'a>> {
        let level = self.level();
        if store.viewer_id.is_none() && group.nodes.iter().all(Hydrator::needs_viewer) {
            return None;
        }
        let (nodes, queries): (Vec<Hydrators>, Vec<EdgeQuery>) = group
            .edges()
            .into_iter()
            .map(|(graph, direction, nodes)| {
                let query = EdgeQuery {
                    graph,
                    direction,
                    destination_ids: store.distinct_keys(nodes),
                };
                (nodes, query)
            })
            .unzip();
        let keys = if queries.is_empty() {
            store.distinct_keys(group.nodes)
        } else {
            Vec::new()
        };
        if keys.is_empty() && queries.iter().all(|query| query.destination_ids.is_empty()) {
            if group.source == Source::ViewerCountry
                && store
                    .controls()
                    .any(|control| control.arm == ConversationControlArm::Co)
            {
                record_viewer_country(ViewerCountry::NoAllowedList.into(), level);
            }
            return None;
        }
        let (client, method) = group.label();
        if let Some(size) = batch_size(group, store, keys.len()) {
            record_batch_size(&client, size);
        }
        let timed = Timed {
            client,
            method,
            level,
            counts: store.candidate_count_by_key(group.nodes),
        };
        let viewer_id = store.viewer_id;
        let reply: Pin<Box<dyn Future<Output = Reply> + Send + 'a>> = match group.source {
            Source::TesPureCore => {
                let cache = sources
                    .pure_core_cache()
                    .map(|cache| (cache, cache.begin_request()));
                Box::pin(async move {
                    Reply::PureCores(fall_back(cache, timed.run(sources.pure_cores(keys)).await))
                })
            }
            Source::TesComposite => {
                Box::pin(async move { Reply::Tweets(timed.run(sources.tweets(keys)).await) })
            }
            Source::TesConversationControl => Box::pin(async move {
                Reply::Controls(timed.run(sources.conversation_controls(keys)).await)
            }),
            Source::SafetyLabels => {
                Box::pin(async move { Reply::Labels(timed.run(sources.safety_labels(keys)).await) })
            }
            Source::GizmoduckViewer => {
                let viewer_id = viewer_id?;
                let fields = group.fields();
                Box::pin(async move {
                    Reply::Viewer(timed.run(sources.viewer(viewer_id, &fields)).await)
                })
            }
            Source::GizmoduckAuthor => {
                let fields = group.fields();
                let cache = sources
                    .author_cache()
                    .map(|cache| (cache, cache.begin_request()));
                Box::pin(async move {
                    Reply::Authors(fall_back(
                        cache,
                        timed.run(sources.users(keys, &fields)).await,
                    ))
                })
            }
            Source::Flock => {
                let viewer_id = viewer_id?;
                Box::pin(async move {
                    let edges = timed
                        .run(async {
                            let edges = sources.select_edges(viewer_id, &queries, &nodes).await;
                            let (edges, missing) = missing_sets_read_no_edge(edges);
                            record_flock_missing_keys(&timed.client, &timed.method, level, missing);
                            edges
                        })
                        .await;
                    Reply::Edges(edges)
                })
            }
            Source::ViewerCountry => {
                let viewer_id = viewer_id?;
                Box::pin(async move {
                    let country = timed.run(sources.viewer_country(viewer_id)).await;
                    let result = match country.hydrated(&viewer_id) {
                        Some(Hydrated::Found(_)) => ViewerCountry::Found,
                        Some(Hydrated::NotFound) => ViewerCountry::NoRow,
                        _ => ViewerCountry::Failed,
                    };
                    record_viewer_country(result.into(), level);
                    Reply::ViewerCountry(country)
                })
            }
            Source::Wingman => {
                let viewer_id = viewer_id?;
                Box::pin(async move {
                    let answers = timed.run(sources.second_degree(viewer_id, keys)).await;
                    Reply::Edges(second_degree_fails_open(answers, level))
                })
            }
        };
        Some(Box::pin(async move { (group, reply.await) }))
    }
}

fn batch_size(group: &Group, store: &Store, keys: usize) -> Option<usize> {
    match group.source {
        Source::TesPureCore | Source::TesConversationControl | Source::GizmoduckAuthor => {
            Some(keys)
        }
        Source::SafetyLabels | Source::Wingman => Some(store.tweet_ids.len()),
        Source::Flock if group.input == Some(Hydrator::PureCore) => Some(store.candidates.len()),
        Source::Flock => Some(store.tweet_ids.len()),
        Source::TesComposite | Source::GizmoduckViewer | Source::ViewerCountry => None,
    }
}

fn fall_back<V: Clone>(
    cache: Option<(&FallbackCache<u64, V>, u64)>,
    batch: RawHydrationBatch<V>,
) -> RawHydrationBatch<V> {
    match cache {
        Some((cache, generation)) => cache.resolve_hydration_batch(generation, batch),
        None => batch,
    }
}

fn missing_sets_read_no_edge(
    edges: RawHydrationBatch<Hydrators>,
) -> (RawHydrationBatch<Hydrators>, usize) {
    let mut edges = edges.into_hydrated();
    let mut missing = 0;
    for answer in edges.values_mut() {
        if let Hydrated::Partial(holds) = answer {
            let holds = *holds;
            *answer = Hydrated::Found(holds);
            missing += 1;
        }
    }
    (HydrationBatch::from_hydrated(edges), missing)
}

fn second_degree_fails_open(
    answers: RawHydrationBatch<bool>,
    level: SafetyLevel,
) -> RawHydrationBatch<Hydrators> {
    let answers = answers.into_hydrated();
    let answered = |in_network: bool| {
        answers
            .values()
            .filter(|answer| answer.value() == Some(&in_network))
            .count()
    };
    record_wingman_second_degree(answered(true), answered(false), level);
    HydrationBatch::from_hydrated(
        answers
            .into_iter()
            .map(|(root, answer)| {
                let holds = match answer.into_value() {
                    Some(true) => Hydrators::of(Hydrator::RootFollowsViewerSecondDegree),
                    Some(false) | None => Hydrators::empty(),
                };
                (root, Hydrated::Found(holds))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::socialgraph_client::{EdgeDirection, Graph};
    use crate::hydration::decode::author::fallback_cache;
    use crate::hydration::decode::tweet::pure_core_fallback_cache;
    use crate::hydration::sources::{Fault, InMemorySources};
    use crate::models::{RawCandidate, TweetId, Viewer};
    use crate::rules::{RuleEngine, SafetyLevel};
    use xai_core_entities::entities::{
        GizmoduckUser, GizmoduckUserResult, PureCoreData, Safety, UserResponseState,
    };
    use xai_core_entities::gizmoduck_client::{QueryFields, ViewerData};

    const VIEWER: u64 = 50;

    fn raw(tweet_id: u64, request_author_id: Option<u64>) -> RawCandidate {
        RawCandidate {
            tweet_id: TweetId(tweet_id),
            request_author_id,
        }
    }

    async fn hydrate(
        sources: &InMemorySources,
        level: SafetyLevel,
        viewer_id: Option<u64>,
        raw: &[RawCandidate],
    ) -> HydrationOutput {
        RuleEngine::for_tests()
            .plan(level)
            .hydrate(
                sources,
                HydrationRequest::new(viewer_id, Some("US".into()), raw),
            )
            .await
    }

    fn ids(ids: &[u64]) -> HashSet<TweetId> {
        ids.iter().copied().map(TweetId).collect()
    }

    fn suspended() -> GizmoduckUserResult {
        GizmoduckUserResult {
            user: Some(GizmoduckUser {
                safety: Safety {
                    suspended: true,
                    ..Default::default()
                },
                ..Default::default()
            }),
            response_state: Some(UserResponseState::Found),
        }
    }

    fn exclusive_tweet() -> TweetForVisibility {
        TweetForVisibility {
            author_id: 900,
            source_tweet_id: None,
            is_nullcast: false,
            nsfw_user: false,
            nsfw_admin: false,
            has_takedown: false,
            takedown_reasons: vec![],
            media: Default::default(),
            is_community_tweet: false,
            edit_control: None,
            exclusive_conversation_author_id: Some(30),
        }
    }

    fn control(arm: ConversationControlArm, root: u64, countries: &[&str]) -> ConversationControl {
        ConversationControl {
            arm,
            conversation_tweet_author_id: root,
            invited_user_ids: vec![],
            invite_via_mention: None,
            allowed_country_codes: countries.iter().map(|c| (*c).to_owned()).collect(),
        }
    }

    #[tokio::test]
    async fn failed_ids_reports_exactly_the_candidates_each_node_flags() {
        use ConversationControlArm::{Co, Community};
        use SafetyLevel::{TimelineHome, TimelineHomeHydration};
        let world = || InMemorySources::default().tweet(1, 10).tweet(2, 20);
        let rows = [
            ("healthy home", TimelineHome, world(), vec![]),
            ("healthy level 82", TimelineHomeHydration, world(), vec![]),
            (
                "failed pure core",
                TimelineHome,
                world().fault(Source::TesPureCore, Fault::Fails),
                vec![1, 2],
            ),
            (
                "incomplete author",
                TimelineHome,
                world().user(
                    10,
                    GizmoduckUserResult {
                        response_state: Some(UserResponseState::Failed),
                        ..suspended()
                    },
                ),
                vec![1],
            ),
            (
                "failed author-keyed select",
                TimelineHome,
                world().fail_graph(Graph::Mutes),
                vec![1, 2],
            ),
            (
                "failed blocked-by select",
                TimelineHomeHydration,
                world().fail_graph(Graph::Blocks),
                vec![1, 2],
            ),
            (
                "failed composite row",
                TimelineHome,
                world().fail_key(Source::TesComposite, 1),
                vec![1],
            ),
            (
                "failed label lookup",
                TimelineHome,
                world().fail_key(Source::SafetyLabels, 1),
                vec![1],
            ),
            (
                "failed exclusive select",
                TimelineHome,
                world()
                    .composite(1, exclusive_tweet())
                    .fail_graph(Graph::SuperFollows),
                vec![1],
            ),
            (
                "failed root-edge select",
                TimelineHomeHydration,
                world()
                    .control(1, control(Community, 30, &[]))
                    .fail_graph(Graph::Follows),
                vec![1],
            ),
            (
                "failed conversation-control row",
                TimelineHomeHydration,
                world().fail_key(Source::TesConversationControl, 1),
                vec![1],
            ),
            (
                "failed country lookup",
                TimelineHomeHydration,
                world()
                    .control(1, control(Co, 30, &["us"]))
                    .control(2, control(Co, 30, &[]))
                    .fault(Source::ViewerCountry, Fault::Fails),
                vec![1],
            ),
            (
                "failed viewer",
                TimelineHome,
                world().fault(Source::GizmoduckViewer, Fault::Fails),
                vec![1, 2],
            ),
        ];
        let raw = [raw(1, Some(10)), raw(2, Some(20))];
        for (name, level, sources, expected) in rows {
            let hydrated = hydrate(&sources, level, Some(VIEWER), &raw).await;
            assert_eq!(hydrated.failed_ids, ids(&expected), "{name}");
        }
    }

    #[tokio::test]
    async fn each_candidate_carries_the_nodes_that_failed_for_it() {
        use ConversationControlArm::MyNetwork;
        use Hydrator::{
            BlockedByReplyRoot, PureCore, RootFollowsViewer, RootFollowsViewerSecondDegree,
            SuperFollowsExclusive, Tweet,
        };
        use SafetyLevel::{TimelineHome, TimelineHomeHydration};
        let world = || InMemorySources::default().tweet(1, 10).tweet(2, 20);
        let rows = [
            (
                "failed composite row",
                TimelineHome,
                Some(VIEWER),
                world()
                    .composite(2, exclusive_tweet())
                    .fail_key(Source::TesComposite, 1),
                [
                    Hydrators::of(Tweet).with(SuperFollowsExclusive),
                    Hydrators::empty(),
                ],
            ),
            (
                "failed root edge on a MyNetwork reply",
                TimelineHomeHydration,
                Some(VIEWER),
                world()
                    .control(1, control(MyNetwork, 30, &[]))
                    .fail_graph(Graph::Follows),
                [
                    Hydrators::of(RootFollowsViewer).with(RootFollowsViewerSecondDegree),
                    Hydrators::empty(),
                ],
            ),
            (
                "failed pure core, authors from the request",
                TimelineHomeHydration,
                Some(VIEWER),
                world().fault(Source::TesPureCore, Fault::Fails),
                [Hydrators::of(PureCore).with(BlockedByReplyRoot); 2],
            ),
            (
                "failed pure core, logged out",
                TimelineHomeHydration,
                None,
                world().fault(Source::TesPureCore, Fault::Fails),
                [Hydrators::of(PureCore); 2],
            ),
        ];
        let raw = [raw(1, Some(10)), raw(2, Some(20))];
        for (name, level, viewer_id, sources, expected) in rows {
            let hydrated = hydrate(&sources, level, viewer_id, &raw).await;
            let failed: Vec<Hydrators> = hydrated.candidates.iter().map(|c| c.failed).collect();
            assert_eq!(failed, expected, "{name}");
        }
    }

    #[tokio::test]
    async fn safety_labels_carry_only_the_tweets_whose_labels_were_found() {
        let sources = InMemorySources::default()
            .tweet(1, 10)
            .tweet(2, 20)
            .tweet(3, 30)
            .labels(2, Default::default())
            .fail_key(Source::SafetyLabels, 1);
        let raw = [raw(1, None), raw(2, None), raw(3, None)];
        let hydrated = hydrate(&sources, SafetyLevel::TimelineHome, None, &raw).await;
        assert_eq!(
            hydrated
                .safety_labels
                .keys()
                .copied()
                .collect::<HashSet<_>>(),
            ids(&[2])
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_timed_out_label_lookup_fails_every_tweet() {
        let sources = InMemorySources::default()
            .tweet(1, 10)
            .fault(Source::SafetyLabels, Fault::Hangs);
        let hydrated = hydrate(&sources, SafetyLevel::TimelineHome, None, &[raw(1, None)]).await;
        assert_eq!(hydrated.failed_ids, ids(&[1]));
        assert!(hydrated.safety_labels.is_empty());
    }

    fn author_keyed(sources: &InMemorySources) -> bool {
        sources.calls().contains(&Source::GizmoduckAuthor) || !sources.selects().is_empty()
    }

    #[tokio::test(start_paused = true)]
    async fn author_keyed_calls_wait_for_pure_core() {
        let sources = InMemorySources::default()
            .tweet(1, 10)
            .fault(Source::TesPureCore, Fault::Hangs);
        let raw = [raw(1, None)];
        let hydration = hydrate(&sources, SafetyLevel::TimelineHome, Some(VIEWER), &raw);
        tokio::pin!(hydration);
        let early = tokio::time::timeout(HYDRATION_TIMEOUT / 2, &mut hydration).await;
        assert!(early.is_err());
        assert!(!author_keyed(&sources));
        let hydrated = hydration.await;
        assert!(!author_keyed(&sources));
        assert!(hydrated.candidates.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn author_calls_do_not_wait_for_the_composite() {
        let sources = InMemorySources::default()
            .tweet(1, 10)
            .user(10, suspended())
            .edge(Graph::Follows, VIEWER, 10)
            .fault(Source::TesComposite, Fault::Hangs);
        let raw = [raw(1, None)];
        let started = tokio::time::Instant::now();
        let hydration = hydrate(&sources, SafetyLevel::TimelineHome, Some(VIEWER), &raw);
        tokio::pin!(hydration);
        let early = tokio::time::timeout(HYDRATION_TIMEOUT / 2, &mut hydration).await;
        assert!(early.is_err());
        assert_eq!(sources.keys(Source::GizmoduckAuthor), [vec![10]]);
        assert_eq!(sources.selects().len(), 1);

        let hydrated = hydration.await;
        assert_eq!(started.elapsed(), HYDRATION_TIMEOUT);
        let candidate = &hydrated.candidates[0];
        assert!(candidate.author_features.is_suspended);
        assert!(candidate.edges.contains(Hydrator::Follows));
        assert_eq!(candidate.tweet_features, Default::default());
        assert_eq!(hydrated.failed_ids, ids(&[1]));
    }

    fn sorted(mut calls: Vec<Source>) -> Vec<String> {
        let mut names: Vec<String> = calls.drain(..).map(|s| format!("{s:?}")).collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn empty_key_sets_and_logged_out_viewers_send_no_call() {
        let raw = [raw(1, None)];
        let logged_out = InMemorySources::default().tweet(1, 10);
        hydrate(&logged_out, SafetyLevel::TimelineHomeHydration, None, &raw).await;
        assert_eq!(
            sorted(logged_out.calls()),
            [
                "GizmoduckAuthor",
                "SafetyLabels",
                "TesComposite",
                "TesConversationControl",
                "TesPureCore"
            ]
        );

        let logged_in = InMemorySources::default().tweet(1, 10);
        hydrate(
            &logged_in,
            SafetyLevel::TimelineHomeHydration,
            Some(VIEWER),
            &raw,
        )
        .await;
        assert!(!logged_in.calls().contains(&Source::ViewerCountry));
        assert_eq!(
            logged_in.selects(),
            [vec![EdgeQuery {
                graph: Graph::Blocks,
                direction: EdgeDirection::Reverse,
                destination_ids: vec![10],
            }]]
        );
    }

    #[tokio::test]
    async fn filter_all_calls_pure_core_only_and_keeps_the_request_side_viewer() {
        let sources = InMemorySources::default()
            .tweet(1, 10)
            .composite(1, exclusive_tweet());
        let hydrated = hydrate(
            &sources,
            SafetyLevel::FilterAll,
            Some(VIEWER),
            &[raw(1, None)],
        )
        .await;
        assert_eq!(sources.calls(), [Source::TesPureCore]);
        assert_eq!(
            hydrated.viewer_features.viewer,
            Viewer::LoggedIn {
                id: VIEWER,
                profile: ViewerProfile::default(),
            }
        );
        assert_eq!(hydrated.viewer_features.country_code.as_deref(), Some("us"));
        assert_eq!(hydrated.candidates[0].author_id, 10);
        assert!(hydrated.safety_labels.is_empty());
        assert!(hydrated.failed_ids.is_empty());
    }

    #[tokio::test]
    async fn a_level_that_plans_the_viewer_profile_decodes_it() {
        let sources = InMemorySources::default().viewer(
            VIEWER,
            ViewerData {
                user_exists: true,
                age_in_years: Some(30),
                ..Default::default()
            },
        );
        let hydrated = hydrate(&sources, SafetyLevel::TimelineHome, Some(VIEWER), &[]).await;
        let Viewer::LoggedIn { profile, .. } = hydrated.viewer_features.viewer else {
            panic!("a logged-in request stays logged in")
        };
        assert_ne!(profile, ViewerProfile::default());
    }

    #[tokio::test]
    async fn exclusive_edges_dedup_conversation_authors() {
        let sources = || {
            InMemorySources::default()
                .tweet(1, 10)
                .tweet(2, 20)
                .composite(1, exclusive_tweet())
                .composite(2, exclusive_tweet())
                .edge(Graph::SuperFollows, VIEWER, 30)
        };
        let raw = [raw(1, None), raw(2, None), raw(1, None), raw(3, Some(40))];
        for viewer_id in [Some(VIEWER), None] {
            let sources = sources();
            let hydrated = hydrate(&sources, SafetyLevel::TimelineHome, viewer_id, &raw).await;
            assert_eq!(sources.keys(Source::TesPureCore), [vec![1, 2, 3]]);
            assert_eq!(sources.keys(Source::TesComposite), [vec![1, 2, 3]]);
            let exclusive = (Some(30), viewer_id.is_some());
            assert_eq!(
                hydrated
                    .candidates
                    .iter()
                    .map(|c| (
                        c.tweet_features.exclusive_conversation_author_id,
                        c.edges.contains(Hydrator::SuperFollowsExclusive)
                    ))
                    .collect::<Vec<_>>(),
                [exclusive, exclusive, exclusive, (None, false)]
            );
            let super_follows: Vec<Vec<u64>> = sources
                .selects()
                .into_iter()
                .flatten()
                .filter(|query| query.graph == Graph::SuperFollows)
                .map(|query| query.destination_ids)
                .collect();
            let expected: &[Vec<u64>] = if viewer_id.is_some() {
                &[vec![30]]
            } else {
                &[]
            };
            assert_eq!(super_follows, expected);
        }
    }

    fn root_edges(sources: &InMemorySources) -> Vec<Vec<EdgeQuery>> {
        sources
            .selects()
            .into_iter()
            .filter(|queries| queries.iter().any(|query| query.graph == Graph::Follows))
            .collect()
    }

    #[tokio::test]
    async fn one_select_carries_both_root_edges_and_a_failure_fails_every_tweet_it_keyed() {
        use ConversationControlArm::{ByInvitation, Community, MyNetwork, Subscribers};
        let world = || {
            InMemorySources::default()
                .tweet(1, 10)
                .tweet(2, 10)
                .tweet(3, 10)
                .tweet(4, 10)
                .control(1, control(Community, 30, &[]))
                .control(2, control(MyNetwork, 30, &[]))
                .control(3, control(Subscribers, 40, &[]))
                .control(4, control(ByInvitation, 40, &[]))
                .edge(Graph::Follows, 30, VIEWER)
                .edge(Graph::SuperFollows, VIEWER, 40)
        };
        let raw = [raw(1, None), raw(2, None), raw(3, None), raw(4, None)];
        let facts = |hydrated: &HydrationOutput| {
            hydrated
                .candidates
                .iter()
                .map(|c| {
                    (
                        c.edges.contains(Hydrator::RootFollowsViewer),
                        c.edges.contains(Hydrator::SuperFollowsRoot),
                    )
                })
                .collect::<Vec<_>>()
        };

        let healthy = world();
        let hydrated = hydrate(
            &healthy,
            SafetyLevel::TimelineHomeHydration,
            Some(VIEWER),
            &raw,
        )
        .await;
        assert_eq!(
            root_edges(&healthy),
            [vec![
                EdgeQuery {
                    graph: Graph::Follows,
                    direction: EdgeDirection::Reverse,
                    destination_ids: vec![30],
                },
                EdgeQuery {
                    graph: Graph::SuperFollows,
                    direction: EdgeDirection::Forward,
                    destination_ids: vec![40],
                },
            ]]
        );
        assert_eq!(
            facts(&hydrated),
            [(true, false), (true, false), (false, true), (false, false)]
        );
        assert!(hydrated.failed_ids.is_empty());

        let failed = world().fail_graph(Graph::Follows);
        let hydrated = hydrate(
            &failed,
            SafetyLevel::TimelineHomeHydration,
            Some(VIEWER),
            &raw,
        )
        .await;
        assert_eq!(facts(&hydrated), [(false, false); 4]);
        assert_eq!(hydrated.failed_ids, ids(&[1, 2, 3]));

        let logged_out = world();
        let hydrated = hydrate(&logged_out, SafetyLevel::TimelineHomeHydration, None, &raw).await;
        assert!(root_edges(&logged_out).is_empty());
        assert_eq!(facts(&hydrated), [(false, false); 4]);
        assert!(hydrated.failed_ids.is_empty());
    }

    #[tokio::test]
    async fn a_query_missing_from_the_select_answer_reads_no_edge_and_fails_no_candidate() {
        let sources = InMemorySources::default()
            .tweet(1, 10)
            .edge(Graph::Follows, VIEWER, 10)
            .edge(Graph::Mutes, VIEWER, 10)
            .miss_graph(Graph::Mutes);
        let hydrated = hydrate(
            &sources,
            SafetyLevel::TimelineHome,
            Some(VIEWER),
            &[raw(1, None)],
        )
        .await;
        assert_eq!(
            hydrated.candidates[0].edges,
            Hydrators::of(Hydrator::Follows)
        );
        assert!(hydrated.failed_ids.is_empty());
    }

    #[tokio::test]
    async fn wingman_asks_once_for_my_network_roots_the_followed_by_edge_answered_no() {
        use ConversationControlArm::{Community, MyNetwork};
        let world = || {
            InMemorySources::default()
                .tweet(1, 10)
                .tweet(2, 10)
                .tweet(3, 10)
                .tweet(4, 10)
                .tweet(5, 10)
                .tweet(6, 10)
                .control(1, control(Community, 30, &[]))
                .control(2, control(MyNetwork, 30, &[]))
                .control(3, control(MyNetwork, 60, &[]))
                .control(4, control(MyNetwork, 60, &[]))
                .control(5, control(MyNetwork, 70, &[]))
                .control(6, control(Community, 80, &[]))
                .edge(Graph::Follows, 30, VIEWER)
                .second_degree_path(60, VIEWER)
        };
        let raw = [1, 2, 3, 4, 5, 6].map(|id| raw(id, None));
        let second_degree = |hydrated: &HydrationOutput| {
            hydrated
                .candidates
                .iter()
                .map(|c| c.edges.contains(Hydrator::RootFollowsViewerSecondDegree))
                .collect::<Vec<_>>()
        };
        let level = SafetyLevel::TimelineHomeHydration;

        let healthy = world();
        let hydrated = hydrate(&healthy, level, Some(VIEWER), &raw).await;
        assert_eq!(healthy.keys(Source::Wingman), [vec![60, 70]]);
        assert_eq!(
            second_degree(&hydrated),
            [false, false, true, true, false, false]
        );
        assert!(hydrated.failed_ids.is_empty());

        let failed = world().fault(Source::Wingman, Fault::Fails);
        let hydrated = hydrate(&failed, level, Some(VIEWER), &raw).await;
        assert_eq!(second_degree(&hydrated), [false; 6]);
        assert!(hydrated.failed_ids.is_empty());

        for (sources, viewer_id, raw) in [
            (world().fail_graph(Graph::Follows), Some(VIEWER), &raw[..]),
            (world(), None, &raw[..]),
            (world(), Some(VIEWER), &raw[..2]),
        ] {
            let hydrated = hydrate(&sources, level, viewer_id, raw).await;
            assert!(sources.keys(Source::Wingman).is_empty());
            assert_eq!(second_degree(&hydrated), vec![false; raw.len()]);
        }
    }

    #[tokio::test]
    async fn one_country_lookup_reaches_every_co_tweet_that_needs_it() {
        use ConversationControlArm::Co;
        let raw = [raw(1, None), raw(2, None)];
        let world = |countries: &[&str]| {
            InMemorySources::default()
                .tweet(1, 10)
                .tweet(2, 10)
                .control(1, control(Co, 30, countries))
                .control(2, control(Co, 30, countries))
                .country(VIEWER, "us")
        };
        let country = |hydrated: &HydrationOutput| {
            hydrated
                .candidates
                .iter()
                .map(|c| {
                    c.conversation_control
                        .as_ref()
                        .unwrap()
                        .viewer_country
                        .as_deref()
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>()
        };

        let listed = world(&["us"]);
        let hydrated = hydrate(
            &listed,
            SafetyLevel::TimelineHomeHydration,
            Some(VIEWER),
            &raw,
        )
        .await;
        assert_eq!(listed.keys(Source::ViewerCountry), [vec![VIEWER]]);
        assert_eq!(
            country(&hydrated),
            [Some("us".to_owned()), Some("us".to_owned())]
        );

        for (sources, viewer_id) in [(world(&[]), Some(VIEWER)), (world(&["us"]), None)] {
            let hydrated = hydrate(
                &sources,
                SafetyLevel::TimelineHomeHydration,
                viewer_id,
                &raw,
            )
            .await;
            assert!(sources.keys(Source::ViewerCountry).is_empty());
            assert_eq!(country(&hydrated), [None, None]);
        }
    }

    #[tokio::test]
    async fn authors_share_one_key_and_a_missing_user_is_complete() {
        let sources = InMemorySources::default().tweet(1, 10).tweet(2, 10);
        let raw = [raw(1, None), raw(2, None)];
        let hydrated = hydrate(&sources, SafetyLevel::TimelineHome, None, &raw).await;
        assert_eq!(sources.keys(Source::GizmoduckAuthor), [vec![10]]);
        assert!(hydrated
            .candidates
            .iter()
            .all(|c| !c.author_features.is_suspended));
        assert!(hydrated.failed_ids.is_empty());
    }

    #[tokio::test]
    async fn the_author_cache_serves_the_last_known_author_when_the_call_fails() {
        let sources = InMemorySources::default()
            .tweet(1, 10)
            .user(10, suspended())
            .with_author_cache(fallback_cache());
        let raw = [raw(1, None)];
        let first = hydrate(&sources, SafetyLevel::TimelineHome, None, &raw).await;
        assert!(first.candidates[0].author_features.is_suspended);

        sources.break_source(Source::GizmoduckAuthor, Fault::Fails);
        let second = hydrate(&sources, SafetyLevel::TimelineHome, None, &raw).await;
        assert!(second.candidates[0].author_features.is_suspended);
        assert!(second.failed_ids.is_empty());
    }

    #[tokio::test]
    async fn the_pure_core_cache_serves_the_last_known_core_when_the_call_fails() {
        let sources = InMemorySources::default()
            .tweet(1, 10)
            .with_pure_core_cache(pure_core_fallback_cache(8));
        let raw = [raw(1, None)];
        let first = hydrate(&sources, SafetyLevel::TimelineHome, None, &raw).await;
        assert_eq!(first.candidates[0].author_id, 10);

        sources.break_source(Source::TesPureCore, Fault::Fails);
        let second = hydrate(&sources, SafetyLevel::TimelineHome, None, &raw).await;
        assert_eq!(second.candidates[0].author_id, 10);
        assert!(second.failed_ids.is_empty());
    }

    #[tokio::test]
    async fn gizmoduck_calls_ask_for_every_field_any_level_reads() {
        use QueryFields::{ACCOUNT, EXTENDED_PROFILE, LABELS, SAFETY};
        for level in [
            SafetyLevel::TimelineHome,
            SafetyLevel::TimelineHomeHydration,
        ] {
            let sources = InMemorySources::default().tweet(1, 10);
            hydrate(&sources, level, Some(VIEWER), &[raw(1, None)]).await;
            assert_eq!(
                sources.fields(Source::GizmoduckViewer),
                [vec![ACCOUNT, EXTENDED_PROFILE, SAFETY]],
                "{level:?}"
            );
            assert_eq!(
                sources.fields(Source::GizmoduckAuthor),
                [vec![SAFETY, LABELS]],
                "{level:?}"
            );
        }
    }

    fn call_labels(sources: &InMemorySources) -> Vec<String> {
        let mut labels: Vec<String> = sources
            .calls()
            .into_iter()
            .filter(|source| *source != Source::Flock)
            .map(|source| format!("{source:?}"))
            .chain(sources.selects().into_iter().map(|queries| {
                let graphs: Vec<&str> = queries.iter().map(|q| <&str>::from(q.graph)).collect();
                format!("Flock {}", graphs.join(","))
            }))
            .collect();
        labels.sort();
        labels
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_source_delays_only_the_calls_waiting_on_it() {
        use ConversationControlArm::{Co, Community, MyNetwork};
        use SafetyLevel::{TimelineHome, TimelineHomeHydration};
        let world = || {
            InMemorySources::default()
                .pure_core(
                    1,
                    PureCoreData {
                        author_id: 10,
                        conversation_id: Some(100),
                        in_reply_to_tweet_id: Some(100),
                        in_reply_to_user_id: Some(30),
                        ..Default::default()
                    },
                )
                .tweet(2, 20)
                .composite(1, exclusive_tweet())
                .control(1, control(Community, 30, &[]))
                .control(2, control(Co, 30, &["us"]))
                .tweet(3, 20)
                .control(3, control(MyNetwork, 40, &[]))
        };
        let rows: [(SafetyLevel, Source, &[&str]); 15] = [
            (
                TimelineHome,
                Source::TesPureCore,
                &[
                    "Flock follows,blocks,mutes,mute_retweets",
                    "GizmoduckAuthor",
                ],
            ),
            (TimelineHome, Source::TesComposite, &["Flock super_follows"]),
            (TimelineHome, Source::SafetyLabels, &[]),
            (TimelineHome, Source::GizmoduckViewer, &[]),
            (TimelineHome, Source::GizmoduckAuthor, &[]),
            (TimelineHome, Source::Flock, &[]),
            (
                TimelineHomeHydration,
                Source::TesPureCore,
                &["Flock blocks", "GizmoduckAuthor"],
            ),
            (
                TimelineHomeHydration,
                Source::TesComposite,
                &["Flock super_follows"],
            ),
            (
                TimelineHomeHydration,
                Source::TesConversationControl,
                &["Flock follows,super_follows", "ViewerCountry", "Wingman"],
            ),
            (TimelineHomeHydration, Source::SafetyLabels, &[]),
            (TimelineHomeHydration, Source::GizmoduckViewer, &[]),
            (TimelineHomeHydration, Source::GizmoduckAuthor, &[]),
            (TimelineHomeHydration, Source::Flock, &["Wingman"]),
            (TimelineHomeHydration, Source::ViewerCountry, &[]),
            (TimelineHomeHydration, Source::Wingman, &[]),
        ];
        let raw = [raw(1, None), raw(2, None), raw(3, None)];
        for (level, hung, waiting) in rows {
            let healthy = world();
            hydrate(&healthy, level, Some(VIEWER), &raw).await;
            let every_call = call_labels(&healthy);
            let groups = RuleEngine::for_tests().plan(level).groups().count();
            assert_eq!(every_call.len(), groups, "{level:?}: {every_call:?}");
            assert!(
                waiting
                    .iter()
                    .all(|call| every_call.iter().any(|c| c == call)),
                "{level:?} {hung:?}: {every_call:?}"
            );

            let sources = world().fault(hung, Fault::Hangs);
            let started = tokio::time::Instant::now();
            let hydration = hydrate(&sources, level, Some(VIEWER), &raw);
            tokio::pin!(hydration);
            let early = tokio::time::timeout(HYDRATION_TIMEOUT / 2, &mut hydration).await;
            assert!(early.is_err(), "{level:?} {hung:?}");
            let expected: Vec<String> = every_call
                .iter()
                .filter(|call| !waiting.contains(&call.as_str()))
                .cloned()
                .collect();
            assert_eq!(call_labels(&sources), expected, "{level:?} {hung:?}");
            hydration.await;
            assert_eq!(started.elapsed(), HYDRATION_TIMEOUT, "{level:?} {hung:?}");
        }
    }
}

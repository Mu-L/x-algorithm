use crate::clients::about_this_account_client::AboutThisAccountClient;
use crate::clients::gizmoduck_client::GizmoduckLookup;
use crate::clients::socialgraph_client::{EdgeQuery, SocialgraphClient};
use crate::clients::wingman_client::WingmanClient;
use crate::hydration::batch::{Hydrated, HydrationBatch, HydrationError, RawHydrationBatch};
use crate::hydration::decode::author::{decode_authors, AuthorFallbackCache, DecodedAuthor};
use crate::hydration::decode::tweet::{pure_core, PureCoreFallbackCache};
use crate::hydration::decode::viewer::viewer_profile;
use crate::hydration::tes_composite::{TweetForVisibility, TweetForVisibilitySource};
use crate::hydration::Hydrators;
use crate::models::{PureCore, ViewerProfile};
use crate::safety_label_source::SafetyLabelSource;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::warn;
use wingman_client::Exists;
use xai_core_entities::entities::ConversationControl;
use xai_core_entities::gizmoduck_client::{GizmoduckClient, QueryFields};
use xai_core_entities::tweet_entity_service_client::TESClient;
use xai_visibility_filtering_proto as vf_pb;

#[tonic::async_trait]
pub(crate) trait Sources: Send + Sync {
    async fn pure_cores(&self, tweet_ids: Vec<u64>) -> RawHydrationBatch<PureCore>;

    async fn tweets(&self, tweet_ids: Vec<u64>) -> RawHydrationBatch<TweetForVisibility>;

    async fn conversation_controls(
        &self,
        tweet_ids: Vec<u64>,
    ) -> RawHydrationBatch<ConversationControl>;

    async fn safety_labels(
        &self,
        tweet_ids: Vec<u64>,
    ) -> RawHydrationBatch<Arc<vf_pb::SafetyLabelMap>>;

    async fn viewer(
        &self,
        viewer_id: u64,
        fields: &[QueryFields],
    ) -> RawHydrationBatch<ViewerProfile>;

    async fn users(
        &self,
        user_ids: Vec<u64>,
        fields: &[QueryFields],
    ) -> RawHydrationBatch<DecodedAuthor>;

    async fn select_edges(
        &self,
        viewer_id: u64,
        queries: &[EdgeQuery],
        nodes: &[Hydrators],
    ) -> RawHydrationBatch<Hydrators>;

    async fn viewer_country(&self, viewer_id: u64) -> RawHydrationBatch<Arc<str>>;

    async fn second_degree(
        &self,
        viewer_id: u64,
        root_author_ids: Vec<u64>,
    ) -> RawHydrationBatch<bool>;

    fn pure_core_cache(&self) -> Option<&PureCoreFallbackCache> {
        None
    }

    fn author_cache(&self) -> Option<&AuthorFallbackCache> {
        None
    }
}

fn landed_edges(
    queries: &[EdgeQuery],
    nodes: &[Hydrators],
    sets: Option<Vec<Option<HashSet<u64>>>>,
) -> RawHydrationBatch<Hydrators> {
    let destinations = queries
        .iter()
        .flat_map(|query| query.destination_ids.iter().copied());
    let Some(sets) = sets else {
        return HydrationBatch::from_hydrated(
            destinations
                .map(|destination| (destination, Hydrated::Failed(HydrationError::Error)))
                .collect(),
        );
    };
    let missing: HashSet<u64> = queries
        .iter()
        .zip(&sets)
        .filter(|(_, set)| set.is_none())
        .flat_map(|(query, _)| query.destination_ids.iter().copied())
        .collect();
    let mut landed: HashMap<u64, Hydrated<Hydrators>> = destinations
        .map(|destination| {
            let holds = Hydrators::empty();
            let answer = if missing.contains(&destination) {
                Hydrated::Partial(holds)
            } else {
                Hydrated::Found(holds)
            };
            (destination, answer)
        })
        .collect();
    for ((query, nodes), set) in queries.iter().zip(nodes).zip(&sets) {
        let Some(set) = set else { continue };
        for destination in query.destination_ids.iter().filter(|id| set.contains(id)) {
            if let Some(Hydrated::Found(holds) | Hydrated::Partial(holds)) =
                landed.get_mut(destination)
            {
                *holds = holds.union(*nodes);
            }
        }
    }
    HydrationBatch::from_hydrated(landed)
}

pub(crate) struct ProdSources {
    tes: Arc<dyn TESClient + Send + Sync>,
    composite: Arc<dyn TweetForVisibilitySource>,
    gizmoduck: Arc<dyn GizmoduckClient + Send + Sync>,
    authors: GizmoduckLookup,
    socialgraph: Arc<dyn SocialgraphClient + Send + Sync>,
    about_this_account: Arc<dyn AboutThisAccountClient>,
    wingman: Arc<dyn WingmanClient>,
    safety_labels: Arc<SafetyLabelSource>,
    author_cache: Option<AuthorFallbackCache>,
    pure_core_cache: Option<PureCoreFallbackCache>,
}

impl ProdSources {
    #[expect(
        clippy::too_many_arguments,
        reason = "one argument per backend and cache"
    )]
    pub(crate) fn new(
        tes: Arc<dyn TESClient + Send + Sync>,
        composite: Arc<dyn TweetForVisibilitySource>,
        gizmoduck: Arc<dyn GizmoduckClient + Send + Sync>,
        socialgraph: Arc<dyn SocialgraphClient + Send + Sync>,
        about_this_account: Arc<dyn AboutThisAccountClient>,
        wingman: Arc<dyn WingmanClient>,
        safety_labels: Arc<SafetyLabelSource>,
        author_cache: Option<AuthorFallbackCache>,
        pure_core_cache: Option<PureCoreFallbackCache>,
    ) -> Self {
        Self {
            tes,
            composite,
            authors: GizmoduckLookup::new(gizmoduck.clone()),
            gizmoduck,
            socialgraph,
            about_this_account,
            wingman,
            safety_labels,
            author_cache,
            pure_core_cache,
        }
    }
}

#[tonic::async_trait]
impl Sources for ProdSources {
    async fn pure_cores(&self, tweet_ids: Vec<u64>) -> RawHydrationBatch<PureCore> {
        let cores = self.tes.get_tweet_core_datas(tweet_ids.clone()).await;
        HydrationBatch::from_results(tweet_ids, cores).map(|core| pure_core(&core))
    }

    async fn tweets(&self, tweet_ids: Vec<u64>) -> RawHydrationBatch<TweetForVisibility> {
        let tweets = self.composite.get_tweets_for_visibility(&tweet_ids).await;
        HydrationBatch::from_results(tweet_ids, tweets)
    }

    async fn conversation_controls(
        &self,
        tweet_ids: Vec<u64>,
    ) -> RawHydrationBatch<ConversationControl> {
        let controls = self.tes.get_conversation_controls(tweet_ids.clone()).await;
        HydrationBatch::from_results(tweet_ids, controls)
    }

    async fn safety_labels(
        &self,
        tweet_ids: Vec<u64>,
    ) -> RawHydrationBatch<Arc<vf_pb::SafetyLabelMap>> {
        let labels = self
            .safety_labels
            .get(&tweet_ids)
            .await
            .into_iter()
            .map(|(id, labels)| (id, labels.map(Some)))
            .collect();
        HydrationBatch::from_results(tweet_ids, labels)
    }

    async fn viewer(
        &self,
        viewer_id: u64,
        fields: &[QueryFields],
    ) -> RawHydrationBatch<ViewerProfile> {
        let profile = self
            .gizmoduck
            .get_viewer_data_with_fields(viewer_id, fields)
            .await
            .inspect_err(|error| warn!(%error, "Gizmoduck viewer lookup failed; failing open"))
            .map(|data| Some(viewer_profile(data)));
        HydrationBatch::from_results([viewer_id], HashMap::from([(viewer_id, profile)]))
    }

    async fn users(
        &self,
        user_ids: Vec<u64>,
        fields: &[QueryFields],
    ) -> RawHydrationBatch<DecodedAuthor> {
        let users = self.authors.get_users(user_ids.clone(), fields).await;
        decode_authors(HydrationBatch::from_results(user_ids, users))
    }

    async fn select_edges(
        &self,
        viewer_id: u64,
        queries: &[EdgeQuery],
        nodes: &[Hydrators],
    ) -> RawHydrationBatch<Hydrators> {
        let sets = self.socialgraph.select_edges(viewer_id, queries).await;
        landed_edges(queries, nodes, sets)
    }

    async fn viewer_country(&self, viewer_id: u64) -> RawHydrationBatch<Arc<str>> {
        let country = self
            .about_this_account
            .tfe_top_country(viewer_id)
            .await
            .inspect_err(|error| warn!(%error, "tfe_top_country lookup failed"))
            .map(|country| country.map(Arc::from));
        HydrationBatch::from_results([viewer_id], HashMap::from([(viewer_id, country)]))
    }

    async fn second_degree(
        &self,
        viewer_id: u64,
        root_author_ids: Vec<u64>,
    ) -> RawHydrationBatch<bool> {
        let answers = self
            .wingman
            .batch_exists_intersect(viewer_id, &root_author_ids)
            .await;
        let answers = root_author_ids
            .iter()
            .copied()
            .zip(answers.into_iter().flatten())
            .map(|(root, answer)| {
                let answer = match answer {
                    Exists::Found => Ok(Some(true)),
                    Exists::NotFound => Ok(Some(false)),
                    Exists::Incomplete | Exists::ItemError => Err(answer),
                };
                (root, answer)
            })
            .collect();
        HydrationBatch::from_results(root_author_ids, answers)
    }

    fn pure_core_cache(&self) -> Option<&PureCoreFallbackCache> {
        self.pure_core_cache.as_ref()
    }

    fn author_cache(&self) -> Option<&AuthorFallbackCache> {
        self.author_cache.as_ref()
    }
}

#[cfg(test)]
pub(crate) use in_memory::{Fault, InMemorySources};

#[cfg(test)]
mod in_memory {
    use super::*;
    use crate::clients::socialgraph_client::{EdgeDirection, Graph};
    use crate::hydration::plan::Source;
    use std::sync::Mutex;
    use xai_core_entities::entities::{GizmoduckUserResult, PureCoreData};
    use xai_core_entities::gizmoduck_client::ViewerData;

    #[derive(Clone, Copy, Debug)]
    pub(crate) enum Fault {
        Fails,
        Hangs,
    }

    #[derive(Default)]
    pub(crate) struct InMemorySources {
        pure_cores: HashMap<u64, PureCoreData>,
        tweets: HashMap<u64, TweetForVisibility>,
        controls: HashMap<u64, ConversationControl>,
        labels: HashMap<u64, Arc<vf_pb::SafetyLabelMap>>,
        viewers: HashMap<u64, ViewerData>,
        users: HashMap<u64, GizmoduckUserResult>,
        edges: HashSet<(Graph, u64, u64)>,
        countries: HashMap<u64, Arc<str>>,
        second_degree: HashSet<(u64, u64)>,
        faults: Mutex<Vec<(Source, Fault)>>,
        failed_keys: HashSet<(Source, u64)>,
        failed_graphs: HashSet<Graph>,
        missing_graphs: HashSet<Graph>,
        author_cache: Option<AuthorFallbackCache>,
        pure_core_cache: Option<PureCoreFallbackCache>,
        calls: Mutex<Vec<(Source, Vec<u64>)>>,
        selects: Mutex<Vec<Vec<EdgeQuery>>>,
        fields: Mutex<Vec<(Source, Vec<QueryFields>)>>,
    }

    impl InMemorySources {
        pub(crate) fn tweet(self, tweet_id: u64, author_id: u64) -> Self {
            self.pure_core(
                tweet_id,
                PureCoreData {
                    author_id,
                    ..Default::default()
                },
            )
        }

        pub(crate) fn pure_core(mut self, tweet_id: u64, core: PureCoreData) -> Self {
            self.pure_cores.insert(tweet_id, core);
            self
        }

        pub(crate) fn composite(mut self, tweet_id: u64, tweet: TweetForVisibility) -> Self {
            self.tweets.insert(tweet_id, tweet);
            self
        }

        pub(crate) fn control(mut self, tweet_id: u64, control: ConversationControl) -> Self {
            self.controls.insert(tweet_id, control);
            self
        }

        pub(crate) fn labels(mut self, tweet_id: u64, labels: vf_pb::SafetyLabelMap) -> Self {
            self.labels.insert(tweet_id, Arc::new(labels));
            self
        }

        pub(crate) fn viewer(mut self, viewer_id: u64, data: ViewerData) -> Self {
            self.viewers.insert(viewer_id, data);
            self
        }

        pub(crate) fn user(mut self, user_id: u64, user: GizmoduckUserResult) -> Self {
            self.users.insert(user_id, user);
            self
        }

        pub(crate) fn edge(mut self, graph: Graph, source: u64, destination: u64) -> Self {
            self.edges.insert((graph, source, destination));
            self
        }

        pub(crate) fn country(mut self, viewer_id: u64, country: &str) -> Self {
            self.countries.insert(viewer_id, Arc::from(country));
            self
        }

        pub(crate) fn second_degree_path(mut self, root_author: u64, viewer_id: u64) -> Self {
            self.second_degree.insert((root_author, viewer_id));
            self
        }

        pub(crate) fn fault(self, source: Source, fault: Fault) -> Self {
            self.break_source(source, fault);
            self
        }

        pub(crate) fn break_source(&self, source: Source, fault: Fault) {
            self.faults.lock().unwrap().push((source, fault));
        }

        pub(crate) fn fail_graph(mut self, graph: Graph) -> Self {
            self.failed_graphs.insert(graph);
            self
        }

        pub(crate) fn miss_graph(mut self, graph: Graph) -> Self {
            self.missing_graphs.insert(graph);
            self
        }

        pub(crate) fn fail_key(mut self, source: Source, key: u64) -> Self {
            self.failed_keys.insert((source, key));
            self
        }

        pub(crate) fn with_author_cache(mut self, cache: AuthorFallbackCache) -> Self {
            self.author_cache = Some(cache);
            self
        }

        pub(crate) fn with_pure_core_cache(mut self, cache: PureCoreFallbackCache) -> Self {
            self.pure_core_cache = Some(cache);
            self
        }

        pub(crate) fn calls(&self) -> Vec<Source> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|(source, _)| *source)
                .collect()
        }

        pub(crate) fn keys(&self, source: Source) -> Vec<Vec<u64>> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(called, _)| *called == source)
                .map(|(_, keys)| keys.clone())
                .collect()
        }

        pub(crate) fn selects(&self) -> Vec<Vec<EdgeQuery>> {
            self.selects.lock().unwrap().clone()
        }

        pub(crate) fn fields(&self, source: Source) -> Vec<Vec<QueryFields>> {
            self.fields
                .lock()
                .unwrap()
                .iter()
                .filter(|(called, _)| *called == source)
                .map(|(_, fields)| fields.clone())
                .collect()
        }

        fn record_fields(&self, source: Source, fields: &[QueryFields]) {
            self.fields.lock().unwrap().push((source, fields.to_vec()));
        }

        async fn enter(&self, source: Source, keys: &[u64]) -> bool {
            let mut keys = keys.to_vec();
            keys.sort_unstable();
            self.calls.lock().unwrap().push((source, keys));
            let fault = self
                .faults
                .lock()
                .unwrap()
                .iter()
                .find(|(faulty, _)| *faulty == source)
                .map(|(_, fault)| *fault);
            match fault {
                Some(Fault::Hangs) => std::future::pending().await,
                Some(Fault::Fails) => true,
                None => false,
            }
        }

        async fn keyed<V: Clone>(
            &self,
            source: Source,
            ids: Vec<u64>,
            values: &HashMap<u64, V>,
        ) -> RawHydrationBatch<V> {
            let fails = self.enter(source, &ids).await;
            let results = ids
                .iter()
                .map(|&id| {
                    let result = if fails || self.failed_keys.contains(&(source, id)) {
                        Err(())
                    } else {
                        Ok(values.get(&id).cloned())
                    };
                    (id, result)
                })
                .collect();
            HydrationBatch::from_results(ids, results)
        }
    }

    #[tonic::async_trait]
    impl Sources for InMemorySources {
        async fn pure_cores(&self, tweet_ids: Vec<u64>) -> RawHydrationBatch<PureCore> {
            self.keyed(Source::TesPureCore, tweet_ids, &self.pure_cores)
                .await
                .map(|core| pure_core(&core))
        }

        async fn tweets(&self, tweet_ids: Vec<u64>) -> RawHydrationBatch<TweetForVisibility> {
            self.keyed(Source::TesComposite, tweet_ids, &self.tweets)
                .await
        }

        async fn conversation_controls(
            &self,
            tweet_ids: Vec<u64>,
        ) -> RawHydrationBatch<ConversationControl> {
            self.keyed(Source::TesConversationControl, tweet_ids, &self.controls)
                .await
        }

        async fn safety_labels(
            &self,
            tweet_ids: Vec<u64>,
        ) -> RawHydrationBatch<Arc<vf_pb::SafetyLabelMap>> {
            self.keyed(Source::SafetyLabels, tweet_ids, &self.labels)
                .await
        }

        async fn viewer(
            &self,
            viewer_id: u64,
            fields: &[QueryFields],
        ) -> RawHydrationBatch<ViewerProfile> {
            self.record_fields(Source::GizmoduckViewer, fields);
            self.keyed(Source::GizmoduckViewer, vec![viewer_id], &self.viewers)
                .await
                .map(viewer_profile)
        }

        async fn users(
            &self,
            user_ids: Vec<u64>,
            fields: &[QueryFields],
        ) -> RawHydrationBatch<DecodedAuthor> {
            self.record_fields(Source::GizmoduckAuthor, fields);
            decode_authors(
                self.keyed(Source::GizmoduckAuthor, user_ids, &self.users)
                    .await,
            )
        }

        async fn select_edges(
            &self,
            viewer_id: u64,
            queries: &[EdgeQuery],
            nodes: &[Hydrators],
        ) -> RawHydrationBatch<Hydrators> {
            let mut recorded = queries.to_vec();
            for query in &mut recorded {
                query.destination_ids.sort_unstable();
            }
            self.selects.lock().unwrap().push(recorded);
            let failed_graph = queries
                .iter()
                .any(|query| self.failed_graphs.contains(&query.graph));
            let fails = self.enter(Source::Flock, &[viewer_id]).await || failed_graph;
            let answer = |query: &EdgeQuery| {
                let holds = |&id: &u64| {
                    let edge = match query.direction {
                        EdgeDirection::Forward => (query.graph, viewer_id, id),
                        EdgeDirection::Reverse => (query.graph, id, viewer_id),
                    };
                    self.edges.contains(&edge)
                };
                (!self.missing_graphs.contains(&query.graph)).then(|| {
                    query
                        .destination_ids
                        .iter()
                        .copied()
                        .filter(holds)
                        .collect()
                })
            };
            let sets = (!fails).then(|| queries.iter().map(answer).collect());
            landed_edges(queries, nodes, sets)
        }

        async fn viewer_country(&self, viewer_id: u64) -> RawHydrationBatch<Arc<str>> {
            self.keyed(Source::ViewerCountry, vec![viewer_id], &self.countries)
                .await
        }

        async fn second_degree(
            &self,
            viewer_id: u64,
            root_author_ids: Vec<u64>,
        ) -> RawHydrationBatch<bool> {
            let paths = root_author_ids
                .iter()
                .map(|&root| (root, self.second_degree.contains(&(root, viewer_id))))
                .collect();
            self.keyed(Source::Wingman, root_author_ids, &paths).await
        }

        fn pure_core_cache(&self) -> Option<&PureCoreFallbackCache> {
            self.pure_core_cache.as_ref()
        }

        fn author_cache(&self) -> Option<&AuthorFallbackCache> {
            self.author_cache.as_ref()
        }
    }
}

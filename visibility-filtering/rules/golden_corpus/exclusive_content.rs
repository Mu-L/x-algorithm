use super::{Role, Row};
use crate::hydration::Hydrator;
use crate::models::HydratedTweetCandidate;
use crate::rules::fixtures::{allow, candidate, dropped, CandidateBuilder};
use crate::rules::SafetyLevel::TimelineHome;
use xai_visibility_filtering::models::FilteredReason;

pub(super) fn rows() -> Vec<Row> {
    vec![
        Row {
            name: "exclusive",
            post: exclusive_candidate(candidate()),
            expect: vec![
                (
                    TimelineHome,
                    Role::NonFollower,
                    dropped(
                        FilteredReason::ExclusiveTweet,
                        "DropExclusiveTweetContentRule",
                    ),
                ),
                (TimelineHome, Role::Author, allow()),
                (
                    TimelineHome,
                    Role::LoggedOut,
                    dropped(
                        FilteredReason::ExclusiveTweet,
                        "DropExclusiveTweetContentRule",
                    ),
                ),
            ],
        },
        Row {
            name: "exclusive_super_followed",
            post: exclusive_candidate(candidate().with_edge(Hydrator::SuperFollowsExclusive)),
            expect: vec![(TimelineHome, Role::NonFollower, allow())],
        },
        Row {
            name: "exclusive_retweet",
            post: {
                let mut retweet = exclusive_candidate(candidate());
                retweet.tweet_features.source_tweet_id = Some(2);
                retweet
            },
            expect: vec![(
                TimelineHome,
                Role::Author,
                dropped(
                    FilteredReason::ExclusiveTweet,
                    "DropExclusiveTweetContentRule",
                ),
            )],
        },
    ]
}

fn exclusive_candidate(builder: CandidateBuilder) -> HydratedTweetCandidate {
    let mut c = builder.build();
    c.tweet_features.exclusive_conversation_author_id = Some(42);
    c
}

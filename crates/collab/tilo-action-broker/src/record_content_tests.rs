/// Two hosts on one record's route, each with an app on it: a picture one
/// app hands its host reaches the other app through the other host, in
/// verified pieces, as content its record references, after the app
/// heard it being fetched.
#[tokio::test(flavor = "current_thread")]
async fn a_picture_one_app_offers_reaches_an_app_on_another_host() {
    use runtime_collab::BlobState;
    use signed_worker::OriginalRoom;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (content_a, content_b, _) = crate::record_content::tests::pair(true);
            let host_a = OriginalRoom::with_record_content(content_a, None);
            let room = tilo_desktop_model::RoomId::parse(&host_a.route.cluster_id);
            let host_b = OriginalRoom::with_record_content(content_b, room);
            assert_eq!(host_a.route, host_b.route, "both hosts hold the one record");
            let (app_a, _) = host_a.join_worker(901).await;
            let (app_b, _) = host_b.join_worker(902).await;

            let picture: Vec<u8> = (0..crate::record_content::PIECE_BYTES * 2 + 5)
                .map(|index| u8::try_from(index % 253).unwrap())
                .collect();
            let hash = tilo_content::hash_bytes(&picture);
            app_a.handle.offer_blob(&hash, &picture).await.unwrap();
            assert!(
                app_a.handle.offer_blob(&hash, b"forged").await.is_err(),
                "a host keeps only bytes that are their address"
            );

            let news = app_b.handle.subscribe_blobs().await.unwrap();
            let next = || async {
                tokio::time::timeout(tilo_test_env::hang_bound(), news.read())
                    .await
                    .expect("news of the picture")
                    .unwrap()
            };
            app_b.handle.want_blob(&hash).await.unwrap();
            let fetching = next().await;
            assert_eq!(
                (fetching.hash.as_str(), fetching.state),
                (hash.as_str(), BlobState::Fetching),
                "the app hears its want is fetched from the host that holds it"
            );
            assert!(fetching.bytes.is_empty());
            let arrived = next().await;
            assert_eq!(
                (arrived.hash.as_str(), arrived.state),
                (hash.as_str(), BlobState::Arrived)
            );
            assert_eq!(arrived.bytes, picture);
        })
        .await;
}

/// An app on a host that keeps no record content is told so, rather than
/// waiting for an answer that never comes.
#[tokio::test(flavor = "current_thread")]
async fn a_host_without_record_content_says_so() {
    use signed_worker::OriginalRoom;
    tokio::task::LocalSet::new()
        .run_until(async {
            let host =
                OriginalRoom::new(tilo_user_identity::DeviceCapabilities::host(), None, None);
            let (app, _) = host.join_worker(901).await;
            assert!(app.handle.want_blob(&"0".repeat(64)).await.is_err());
        })
        .await;
}

/// An app asking for a picture whose only holder is offline hears that it
/// waits; the moment the holder connects it is fetched, and arrives. Two
/// hosts, each with an app on the record, as two devices run them.
#[tokio::test(flavor = "current_thread")]
async fn an_app_hears_its_want_wait_for_an_offline_holder_and_the_picture_arrive_when_it_connects()
{
    use runtime_collab::BlobState;
    use signed_worker::OriginalRoom;
    tokio::task::LocalSet::new()
        .run_until(async {
            let pair = crate::record_content::tests::hosts(false, true);
            let host_a = OriginalRoom::with_record_content(pair.a.clone(), None);
            let room = tilo_desktop_model::RoomId::parse(&host_a.route.cluster_id);
            let host_b = OriginalRoom::with_record_content(pair.b.clone(), room);
            let (app_a, _) = host_a.join_worker(903).await;
            let (app_b, _) = host_b.join_worker(904).await;

            let picture = vec![9_u8; crate::record_content::PIECE_BYTES + 3];
            let hash = tilo_content::hash_bytes(&picture);
            app_a.handle.offer_blob(&hash, &picture).await.unwrap();

            let news = app_b.handle.subscribe_blobs().await.unwrap();
            let next = || async {
                tokio::time::timeout(tilo_test_env::hang_bound(), news.read())
                    .await
                    .expect("news of the picture")
                    .unwrap()
            };
            app_b.handle.want_blob(&hash).await.unwrap();
            let waiting = next().await;
            assert_eq!(
                (waiting.hash.as_str(), waiting.state),
                (hash.as_str(), BlobState::Waiting)
            );
            assert!(waiting.bytes.is_empty());

            pair.connect(true);
            assert_eq!(
                next().await.state,
                BlobState::Fetching,
                "asked of the holder"
            );
            let arrived = next().await;
            assert_eq!(arrived.state, BlobState::Arrived);
            assert_eq!(arrived.bytes, picture);
        })
        .await;
}

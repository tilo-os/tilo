# Source-pinned MoQ Rust adapter

Upstream: https://github.com/moq-dev/moq

Revision: `3aedaf3503146c664479f6fc53a50b36bb5e816d` (moq-net 0.2.22; kio 0.5.9; moq-native 0.19.19).

License: MIT OR Apache-2.0; both upstream licenses accompany this copy.

Local patches:

- Expanded inherited manifest fields from the pinned root; local relative crate paths.
- Quinn + ring is the supported native dependency closure. Optional noq, iroh, quiche, qmux, websocket, tcp and uds manifest features/dependencies are removed. Their upstream source files remain as provenance, but are not supported build combinations. Native consumers explicitly select `default-features = false, features = ["quinn", "ring"]` and negotiate only `moqt-21`.
- Disabled default AWS features in web-transport-quinn and selected ring in rustls/webpki/rcgen. This preserves the existing workspace rustls 0.23.45 and webpki 0.103.15 security versions. The upstream general TLS helper retains `tls12`; QUIC uses TLS 1.3.
- Aligned the wasm-only test runner to wasm-bindgen-test 0.3.58 (the existing workspace wasm-bindgen 0.2.108 ABI); kept native test toml at the existing 1.0.6 version. No upstream security dependency floor was lowered.
- Bounded standard Draft 21 SETUP authorization, client emission and pre-admission Request access. The parser preserves up to 16 standard tokens with at most 8192 aggregate opaque payload bytes; Type 0 is the out-of-band `tilo-media-v1` agreement. MOQT permits repeated AUTHORIZATION_TOKEN options. The compatible authorization() accessor selects a proof only when exactly one Type 0 token is present; multiple Type 0 tokens are ambiguous and yield no proof for host admission. Unknown token types never confer a grant; all tokens remain accessible through authorization_tokens(). Unknown duplicate SETUP options are ignored; recognized non-repeatable options still reject duplicates. Grants belong in this option, never URLs or logs.
- Native transport + SETUP queue has a default ceiling of 64 pending handshakes and 3 seconds, configurable downward with `Server::with_accept_limits`.
- Bounded control and extension reads before allocation, incremental Reader decoding, standard Draft 21 session error mapping, and fatal propagation of malformed/unauthorized media. SETUP has a separate u16 body budget (65535 + up to 11 framing bytes); ordinary post-SETUP control bodies have an 8192-byte ceiling, while GOAWAY permits 8192 URI bytes plus up to 18 bytes for its URI length and timeout; object extension blocks have a 4096-byte ceiling.
- Origins carrying the opt-in voice model policy enable session-wide 32-request and 32-group child-future admission shared across publisher/subscriber clones. Permits live until each admitted future completes or is cancelled, including outgoing group and FILL futures. Each nested subscription child acquires its own request permit; a long-lived dynamic-broadcast coordinator also retains one request slot.
- The model policy patches (exact namespace scope, hard payload/cache bounds and expiry) are maintained by the model-limit implementation task. See that task's report for ownership and validation.
- Media hardening (2026-09-22), voice group slots: an evicted, expired, aborted or dropped voice group now returns its per-broadcast group slot at once (`model/group.rs`, `model/origin.rs` `Budget::acquire_group_slot`), and admission of a fourth concurrent group first expires and then evicts the oldest slot holder, refusing only as stream-scoped `Error::Old`. Previously a lossy or jittery honest publisher (earlier group streams still open while QUIC retransmits) hit `ProtocolViolation`, which `run_unis` turned into a session-fatal CLOSE_WEBTRANSPORT_SESSION(3, "invalid media stream"); that server close is what triggered the Chromium renderer crash (see the media-hardening report). Memory stays bounded by the 16 KiB byte budget and the session group admission.
- Media hardening, voice source/track slots: a path's live-source slot and its track slots are returned when that source's broadcast ends (`run_source` exit in `model/origin.rs`; revocable track slot in `model/cache.rs`), not when the last `Info`/track clone drops. A subscriber on another session (possibly itself in an outage) could hold the old broadcast for seconds, so an honest publisher reconnect hit `ProtocolViolation` (session-fatal) repeatedly. A second *live* source for the same path is still refused. Cached bytes stay charged to the byte budget until released.
- Media hardening, Draft 21 OBJECT_DATAGRAM (section 11.2.1): `ietf/datagram.rs` (codec), `ietf/publisher.rs` (a subscribed track's model datagrams leave as single-object OBJECT_DATAGRAMs, Group ID = datagram sequence, Object ID 0, END_OF_GROUP), `ietf/subscriber.rs` + `ietf/session.rs` (receive loop routing by track alias into the subscription's track producer). Malformed, status or unknown-alias datagrams are dropped rather than closing the session (a deliberate robustness deviation from the draft's MUST-close for invalid Type Flags). `model/track.rs` admits datagrams on voice-limited origins: 4 KiB object ceiling, the origin frame validator, and at most 8 buffered per track in addition to the 50 ms age bound.
- Media hardening, admission signal: `Session::setup_received()` (`session.rs`, `client.rs`, `ietf/session.rs`) resolves when the peer's SETUP is read. A gated relay sends SETUP only after host admission, so a Draft 21 client can distinguish an admitted session from an open transport.
- Media hardening, moq-native: `Server::with_accept_limits` accepts up to 10 s (was 3 s) so a satellite-class path (600 ms RTT plus one loss recovery) can finish the handshake.

The canonical cross-language fixture is opaque bytes `68 69` (hex) encoded as SETUP option `03 04 03 00 68 69`: option 3, option length 4, USE_VALUE 3, Type 0, opaque bytes. There is no inner proof length. Alias REGISTER with the default zero token cache is interpreted as USE_VALUE as required by Draft 21; DELETE/USE_ALIAS are protocol errors in SETUP.

These are local application-profile patches to a maintained upstream implementation, not a claim that every optional Draft 21 feature is supported. Production authorization is the host's responsibility before attaching origins; the protocol hook does not create a grant or verify application identities. Proof tests do not qualify a production deployment.

## Upstream file SHA-256 manifest (before local patches)

```text
cc1057339e51da5cb6fb6498747700ef177a986af1369941101494fa8195c659  rs/kio/CHANGELOG.md
dc14397bca68464a4bb054488f75c53f15742a4f4ce01ec33afebb01e1e56a5e  rs/kio/Cargo.toml
5d263c39a0ac82af0fa7f5e3dd277aa8b7218e3c2f3cb8989d830baa04d5f613  rs/kio/README.md
ce7ce9fba01965bba3132ac49cc866b901f8823e1c7acce9d63d9593d9068fe3  rs/kio/benches/channel.rs
0f44d21a48ae0bd7897fa9f92154e9692b601e4748b7277a729acd62a71f4d89  rs/kio/benches/waiter.rs
81d040cfdcf28b2c1b7e948b4925b9069a4ccb413535402c229a6f88b95c0609  rs/kio/src/consumer.rs
f6db0ce47603948a7bf3fdaf9f058b98c3544628fc597b51abfa6527138c36da  rs/kio/src/lib.rs
0d65371388d315ee879452e238b055bbdce3fc30b6ce676480f66d0fa0e8cea5  rs/kio/src/lock.rs
d4598150bf898694f9aaaddba6bf5df4ac3e557e80fae4810a79e95c15bf66b6  rs/kio/src/loom.rs
11793443dd00e9a1af416a9397c28223b95d748a15c98145fe0cc47ff198e299  rs/kio/src/pollable.rs
35e11960e4e6fcc8444086435707fb44ab9b9c8c3c51c06941344402b0a41f35  rs/kio/src/producer.rs
5283497587a84e60defc07ffed11b15097ee5146af4431f79b22973d77df38bf  rs/kio/src/queue.rs
398dd7575c2bd0eaabffd1ec4a3042ef23d5e95e16323ad68fa73d6247798314  rs/kio/src/send.rs
a4c07bd69374808584790cec62bea426839bcfc4d326a157c1c4eb0ccd5b70c2  rs/kio/src/shared.rs
1a1571b45cab09cbed7b76af2602f09eba9ccb7b10d0163ddc1429bc23872825  rs/kio/src/sync.rs
2aadaa27f06c82d40488fae81ef8d14bf8d794fc332aaaff1467f64a902cc225  rs/kio/src/tests.rs
233c5b63f952c627a149bea584e9ba7a75cf40c468c32ec167886e4a260b9da7  rs/kio/src/time.rs
9ad9030644be754dc9dbabb85f01d0b89c07a54c11df19a51e8923382f0e4744  rs/kio/src/tokio.rs
b1af31f33ade54f7bf04c484a977e2052f7a40e9cbfbad64fcf55ed4a777d736  rs/kio/src/waiter.rs
cfc452c35be44df2b72559f13b09761bc6863ff089589f3b50d91edfa4d017a6  rs/kio/src/weak.rs
4d885df1765161350cb772fd4a5feec899e18146b3d21dd34ed1fe30cda509e0  rs/kio/tests/waiter_allocs.rs
fba5a2f70e35e872f2f31762eed6ac4790f3ae9e6805019555ca94ab5b098e96  rs/moq-net/CHANGELOG.md
7f9036704a04dfbbce51d41b3f8a75387b4072c37f9edbf7bd39dcf9b1bce25e  rs/moq-net/CLAUDE.md
4640ea87ea1eb6380bf016b28b9f68f232ed1578cfb90d4bfefd227058c75ec4  rs/moq-net/Cargo.toml
735e089265d2389a3b4eda963c128476ac7129bba8aa265dfdf3489cc0ce3718  rs/moq-net/README.md
b21baf29692c21e67543fe84de346f477f27b5dcc36076f3609ffb853b893e31  rs/moq-net/benches/group.rs
5fb40e8000ab1cd7c39ec1799f9447221e518adac3443edae1fee31ddf4c31e0  rs/moq-net/benches/track.rs
64cd4deea85e2ea30a667dddcc709c88d7ceed5ebc8de7da6b552022e013ff74  rs/moq-net/examples/fuzz-seeds.rs
4bd255574abab3c8dcc24e8c1a66fa56941a8979f59c3981f4a808527299d3a8  rs/moq-net/fuzz/.gitignore
4d195c3dd38d5161320b8a4bb620c13588a48ce785d7b8bdd8f9077b5138e519  rs/moq-net/fuzz/Cargo.toml
e7dbd3b31845b6b3d823c12029a4e316cecc9b3de41bcb5007e5a059e9d27a9b  rs/moq-net/fuzz/README.md
57465d7769ef41ccdc2bd6764c1a5e370b6adb5fb13874cf1b3d7ae1482a02fc  rs/moq-net/fuzz/fuzz_targets/ietf.rs
c1b904912d7b3b80f48164242a7082d3b1ee04acb0055610154a3669fcc4953b  rs/moq-net/fuzz/fuzz_targets/lite.rs
db8747bde3fc1fc93147b1a2a13db593df85e5672d8c04d1ce5833ac2e526957  rs/moq-net/fuzz/fuzz_targets/path.rs
8987f0c9e0fc7d721d90674f78b4b3798eb7bd1395c4dae410b91cacf1cbe363  rs/moq-net/fuzz/fuzz_targets/varint.rs
60ccb33300ddd525f866de2b2b63697763d1353ae1e66e89d23e909081976b13  rs/moq-net/fuzz/regressions/README.md
43a75d39130ade13bc55b9acbb6613b0693e15f7209f2e8b9c729294f136f6ce  rs/moq-net/fuzz/regressions/ietf/subscribe-absolute-filter
cb6b87b7535a82eeabf4636c25d1819dcf1d316b5ab3def4668b047fb08144a5  rs/moq-net/src/client.rs
7a4640d400f0f48ed10ab93d902fc4d89c46ee641f61ad795c2c6e32211fabbf  rs/moq-net/src/coding/decode.rs
35b07eef28194c7e2bd2f034519a1431d008cd36ec3b90ee439531735e7fa030  rs/moq-net/src/coding/encode.rs
636ab7d0333fea32c81847a05a280197a57ae73a9e1edbc1319923f5aefbdc86  rs/moq-net/src/coding/mod.rs
c53abf61cf6141bd3b5dafe3f46a45c9b5f6260645321c0afd980d41daf3a1f1  rs/moq-net/src/coding/reader.rs
408c287f8580ef0fab30d2ad64566957b050757e70abc540eea96ec87ee6a1c4  rs/moq-net/src/coding/size.rs
83b4419f6373ee4f2ac468f7fc8460f1f3ed393f7d651ee068b4af5f01469cc5  rs/moq-net/src/coding/stream.rs
58921f403c1be81edfed02306459127274f3775a3dbfda74e09114d1d5dc2dd5  rs/moq-net/src/coding/varint.rs
22013cd503307d41898a32961a06c115e234df3844a89bf6c3c422c5fe5da558  rs/moq-net/src/coding/version.rs
381059d12152037e151bc07515d71dd4c7ffdf1e916e90c41b0d381af7c0ec5d  rs/moq-net/src/coding/writer.rs
8bb865bba485154e44a66279f93952ad66f66d882961f0766021807e50bfdb28  rs/moq-net/src/error.rs
a7392bea1eac8fda9f4a3e5951145e3569c623a0bfee3d81dca014ab67908bfc  rs/moq-net/src/fuzz.rs
06e77fb74f7981894ebfbf7324f980a40dbba6278a42802cfff612d8ab00562b  rs/moq-net/src/ietf/adapter.rs
9acb5c4114dc31661d5b6b4deea7fc9521a574c2dfcd286922db89c86ef34c18  rs/moq-net/src/ietf/cluster.rs
f627ecf0908152cd873c41f0fc8dbdb3af0868b51e7762067735eb5e098a3e6e  rs/moq-net/src/ietf/control.rs
c25463731b21d8e2d3a71a1435c96acd9ce4b46df77d6a7b282f4d3411eeb459  rs/moq-net/src/ietf/error.rs
e34d35ca998da2723eeaf8b487fe760ca73facc29a57358e82155f1aa6bc1a03  rs/moq-net/src/ietf/fetch.rs
e0b04f4a112e5cae3747be1a5cb4b028e4342ece8a88e7c42a4435a79e7e95a7  rs/moq-net/src/ietf/filter.rs
f8810b0150ff90fd317eb59795b5a59641904abb677367e5fca4cac1a4d1629f  rs/moq-net/src/ietf/goaway.rs
d1595cf9345efb2a554853032cd1246fa3680f7960256fe27043493dd708d759  rs/moq-net/src/ietf/group.rs
2c5840b9827e1d3a2f2bdcf77e7c78c70bf8d30901e3301a670ccc219bfbabd3  rs/moq-net/src/ietf/location.rs
dd352e20997e81e73a3bff89ebe063b4f2fb902dae1868dce4b39eaace1413fc  rs/moq-net/src/ietf/message.rs
31a8438d51f302c7baa1a5234b16c75ba301cbff572e48b8f0c48856b8a73936  rs/moq-net/src/ietf/mod.rs
ed71574e6f3aa1e6192c722e0c0c7a6faab9ae08308b6a8c45a88a496687b549  rs/moq-net/src/ietf/namespace.rs
f19b0b00116bb5160e0438c4a71715df6eaaafef6ee538011c46eb4187551c12  rs/moq-net/src/ietf/parameters.rs
5a4931740a691735db43173c29f9c4f4b3fce56b5ad082075e0a7f2d2b46cc4a  rs/moq-net/src/ietf/peer.rs
0a52612baccc7da35946bb53879d1d23481d4a5d7b76841415e6ec6cd26a6344  rs/moq-net/src/ietf/priority.rs
a0724e67c0f4802344ccc91dc450990b68ed9d32a3719e47db511178f00e5b7a  rs/moq-net/src/ietf/properties.rs
19cb2586036e3047967aebeb1184e4080a56a8a665b163dfa491608cc71f317b  rs/moq-net/src/ietf/publish.rs
9c1c3e48b1f9a44a0fcb93ff12428af3eb652ca4de23a641174b721149cff7f9  rs/moq-net/src/ietf/publish_namespace.rs
ed0051d329468249aa6c13f9522d003244033bffb625d3aafcfc1e5ec45dfeda  rs/moq-net/src/ietf/publisher.rs
ad4cbbe811d79c189ed005c352b044c3fadf6fa619a4dc98a2ce9bb9547ef2fb  rs/moq-net/src/ietf/request.rs
ed06b6f5cada6a11a4f70fee4886961bdd711f205eb499a9d2fbbe31c83eb3b4  rs/moq-net/src/ietf/session.rs
24c554522c26780dc12162c1a7857e2718dab556d0aac484fc8cd8bc19207bdd  rs/moq-net/src/ietf/solicit.rs
a3517d3c391cbbd97c61c6c08b0e67849ddef827519d930a6fe34f3a5ed1d66e  rs/moq-net/src/ietf/subscribe.rs
890c9230b05d3c017004e862e73f278c12eb97623d2083ae1b28aafcf034ed96  rs/moq-net/src/ietf/subscribe_namespace.rs
ac4652d37fc90424173d94f628c30742a7e2419f49bef549260552e0147fdd78  rs/moq-net/src/ietf/subscriber.rs
78eb2053d62208327b953c2a237bb30c73d817efaa0ff8c2b9ccb698aa6ad674  rs/moq-net/src/ietf/track.rs
6b47daa4679ecdd41cbb7a5b2db45f6d7c90a3df7018de38f9fb173a23716cbb  rs/moq-net/src/ietf/version.rs
7b02c49772cd9700536411d3d0138451fc01aed5ea1be333cb686717d5fc34b4  rs/moq-net/src/lib.rs
eb4dcc86d21ec923b7d28608d1da78a0160df81e7238f5c9ff6e89abd2b17c07  rs/moq-net/src/lite/announce.rs
0173cc8cbcfd196911218a4569c193cd3a7aeed36dd5807938a8825fbb875d39  rs/moq-net/src/lite/datagram.rs
ede772d282646d773a7429d3c1ff73cac74d6f883d894d13094f48901d238be5  rs/moq-net/src/lite/fetch.rs
155ac9ad71304bc1148682de3cce23fe0088d627987e93d534b131bbf7073248  rs/moq-net/src/lite/goaway.rs
2eb31e1a4cbac703d51f221b8c0fe114bbea2445e3fa8ed3d8d843e9f7989032  rs/moq-net/src/lite/group.rs
2233f8f320e2480f6a6ead3d9796c02b121cf9952f88589235a2da640dd1336e  rs/moq-net/src/lite/info.rs
9549285ec4c82d3d55f7b861c3b1641dd0f826345f733b7d5361b107f04c6b47  rs/moq-net/src/lite/message.rs
08d72ef29ce3131c19bcc4692f5d6f73d979449efbbe24854a7c054bae74ca9a  rs/moq-net/src/lite/mod.rs
5550db07f628f3e98da41c78ce6d3100cc3df0e4bf51585853363229757df844  rs/moq-net/src/lite/parameters.rs
3b5c60a3cdc4b54f3e744acc89676b08ac97e8612e256a00bc619b27cf76c4c6  rs/moq-net/src/lite/priority.rs
c254c8d260779288b2522dae11b160c1cb003ca7891115720421e146d6063c77  rs/moq-net/src/lite/probe.rs
159f9f8e34f781dff7af0c097f0fc07cfbe064e0bc4d5028dbd05be7d566cf59  rs/moq-net/src/lite/publisher.rs
ca6fa42cbc18d46b8dd9c68015bb6e144f56b49daf724d9d53d42408a097e883  rs/moq-net/src/lite/session.rs
775101ae85928f8746e5558b17212232ff372ded43343cf6b83402f0aa05987e  rs/moq-net/src/lite/setup.rs
ee1e1e69319b933d7e6c050a015e8c297dde1e5e09f091658186454fdee48a57  rs/moq-net/src/lite/stream.rs
f20257b5769faf4dea18e752454fa4ee7eed00ec39f55f8ec2022ceb258151d3  rs/moq-net/src/lite/subscribe.rs
14a8b917a817f6bea4d7013e61e52e6ed3c784d577f444e3afad59d352b222f9  rs/moq-net/src/lite/subscriber.rs
2f24992b90c0bcb5ee78b2840d23f0803b5692b59b1dbecd040e5bf0632736aa  rs/moq-net/src/lite/test_transport.rs
c83c9dd06ee4be89884d5173b68e70179542a68e7725dd42f0423c8b26cc1c6b  rs/moq-net/src/lite/track.rs
411c11216ad9a2e6cf4cbb47014768f98a3566111d3405bcb993dd4f8a59470d  rs/moq-net/src/lite/version.rs
9a9842ba4fa7f77c6c7e1cd980a6a5ed1a07123482729847b145bd5330a2de0f  rs/moq-net/src/model/bandwidth.rs
c09916bb6f2da0ca4d31fc2f36095b3766b9932967cb377dda9fb3a256d76b38  rs/moq-net/src/model/broadcast.rs
d9efd5df64c1ab71473f782b53eca8dd91efa4e3581e0d3492d444b4410fcecf  rs/moq-net/src/model/bytes.rs
d845896c5a710836e6ed0e3960fc40d29b3ec8a5ad1fc4aa5b839b811368631e  rs/moq-net/src/model/cache.rs
d06a7b845a34218936e35389ba63ba6d30152f1a3980447579016241a856881d  rs/moq-net/src/model/datagram.rs
49e3be08a4fb6203b5a3437857a0f4cb530802bfc89490d23ff03faa4dc9bd8f  rs/moq-net/src/model/frame.rs
8dd6ef544813fc392120148e1af243e17e0a062fb31c7d55ff2efb60fc72bd76  rs/moq-net/src/model/group.rs
9000b53be8969dd1623cd6d3a79293c1f35773ec5cb3085a0d808653f7b8d194  rs/moq-net/src/model/mod.rs
0addbf0304eae87322e493bfd22e8f1a6862ca663376df393cae89ecc33a4cd8  rs/moq-net/src/model/origin.rs
163b9e0c2d1017bd450cee429af9efe4ac588fe6ef8872e59291d4d10d14cb93  rs/moq-net/src/model/requests.rs
5f6d60b3c71968a01ffc45a617829ef8dce27391de0823849cf1d72b07922a38  rs/moq-net/src/model/resume.rs
30e5e851831f48c331765919878084c662f50996d17fa18348a2ea62d4f1c4c6  rs/moq-net/src/model/subscription.rs
c3e75dd96e2c9ba23900bc6990e30ee540272dfd0e56f5ee7994a14233ac21f2  rs/moq-net/src/model/test_tracing.rs
0730eaaca1d7e0144687ba0bcf53753958f0616016e2d4b7d67e6d8b17c4c5f2  rs/moq-net/src/model/time.rs
1ce477c329359aa879d9fe49591373d0d98258ec157bf03a3e73748866131127  rs/moq-net/src/model/track.rs
92bb6234b6f30da244c1498da64fbf12b30f4c65e1cf58a53c62faafa919de8a  rs/moq-net/src/model/weak_cache.rs
43d21f22cc84fa3f6457bfbdef61c3a41ea6e941f5142f0d5c10ddd16650f6f1  rs/moq-net/src/path.rs
6e1351f9136335dbda034a5d2169d7880f30b50a2efbb965f713baf1011b5e30  rs/moq-net/src/recv.rs
cb781e146b8c4f738940b58c342bfc8334deb1215b7c267198342c97a0f2273f  rs/moq-net/src/server.rs
4001112306a34e46cdf0703f27ea6aae2e21d6169c9b99302b63d669b6ce38c0  rs/moq-net/src/session.rs
03d9844ca79adcde3d52a351fd19d5e5c7d45508a66930269c9f71c44f71e465  rs/moq-net/src/setup.rs
4a55827675b2eadb66d92b5b161573f362b27bdc041cc6d0089077dc516c9ec2  rs/moq-net/src/stats.rs
521d1fc7bb211a7e672aa9b77548a0d0823e0bd669f83916e17da6d1be8c9a4b  rs/moq-net/src/util.rs
8adf23f9273ca007c09efe530261fa63c1cc99a99501714dbec9efc76e166247  rs/moq-net/src/version.rs
d1ba1ca1af45f4545e67ecc9bcff185b602d415f9880d058905c258b82fc8bcd  rs/moq-net/tests/group_charge.rs
c1cc332572883d0fbf732d47c0a6db745791756ca489029f1974b6dde8862afc  rs/moq-net/tests/loom.rs
2463fb6a21d1c7c01f2ee1119b5f39e87340718648bb7b38dc6670f997d8fa18  rs/moq-net/tests/wasm.rs
bcc0ae56014a5f590dd01f0a3a4b582d0dbd1fa6a0be47a92c1bd8ab71664d09  rs/moq-native/CHANGELOG.md
97c096edc1e4e01812afa802d862a17810d10047e46b5b8d1c484d21d156e914  rs/moq-native/Cargo.toml
f72f03ba9df45c0fa712a57bfc0e8ec7c1b3228c02c084dde4a5d44cfee32b98  rs/moq-native/README.md
a23b98c850b3da6d354257b4cdb7a0b6a262ffc71ca12875f23df008c3eccbff  rs/moq-native/examples/chat.rs
91a9de9c1c8da7c25a2c5cd5e5f41fab3f3a383c353a00a96a3a14a6431d6dca  rs/moq-native/examples/clock.rs
c4ef88064e6c93164e944faef6e0807e6bf00b0d14998eef7423378172c20331  rs/moq-native/src/accept.rs
e4087a3eae0b606e13ab4ce7f0cb59b9e0be28b71d763d0af8a83f2d7aba056f  rs/moq-native/src/bind.rs
e414de9cd4b381cd85f6e5733841039d72aadadda5e3c7d87d890c0d6ce96c8c  rs/moq-native/src/client.rs
615213cdd96565296c2da78be8114e522848d0c554fb46ebd61d46337f33dd4d  rs/moq-native/src/connect.rs
a59cb3f2e11b70445983f1e4ceb203bc07649813fb2127339253456d74234e1c  rs/moq-native/src/crypto.rs
f5ab33946f1f7e48f4d1f6f1584b443f8cde78621b10bd265c469eceaf14dbaf  rs/moq-native/src/error.rs
cf8824df03f3da0b3cb2edcdf1a184601251351cdc125a625b7c84aba464f0bf  rs/moq-native/src/failover.rs
026925a838fabd7bb072d8a710fbd3a7f59d6ab6880b15186eb78370966f282d  rs/moq-native/src/iroh.rs
605301d9704132f1c6af0baf19f82402e739fddc71f189c22d4445920dd8fe4a  rs/moq-native/src/jemalloc.rs
6770ca6da80fa79e1a6a3c278cc16e833d2ab36aef94041630c7157232eb02d1  rs/moq-native/src/lib.rs
3c900e05369bd2cfdfe4b494762f760559e1c9612a9a826fcc64fcad20057456  rs/moq-native/src/log.rs
2a9f7d34c8df6bbffde135b03788ab1aecf33264ab018828b2e059cd5ed09914  rs/moq-native/src/noq.rs
5f2b12ac7a2aa1fcf591b1074a7503729d56c8670550d372d13df2420bc8c646  rs/moq-native/src/quic.rs
0c5ce66e8f5a529310b4fcbacb04f76b4c52414b5b0d6185c98e054c812f619c  rs/moq-native/src/quiche.rs
874bdfd46425774700197c2324c50d31f85f46b7db4d603d31fda709d3dc208f  rs/moq-native/src/quinn.rs
b9c39acac8f55ecbf6b7fbaac55ad62bc1e88098a3568ddb951ee95bee069c73  rs/moq-native/src/reconnect.rs
f04e03affa97b38e0be93057b869446f1671ac440147df0e24246794a85abe37  rs/moq-native/src/resolve.rs
9f491fa4f221bbb77ca9c89d64ab8b20b80dd3aa5b91a358ab8d150835a050ef  rs/moq-native/src/server.rs
62ccd1e377531f8261f700c763f3125d4a061ec70e0ea7687134ad5132bb741b  rs/moq-native/src/tcp.rs
696569c8bde4c2c35e3d82a9a8602baea2105c183c17c61edd12cddcbe749671  rs/moq-native/src/tls.rs
ee69b52e1e7f6f55ee2692bbc90c499ff592187bc850ea59873d64905a46952c  rs/moq-native/src/unix.rs
8ecd85f40e310a11c10fbf7667f1097a98c06b90ee9d59cba738b4837172a4e0  rs/moq-native/src/util.rs
eac448ddde57aaba9c2ca5f4ab18086635df6919f4bc5a2775de7ad7d05758ed  rs/moq-native/src/watch.rs
2117d36f2b2347983ef3ede0fca2870e27b178be522dc1d0b56fdcc3526c6695  rs/moq-native/src/websocket.rs
4ebee65da6ccb76ac714b589c5d30f57833acd3636ef4bc6ce84d1a8de421f75  rs/moq-native/tests/alpn.rs
63ecad0c2431213c717901b33ba5d274aad9dd2f0530326582a227dc877f42ea  rs/moq-native/tests/backend.rs
a657b2c0be9f2a39e8bd2c603d68c9911c5ddee92994cc415f903ddaa3dc6a12  rs/moq-native/tests/broadcast.rs
c098af71850c5eba1b994c22bb99667e7744c6a767e4fbb526a96c8caff422f5  rs/moq-native/tests/reconnect.rs
```

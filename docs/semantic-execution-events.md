# Broker execution observations

`CapnpBrokerState::subscribe_execution_activity()` observes subsequent canonical
MCP/action-host executions. A caller must carry the host-minted execution scope;
ordinary person actions, app invocations and client-supplied context cannot invent
one. Each event records the authenticated principal, execution UUID, caller's
invocation UUID, exact peer and admission-time app/room/record/action address.
The broker's UUID and emission sequence identify its lifetime and event order;
`source_time_ms` is the wall-clock observation timestamp, not presentation time.

The stream contains no action arguments or result content. Acceptance is emitted
only after delivery to the app's invocation stream. Validated progress and
terminal receipts follow their normal persistence and effect barriers. Retries,
queries, refused calls and observer refreshes create no operation. Cancellation
requested, cancellation confirmed, failure and unknown outcome are distinct;
acceptance never implies success or a visible mutation. A later task readback is
still required to establish the result.

The host-local broadcast retains at most 256 events, with each identifying field
bounded to 4096 bytes. Oversized target metadata has no presence. No subscriber
means no event allocation, serialization, task or timer. Subscribers receive
`Lagged` on overflow and must clear their old presence and report a gap; they must
not continue a take as though no event was lost. Drop unsubscribes without ending
agent sessions, invoking cancellation or shutting down the host. A new subscriber
starts with future events and receives no fabricated replay of old work. Broker
shutdown closes the stream even if a facade survives; consumers must clear on
closure as well as lag. Later subscriptions remain closed.

This is a trusted local integration seam, **not network authentication or a
capture API**. Execution UUIDs are correlation, not credentials. A publisher must
recheck current certified publisher/room admission, seal through an already-held
exact record channel and preserve the source sequence. A browser consumer must
verify the authenticated publisher and exact mounted record, reject stale epochs,
expire observer copies, and latch supported target geometry to a genuine
presentation. No publisher or visual consumer is enabled by this increment.
Missing record/geometry/presentation data remains a capture gap, never a guessed
pointer or a move of the physical seat. Synthetic 80-run tests prove only these
broker pathways and bounded observation; they are not real-model or frame-rate
evidence.

Execution lifetime is shared across host-retained caller copies. MCP HTTP session
deletion, idle-session eviction, endpoint disposal and stdio server disposal
retire that lifetime even when an in-flight request still holds the server.
The broker emits `Ended` for that run's remaining execution observations;
app operations continue under their existing receipt/cancellation rules.
Their invocation-level interactions still show actual work and outcomes in the
view, independently of the retired transport run. Late updates cannot revive
execution observations, and buffered events expose the closed lifetime.
A new logical MCP session gets a new host-minted execution identity. Consumers
must treat an ended run or a buffered event with a closed lifetime as removal,
not silently retain its earlier visible state.

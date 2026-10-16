# Exact local runtime custody

The local-model service prepares a stopped OCI container and records its full
CID, random run identity, immutable image, model identifier hash, and framed
argument hash before starting runtime code. The Linux supervisor verifies
actual container metadata against that receipt. A listening endpoint alone is
insufficient: readiness requires health, exact model discovery, and a matching
running owned container.

Stop first attempts removal of the verified exact owned container, then stops
and observes the run's exact systemd unit, and finally observes CID absence.
Manager cleanup remains mandatory, including quiescence and no pending job.
Removal failure does not prevent an attempt to stop the manager. Any failed or
unobservable gate returns failure and retains custody; a manager's successful
Stop does not by itself prove that a container disappeared. Legacy handles
without exact custody cannot certify container cleanup.

Short private state transactions compare the expected model record and merge
independent model/license changes. They hold no lock during loading, readiness,
or process operations. A concurrent Stop prevents a stale worker from publishing
Ready. If canonical admission refuses a prepared handle and cleanup fails,
an exclusive private orphan receipt supports direct unconfined recovery by
random run ID. Recovery verifies exact custody and refuses currently canonical
ownership; it takes no raw container selector, path, or command from an app.

The creation-to-CID crash window remains: a crash before podman returns its
identity can leave a stopped created object without a receipt. Failed receipt
writes cannot be described as durable tracked recovery.

The shared supervisor was exercised with two new fixed-duration sleep fixtures
inside an already available immutable OCI image. Manager-only Stop left the
target object and process present; production cleanup removed that target
while its independently owned control kept running. Final cleanup observed
both exact CIDs absent and both units quiescent. The failed first observation
is retained alongside the successful second observation. This qualifies generic
process lifecycle behavior, not inference or model quality.

//! Engine jobs are read through the one record API: this device's own by the daemon that runs
//! them, and another device's where they run, the held read forwarded whole.

use std::sync::{Arc, Mutex};

use tilo_actions::remote::{
    ActionCallError, Caller, DEVICE_JOBS, ReadingAnswer, ReadsAnswer, RemoteActionMessage,
    ResourceAddress, ResourceCopy, ResourceReadRequest, ResourceReads, ResourceWake,
    device_job_address, device_record_space,
};

use super::{DeviceJobReads, RecordReads};

const DEVICE: [u8; 32] = [7; 32];
const OTHER: [u8; 32] = [8; 32];
const PERSON: [u8; 32] = [9; 32];
const WORKSPACE: &str = "0a0a0a0a-0a0a-4a0a-8a0a-0a0a0a0a0a0a";

/// A device's own engine jobs, as its daemon answers them.
struct OwnJobs;

impl DeviceJobReads for OwnJobs {
    fn device(&self) -> [u8; 32] {
        DEVICE
    }

    fn decided_by(&self, _: &str) -> Option<ResourceAddress> {
        Some(ResourceAddress {
            workspace: WORKSPACE.to_owned(),
            app: "so.tilo.video".to_owned(),
            record: String::new(),
        })
    }

    fn read(&self, _: &Caller, address: &ResourceAddress) -> Result<ResourceCopy, ActionCallError> {
        Ok(ResourceCopy::reading(
            serde_json::json!({ "id": address.record, "state": "running" }),
            Some("own".to_owned()),
        ))
    }

    fn may_read(&self, _: &Caller, _: &ResourceAddress) -> Result<(), ActionCallError> {
        Ok(())
    }

    fn list(
        &self,
        _: &Caller,
        workspace: &str,
        _: &str,
    ) -> Result<Vec<ResourceAddress>, ActionCallError> {
        Ok(vec![ResourceAddress {
            workspace: workspace.to_owned(),
            app: DEVICE_JOBS.to_owned(),
            record: "own-job".to_owned(),
        }])
    }

    fn watch(
        &self,
        _: &Caller,
        _: &ResourceAddress,
        _: &ResourceWake,
    ) -> Result<(), ActionCallError> {
        Ok(())
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> ReadingAnswer {
        Box::pin(tilo_actions::remote::answer_read(
            self,
            caller.clone(),
            correlation,
            read,
        ))
    }
}

impl ResourceReads for OwnJobs {
    fn read<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
    ) -> ReadsAnswer<'a, ResourceCopy> {
        Box::pin(std::future::ready(DeviceJobReads::read(
            self, caller, address,
        )))
    }

    fn may_read<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
    ) -> ReadsAnswer<'a, ()> {
        Box::pin(std::future::ready(DeviceJobReads::may_read(
            self, caller, address,
        )))
    }

    fn list<'a>(
        &'a self,
        caller: &'a Caller,
        workspace: &'a str,
        app: &'a str,
    ) -> ReadsAnswer<'a, Vec<ResourceAddress>> {
        Box::pin(std::future::ready(DeviceJobReads::list(
            self, caller, workspace, app,
        )))
    }

    fn watch<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
        wake: &'a ResourceWake,
    ) -> ReadsAnswer<'a, ()> {
        Box::pin(std::future::ready(DeviceJobReads::watch(
            self, caller, address, wake,
        )))
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> ReadingAnswer {
        DeviceJobReads::answer_read(self, caller, correlation, read)
    }
}

/// Another device's jobs, as its link answers them: what it was asked, and what it says.
#[derive(Default)]
struct OtherJobs {
    asked: Mutex<Vec<ResourceReadRequest>>,
}

impl ResourceReads for OtherJobs {
    fn read<'a>(
        &'a self,
        _: &'a Caller,
        address: &'a ResourceAddress,
    ) -> ReadsAnswer<'a, ResourceCopy> {
        Box::pin(std::future::ready(Ok(ResourceCopy::reading(
            serde_json::json!({ "id": address.record, "state": "running", "device": "other" }),
            Some("other".to_owned()),
        ))))
    }

    fn answer_read(
        self: Arc<Self>,
        _: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> ReadingAnswer {
        self.asked.lock().expect("the asks").push(read);
        Box::pin(std::future::ready(RemoteActionMessage::ResourceReading {
            correlation,
            result: Ok(serde_json::json!({
                "revision": "other",
                "records": [ { "record": "j2", "value": { "id": "j2", "state": "running" } } ]
            })
            .to_string()),
        }))
    }
}

fn reader() -> (Arc<RecordReads>, Arc<OtherJobs>) {
    let other = Arc::new(OtherJobs::default());
    let forwarded = Arc::clone(&other);
    let mut reads = RecordReads::of_device(None, None);
    reads.jobs = Some(Arc::new(OwnJobs));
    reads.other_devices = Some(Arc::new(move |space: &str| {
        (space == device_record_space(&OTHER))
            .then(|| Arc::clone(&forwarded) as Arc<dyn ResourceReads>)
    }));
    (Arc::new(reads), other)
}

#[tokio::test]
async fn a_jobs_read_goes_to_the_device_that_runs_it() {
    let (reads, _other) = reader();
    let caller = Caller::authenticated(PERSON, DEVICE);
    // This device's own jobs are answered here, by the daemon that runs them.
    let own = device_job_address(&DEVICE, "own-job");
    assert_eq!(
        reads
            .read(&caller, &own)
            .await
            .expect("this device reads its job"),
        ResourceCopy::reading(
            serde_json::json!({ "id": "own-job", "state": "running" }),
            Some("own".to_owned())
        )
    );
    // Another device's are read there.
    let elsewhere = device_job_address(&OTHER, "j2");
    let copy = reads
        .read(&caller, &elsewhere)
        .await
        .expect("the other device answers");
    assert_eq!(
        copy.readable().expect("a job reads as JSON")["device"],
        "other"
    );
    // A device space no device answers is refused, never read here.
    let unknown = device_job_address(&[1; 32], "j3");
    assert!(reads.read(&caller, &unknown).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_read_of_another_devices_job_is_forwarded_whole() {
    let (reads, other) = reader();
    let caller = Caller::authenticated(PERSON, DEVICE);
    let request = ResourceReadRequest {
        workspace: device_record_space(&OTHER),
        app: DEVICE_JOBS.to_owned(),
        record: "j2".to_owned(),
        joined: false,
        after: Some("before".to_owned()),
        wait_ms: Some(5_000),
        answer: tilo_actions::remote::ReadAnswer::Reading,
    };
    let answer = Arc::clone(&reads)
        .answer_read(&caller, 3, request.clone())
        .await;
    assert!(
        matches!(
            answer,
            RemoteActionMessage::ResourceReading { correlation: 3, .. }
        ),
        "the answer comes back with the read's correlation"
    );
    assert_eq!(
        *other.asked.lock().expect("the asks"),
        vec![request],
        "the wait is forwarded whole, not answered here"
    );
}

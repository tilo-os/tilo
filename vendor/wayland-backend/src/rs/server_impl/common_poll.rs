use std::{
    os::unix::io::{AsRawFd, BorrowedFd, OwnedFd},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use super::{
    handle::{ErasedState, State},
    ClientId, Data, GlobalHandler, GlobalId, Handle, InnerClientId, InnerGlobalId, InnerHandle,
    InnerObjectId, ObjectId,
};
use crate::{
    core_interfaces::{WL_DISPLAY_INTERFACE, WL_REGISTRY_INTERFACE},
    protocol::{same_interface, Argument, Message},
    rs::map::Object,
    types::server::InitError,
};

#[cfg(any(target_os = "linux", target_os = "android", target_os = "redox"))]
use rustix::event::{epoll, Timespec};

#[cfg(any(
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "macos"
))]
use rustix::event::kqueue::*;
use smallvec::SmallVec;

#[derive(Debug)]
pub struct InnerBackend<D: 'static> {
    state: Arc<Mutex<State<D>>>,
}

impl<D> InnerBackend<D> {
    pub fn new() -> Result<Self, InitError> {
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "redox"))]
        let poll_fd = epoll::create(epoll::CreateFlags::CLOEXEC)
            .map_err(Into::into)
            .map_err(InitError::Io)?;

        #[cfg(any(
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "macos"
        ))]
        let poll_fd = kqueue().map_err(Into::into).map_err(InitError::Io)?;

        Ok(Self {
            state: Arc::new(Mutex::new(State::new(poll_fd))),
        })
    }

    pub fn flush(&self, client: Option<ClientId>) -> std::io::Result<()> {
        self.state.lock().unwrap().flush(client)
    }

    pub fn handle(&self) -> Handle {
        Handle {
            handle: InnerHandle {
                state: self.state.clone() as Arc<_>,
            },
        }
    }

    pub fn poll_fd(&self) -> BorrowedFd<'_> {
        let raw_fd = self.state.lock().unwrap().poll_fd.as_raw_fd();
        // This allows the lifetime of the BorrowedFd to be tied to &self rather than the lock guard,
        // which is the real safety concern
        unsafe { BorrowedFd::borrow_raw(raw_fd) }
    }

    pub fn dispatch_client(
        &self,
        data: &mut D,
        client_id: InnerClientId,
    ) -> std::io::Result<usize> {
        let ret = self.dispatch_events_for(data, client_id, usize::MAX);
        let cleanup = self.state.lock().unwrap().cleanup();
        cleanup(&self.handle(), data);
        ret
    }

    pub fn dispatch_client_bounded(
        &self,
        data: &mut D,
        client_id: InnerClientId,
        request_limit: usize,
    ) -> std::io::Result<usize> {
        let ret = self.dispatch_events_for(data, client_id, request_limit);
        let cleanup = self.state.lock().unwrap().cleanup();
        cleanup(&self.handle(), data);
        ret
    }

    /// Consume one kernel readiness batch and dispatch a bounded amount of
    /// work for every client in that batch.
    ///
    /// This is the event-loop counterpart to [`Self::dispatch_client_bounded`].
    /// Calling the latter directly reads a client socket, but intentionally
    /// does not consume the backend's aggregate epoll/kqueue readiness.  An
    /// event loop that monitors [`Self::poll_fd`] must use this method after
    /// that fd becomes readable, otherwise the aggregate readiness fd remains
    /// readable forever.
    ///
    /// The kernel readiness set is snapshotted once per call. This keeps the
    /// number of clients bounded while every client that was ready at the
    /// snapshot gets at most `request_limit` requests. If decoding the first
    /// socket read leaves more messages in a client's userspace buffer, its
    /// next fair turn is retained internally. Query
    /// [`Self::has_pending_client_dispatch`] before parking an external event
    /// loop so that this decoded work cannot be stranded after the aggregate
    /// epoll fd is drained.
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "redox"))]
    pub fn dispatch_ready_clients_bounded(
        &self,
        data: &mut D,
        request_limit: usize,
    ) -> std::io::Result<usize> {
        use std::os::unix::io::AsFd;

        let poll_fd = self.poll_fd();
        let mut events = Vec::<epoll::Event>::with_capacity(32);
        let buffer = rustix::buffer::spare_capacity(&mut events);
        epoll::wait(poll_fd.as_fd(), buffer, Some(&Timespec::default()))?;

        let ready_clients = {
            let mut state = self.state.lock().unwrap();
            for event in events.drain(..) {
                state.queue_pending_dispatch(InnerClientId::from_u64(event.data.u64()));
            }
            state.take_pending_dispatches()
        };

        let mut dispatched = 0;
        for id in ready_clients {
            // Preserve dispatch_all_clients' behavior for a client which
            // disappears after epoll reported it ready.  Its teardown is
            // collected below, while another ready client still gets a turn.
            let started = Instant::now();
            let credentials = self
                .state
                .lock()
                .unwrap()
                .get_client_credentials(id.clone())
                .ok();
            if let Ok(count) = self.dispatch_events_for(data, id.clone(), request_limit) {
                let elapsed = started.elapsed();
                if std::env::var_os("TILO_LOOP_TIMING").is_some()
                    && elapsed >= Duration::from_millis(4)
                {
                    eprintln!(
                        "SLOW-WAYLAND-CLIENT id={id:?} pid={} requests={count} elapsed_us={}",
                        credentials.map_or(0, |credentials| credentials.pid),
                        elapsed.as_micros()
                    );
                }
                dispatched += count;
                // The Wayland wire reader can fill its decoded buffer with a
                // whole socket payload in one call. Reaching the request
                // limit therefore does not imply the kernel fd remains
                // readable; retain one later, fair dispatch turn.
                if count == request_limit {
                    self.state.lock().unwrap().queue_pending_dispatch(id);
                }
            }
        }
        // Disconnected clients' resources are destroyed here, outside every
        // per-client timing above; a slow teardown is reported on its own.
        let started = Instant::now();
        let cleanup = self.state.lock().unwrap().cleanup();
        cleanup(&self.handle(), data);
        let elapsed = started.elapsed();
        if std::env::var_os("TILO_LOOP_TIMING").is_some() && elapsed >= Duration::from_millis(4) {
            eprintln!("SLOW-WAYLAND-CLEANUP elapsed_us={}", elapsed.as_micros());
        }
        Ok(dispatched)
    }

    /// See the Linux implementation above.  kqueue readiness is consumed in
    /// exactly one non-blocking batch, with the same bounded per-client work.
    #[cfg(any(
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "macos"
    ))]
    pub fn dispatch_ready_clients_bounded(
        &self,
        data: &mut D,
        request_limit: usize,
    ) -> std::io::Result<usize> {
        use std::time::Duration;

        let poll_fd = self.poll_fd();
        let mut events = Vec::<Event>::with_capacity(32);
        let buffer = rustix::buffer::spare_capacity(&mut events);
        unsafe { kevent(&poll_fd, &[], buffer, Some(Duration::ZERO))? };

        let ready_clients = {
            let mut state = self.state.lock().unwrap();
            for event in events.drain(..) {
                state.queue_pending_dispatch(InnerClientId::from_u64(event.udata() as u64));
            }
            state.take_pending_dispatches()
        };

        let mut dispatched = 0;
        for id in ready_clients {
            if let Ok(count) = self.dispatch_events_for(data, id.clone(), request_limit) {
                dispatched += count;
                if count == request_limit {
                    self.state.lock().unwrap().queue_pending_dispatch(id);
                }
            }
        }
        let cleanup = self.state.lock().unwrap().cleanup();
        cleanup(&self.handle(), data);
        Ok(dispatched)
    }

    /// Returns whether a bounded ready dispatch left decoded client requests
    /// waiting for another fair turn.
    pub fn has_pending_client_dispatch(&self) -> bool {
        self.state.lock().unwrap().has_pending_dispatches()
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "redox"))]
    pub fn dispatch_all_clients(&self, data: &mut D) -> std::io::Result<usize> {
        use std::os::unix::io::AsFd;

        let poll_fd = self.poll_fd();
        let mut dispatched = 0;
        // A caller may return to the traditional unbounded API after using a
        // bounded ready turn. Those decoded requests are no longer reflected
        // in epoll, but `dispatch_all_clients` still promises to drain all
        // pending work.
        let pending_clients = self.state.lock().unwrap().take_pending_dispatches();
        let had_pending_clients = !pending_clients.is_empty();
        for client in pending_clients {
            if let Ok(count) = self.dispatch_events_for(data, client, usize::MAX) {
                dispatched += count;
            }
        }
        if had_pending_clients {
            let cleanup = self.state.lock().unwrap().cleanup();
            cleanup(&self.handle(), data);
        }
        let mut events = Vec::<epoll::Event>::with_capacity(32);
        loop {
            let buffer = rustix::buffer::spare_capacity(&mut events);
            epoll::wait(poll_fd.as_fd(), buffer, Some(&Timespec::default()))?;

            if events.is_empty() {
                break;
            }

            for event in events.drain(..) {
                let id = InnerClientId::from_u64(event.data.u64());
                // remove the cb while we call it, to gracefully handle reentrancy
                if let Ok(count) = self.dispatch_events_for(data, id, usize::MAX) {
                    dispatched += count;
                }
            }
            let cleanup = self.state.lock().unwrap().cleanup();
            cleanup(&self.handle(), data);
        }

        Ok(dispatched)
    }

    #[cfg(any(
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "macos"
    ))]
    pub fn dispatch_all_clients(&self, data: &mut D) -> std::io::Result<usize> {
        use std::time::Duration;

        let poll_fd = self.poll_fd();
        let mut dispatched = 0;
        let pending_clients = self.state.lock().unwrap().take_pending_dispatches();
        let had_pending_clients = !pending_clients.is_empty();
        for client in pending_clients {
            if let Ok(count) = self.dispatch_events_for(data, client, usize::MAX) {
                dispatched += count;
            }
        }
        if had_pending_clients {
            let cleanup = self.state.lock().unwrap().cleanup();
            cleanup(&self.handle(), data);
        }
        let mut events = Vec::<Event>::with_capacity(32);
        loop {
            let buffer = rustix::buffer::spare_capacity(&mut events);
            let nevents = unsafe { kevent(&poll_fd, &[], buffer, Some(Duration::ZERO))? };

            if nevents == 0 {
                break;
            }

            for event in events.drain(..) {
                let id = InnerClientId::from_u64(event.udata() as u64);
                // remove the cb while we call it, to gracefully handle reentrancy
                if let Ok(count) = self.dispatch_events_for(data, id, usize::MAX) {
                    dispatched += count;
                }
            }
            let cleanup = self.state.lock().unwrap().cleanup();
            cleanup(&self.handle(), data);
        }

        Ok(dispatched)
    }

    pub(crate) fn dispatch_events_for(
        &self,
        data: &mut D,
        client_id: InnerClientId,
        request_limit: usize,
    ) -> std::io::Result<usize> {
        let mut dispatched = 0;
        if request_limit == 0 {
            return Ok(0);
        }
        let handle = self.handle();
        let mut state = self.state.lock().unwrap();
        loop {
            if dispatched >= request_limit {
                break;
            }
            let action = {
                let state = &mut *state;
                if let Ok(client) = state.clients.get_client_mut(client_id.clone()) {
                    let (message, object) = match client.next_request() {
                        Ok(v) => v,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            if dispatched > 0 {
                                break;
                            } else {
                                return Err(e);
                            }
                        }
                        Err(e) => {
                            #[cfg(any(
                                target_os = "linux",
                                target_os = "android",
                                target_os = "redox"
                            ))]
                            {
                                epoll::delete(&state.poll_fd, client)?;
                            }

                            #[cfg(any(
                                target_os = "dragonfly",
                                target_os = "freebsd",
                                target_os = "netbsd",
                                target_os = "openbsd",
                                target_os = "macos"
                            ))]
                            {
                                use rustix::event::kqueue::*;
                                use std::os::unix::io::{AsFd, AsRawFd};

                                let evt = Event::new(
                                    EventFilter::Read(client.as_fd().as_raw_fd()),
                                    EventFlags::DELETE,
                                    client_id.as_u64() as *mut _,
                                );

                                let events: &mut [Event] = &mut [];
                                unsafe {
                                    kevent(&state.poll_fd, &[evt], events, None).map(|_| ())?;
                                }
                            }
                            return Err(e);
                        }
                    };
                    dispatched += 1;
                    if same_interface(object.interface, &WL_DISPLAY_INTERFACE) {
                        client.handle_display_request(message, &mut state.registry);
                        continue;
                    } else if same_interface(object.interface, &WL_REGISTRY_INTERFACE) {
                        if let Some((client, global, object, handler)) =
                            client.handle_registry_request(message, &mut state.registry)
                        {
                            DispatchAction::Bind {
                                client,
                                global,
                                object,
                                handler,
                            }
                        } else {
                            continue;
                        }
                    } else {
                        let object_id = InnerObjectId {
                            id: message.sender_id,
                            serial: object.data.serial,
                            interface: object.interface,
                            client_id: client.id.clone(),
                        };
                        let opcode = message.opcode;
                        let (arguments, is_destructor, created_id) =
                            match client.process_request(&object, message) {
                                Some(args) => args,
                                None => continue,
                            };
                        // Return the whole set to invoke the callback while handle is not borrower via client
                        DispatchAction::Request {
                            object,
                            object_id,
                            opcode,
                            arguments,
                            is_destructor,
                            created_id,
                        }
                    }
                } else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "Invalid client ID",
                    ));
                }
            };
            match action {
                DispatchAction::Request {
                    object,
                    object_id,
                    opcode,
                    arguments,
                    is_destructor,
                    created_id,
                } => {
                    // temporarily unlock the state Mutex while this request is dispatched
                    std::mem::drop(state);
                    let request_started = Instant::now();
                    let ret = object.data.user_data.clone().request(
                        &handle.clone(),
                        data,
                        ClientId {
                            id: client_id.clone(),
                        },
                        Message {
                            sender_id: ObjectId {
                                id: object_id.clone(),
                            },
                            opcode,
                            args: arguments,
                        },
                    );
                    let request_elapsed = request_started.elapsed();
                    if std::env::var_os("TILO_LOOP_TIMING").is_some()
                        && request_elapsed >= Duration::from_millis(4)
                    {
                        let request_name = object
                            .interface
                            .requests
                            .get(opcode as usize)
                            .map_or("<unknown>", |request| request.name);
                        eprintln!(
                            "SLOW-WAYLAND-REQUEST client={client_id:?} interface={} request={request_name} opcode={opcode} object={} elapsed_us={}",
                            object.interface.name,
                            object_id.id,
                            request_elapsed.as_micros()
                        );
                    }
                    if is_destructor {
                        object.data.user_data.clone().destroyed(
                            &handle.clone(),
                            data,
                            ClientId {
                                id: client_id.clone(),
                            },
                            ObjectId {
                                id: object_id.clone(),
                            },
                        );
                    }
                    // acquire the lock again and continue
                    state = self.state.lock().unwrap();
                    if is_destructor {
                        if let Ok(client) = state.clients.get_client_mut(client_id.clone()) {
                            client.send_delete_id(object_id);
                        }
                    }
                    match (created_id, ret) {
                        (Some(child_id), Some(child_data)) => {
                            if let Ok(client) = state.clients.get_client_mut(client_id.clone()) {
                                // A request callback may provoke a protocol error or otherwise
                                // disconnect the client through the public backend handle. In
                                // that case the newly-created child can be removed before the
                                // callback returns here. The connection is already dead; do not
                                // turn that normal client race into a compositor-wide panic.
                                let _ = client
                                    .map
                                    .with(child_id.id, |obj| obj.data.user_data = child_data);
                            }
                        }
                        (None, None) => {}
                        (Some(child_id), None) => {
                            // Allow the callback to not return any data if the client is already dead (typically
                            // if the callback provoked a protocol error)
                            if let Ok(client) = state.clients.get_client(client_id.clone()) {
                                if !client.killed {
                                    panic!(
                                        "Callback creating object {child_id} did not provide any object data."
                                    );
                                }
                            }
                        }
                        (None, Some(_)) => {
                            panic!("An object data was returned from a callback not creating any object");
                        }
                    }
                    // dropping the object calls destructors from which users could call into wayland-backend again.
                    // so lets release and relock the state again, to avoid a deadlock
                    std::mem::drop(state);
                    std::mem::drop(object);
                    state = self.state.lock().unwrap();
                }
                DispatchAction::Bind {
                    object,
                    client,
                    global,
                    handler,
                } => {
                    // temporarily unlock the state Mutex while this request is dispatched
                    std::mem::drop(state);
                    let child_data = handler.bind(
                        &handle.clone(),
                        data,
                        ClientId { id: client.clone() },
                        GlobalId { id: global },
                        ObjectId { id: object.clone() },
                    );
                    // acquire the lock again and continue
                    state = self.state.lock().unwrap();
                    if let Ok(client) = state.clients.get_client_mut(client.clone()) {
                        client
                            .map
                            .with(object.id, |obj| obj.data.user_data = child_data)
                            .unwrap();
                    }
                }
            }
        }
        Ok(dispatched)
    }
}

enum DispatchAction<D: 'static> {
    Request {
        object: Object<Data<D>>,
        object_id: InnerObjectId,
        opcode: u16,
        arguments: SmallVec<[Argument<ObjectId, OwnedFd>; 4]>,
        is_destructor: bool,
        created_id: Option<InnerObjectId>,
    },
    Bind {
        object: InnerObjectId,
        client: InnerClientId,
        global: InnerGlobalId,
        handler: Arc<dyn GlobalHandler<D>>,
    },
}

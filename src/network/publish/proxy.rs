use super::*;

pub(crate) use connection::{ConnectionActivity, ConnectionSet};

pub(crate) fn run_accept_loop(
    listener: TcpListener,
    shutdown: Arc<AtomicBool>,
    runtime_pid: u32,
    workspace_port: u16,
    connections: ConnectionBudget,
    live: Arc<ConnectionSet>,
) {
    while !shutdown.load(Ordering::SeqCst) {
        // Wait for a connection instead of polling. Sleeping on `WouldBlock`
        // added the poll interval as a latency floor to the first connection
        // after an idle period, and burned a wakeup per interval per port.
        if !wait_for_accept(&listener, shutdown.as_ref()) {
            return;
        }
        // The wait above bounds how often this runs, so an idle connection is
        // reaped within one poll interval of going quiet.
        live.shutdown_idle(CONNECTION_IDLE_TIMEOUT);
        match listener.accept() {
            Ok((stream, _)) => {
                let Some(permit) = connections.try_acquire() else {
                    tracing::debug!(
                        "published port connection limit reached for workspace pid {} port {}; rejecting connection",
                        runtime_pid,
                        workspace_port
                    );
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                };
                let connection_shutdown = Arc::clone(&shutdown);
                let connection_live = Arc::clone(&live);
                if let Err(err) = thread::Builder::new()
                    .name("enclave-port-conn".to_string())
                    .spawn(move || {
                        let _permit = permit;
                        handle_connection(
                            stream,
                            runtime_pid,
                            workspace_port,
                            connection_shutdown,
                            connection_live,
                        );
                    })
                {
                    tracing::warn!("failed to spawn published-port connection worker: {err}");
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                // A spurious wakeup between poll and accept is harmless.
                continue;
            }
            Err(err) => {
                if !shutdown.load(Ordering::SeqCst) {
                    tracing::warn!(
                        "published port accept failed for workspace pid {} port {}: {}",
                        runtime_pid,
                        workspace_port,
                        err
                    );
                }
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
        }
    }
}

/// Block until the listener has a pending connection or the timeout expires.
/// Returns `false` when the publisher has been asked to shut down.
pub(crate) fn wait_for_accept(listener: &TcpListener, shutdown: &AtomicBool) -> bool {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return false;
        }
        let mut descriptor = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result =
            unsafe { libc::poll(&mut descriptor, 1, ACCEPT_POLL_INTERVAL.as_millis() as i32) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            tracing::warn!("published port accept poll failed: {error}");
            return false;
        }
        if result > 0 {
            return true;
        }
    }
}

pub(crate) fn handle_connection(
    mut client_stream: TcpStream,
    runtime_pid: u32,
    workspace_port: u16,
    shutdown: Arc<AtomicBool>,
    live: Arc<ConnectionSet>,
) {
    // Reads block until data arrives, so an idle connection costs nothing. A
    // write keeps a timeout so a peer that stops reading cannot hold a copy
    // thread indefinitely; the connection is dropped when it expires.
    let _ = client_stream.set_write_timeout(Some(CONNECTION_WRITE_TIMEOUT));
    let mut workspace_stream = match connect_to_workspace_service(runtime_pid, workspace_port) {
        Ok(stream) => stream,
        Err(err) => {
            tracing::debug!(
                "published port connect failed for workspace pid {} port {}: {}",
                runtime_pid,
                workspace_port,
                err
            );
            return;
        }
    };
    let _ = workspace_stream.set_write_timeout(Some(CONNECTION_WRITE_TIMEOUT));

    let mut client_reader = match client_stream.try_clone() {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!("failed to clone published-port client stream: {err}");
            return;
        }
    };
    let mut workspace_writer = match workspace_stream.try_clone() {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!("failed to clone published-port workspace stream: {err}");
            return;
        }
    };

    let activity = Arc::new(ConnectionActivity::new());
    let registration = live.register(Arc::clone(&activity), &client_stream, &workspace_stream);
    let upstream_shutdown = Arc::clone(&shutdown);
    let upstream_activity = Arc::clone(&activity);
    let upstream = thread::spawn(move || {
        let _ = copy_until_shutdown(
            &mut client_reader,
            &mut workspace_writer,
            &upstream_shutdown,
            &upstream_activity,
            CONNECTION_IDLE_TIMEOUT,
        );
        let _ = workspace_writer.shutdown(Shutdown::Write);
    });

    let _ = copy_until_shutdown(
        &mut workspace_stream,
        &mut client_stream,
        &shutdown,
        &activity,
        CONNECTION_IDLE_TIMEOUT,
    );
    let _ = client_stream.shutdown(Shutdown::Write);
    let _ = upstream.join();
    if let Some(id) = registration {
        live.release(id);
    }
}

/// Copy until the peer closes, the publisher shuts down, or the connection has
/// been quiet for `idle_timeout`.
pub(crate) fn copy_until_shutdown(
    reader: &mut impl Read,
    writer: &mut impl Write,
    shutdown: &AtomicBool,
    activity: &ConnectionActivity,
    idle_timeout: Duration,
) -> io::Result<()> {
    let mut buffer = [0u8; 16 * 1024];
    while !shutdown.load(Ordering::SeqCst) {
        if activity.idle_for() >= idle_timeout {
            return Ok(());
        }
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => {
                activity.touch();
                writer.write_all(&buffer[..read])?;
            }
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

pub(crate) fn connect_to_workspace_service(
    runtime_pid: u32,
    workspace_port: u16,
) -> io::Result<TcpStream> {
    if runtime_pid == std::process::id() {
        return TcpStream::connect(("127.0.0.1", workspace_port));
    }

    let netns = File::open(format!("/proc/{runtime_pid}/ns/net"))?;
    setns(&netns, CloneFlags::CLONE_NEWNET).map_err(nix_to_io_error)?;
    TcpStream::connect(("127.0.0.1", workspace_port))
}

pub(crate) fn nix_to_io_error(err: nix::errno::Errno) -> io::Error {
    io::Error::from_raw_os_error(err as i32)
}

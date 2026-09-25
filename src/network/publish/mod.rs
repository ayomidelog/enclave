use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nix::sched::{setns, CloneFlags};

use crate::workspace::{validate_published_ports, PublishedPortSpec, PublishedPortStatus};

mod connection;
mod limiter;
mod proxy;
mod publication;

use limiter::{ConnectionBudget, ConnectionLimiter};
use proxy::{run_accept_loop, ConnectionSet};
use publication::{shutdown_publications, ActivePublication, PublishMode, WorkspacePublishKey};

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How long a write to one side of a proxied connection may make no progress.
///
/// Reads block without a timeout so that an idle connection costs no CPU, which
/// leaves the write as the only direction that can stall indefinitely. A peer
/// that stops reading therefore ends its connection when this expires instead of
/// holding a copy thread and a connection slot forever.
const CONNECTION_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a proxied connection may move no bytes before it is closed.
///
/// Published ports are ordinary TCP services, so a connection that goes quiet
/// is either an idle keep-alive or an abandoned socket. Ten minutes is long
/// enough to leave a genuinely idle client alone and short enough that
/// abandoned sockets cannot hold the connection budgets forever.
const CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// Per published port. Kept as the tighter bound so one noisy port cannot
/// dominate the daemon.
const MAX_CONNECTIONS_PER_PUBLISHER: usize = 128;
/// Daemon-wide across every published port, so the total number of proxied
/// connections is bounded no matter how many ports are published.
const MAX_PUBLISHED_CONNECTIONS: usize = 1024;

mod publisher;

use publisher::publish_bind_error;
pub use publisher::{PortPublisher, PublishedPortOwner};

#[cfg(test)]
use proxy::{copy_until_shutdown, ConnectionActivity};

#[cfg(test)]
#[path = "../../../tests/src/network/publish.rs"]
mod tests;

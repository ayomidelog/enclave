//! What a daemon killed in the middle of a lifecycle operation leaves behind.
//!
//! Every other test in this directory drives the lifecycle in-process, which is why none
//! of them can test a crash: there is no second process to kill. These run the real
//! daemon in its own state directory and kill it with SIGKILL, which is the one
//! interruption the daemon cannot catch and answer for itself. What is asserted is not
//! that the interrupted operation succeeded, because it cannot: it is that the state the
//! next daemon start inherits is one it can recover from without an operator.
//!
//! The kill point is chosen from the operation's own journal rather than from a delay.
//! A journal record names the phase it is in before that phase does its work, so waiting
//! for a phase and then killing lands inside the phase every time, on a fast host and a
//! slow one alike. A delay would land wherever the host happened to be, which is the same
//! thing as not choosing.
//!
//! What recovery owes is a settled state that agrees with the host. The workspace is
//! either running with a live runtime or stopped with nothing of its own left on the
//! host, and either way nothing is left running that no record describes.

mod harness;
mod sandbox;
mod workspace;

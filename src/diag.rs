//! Diagnostics that never block the caller. Lines go through a bounded queue to a
//! dedicated writer thread and are dropped when it is full, so an unread stderr pipe
//! cannot stall hardware restoration or the shutdown path.

use std::io::{self, Write};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::OnceLock;
use std::time::Duration;

enum Message {
    Line(String),
    Flush(SyncSender<()>),
}

static SINK: OnceLock<SyncSender<Message>> = OnceLock::new();

pub fn init() {
    SINK.get_or_init(|| {
        let (tx, rx) = sync_channel::<Message>(1024);
        std::thread::spawn(move || {
            for message in rx {
                match message {
                    Message::Line(line) => {
                        let _ = writeln!(io::stderr().lock(), "{line}");
                    }
                    Message::Flush(ack) => {
                        let _ = ack.send(());
                    }
                }
            }
        });
        tx
    });
}

pub fn log(line: &str) {
    match SINK.get() {
        Some(sink) => drop(sink.try_send(Message::Line(line.to_owned()))),
        None => {
            let _ = writeln!(io::stderr().lock(), "{line}");
        }
    }
}

/// Wait (bounded) until everything queued so far has been handed to stderr.
pub fn flush(timeout: Duration) {
    if let Some(sink) = SINK.get() {
        let (ack, done) = sync_channel(1);
        if sink.try_send(Message::Flush(ack)).is_ok() {
            let _ = done.recv_timeout(timeout);
        }
    }
}

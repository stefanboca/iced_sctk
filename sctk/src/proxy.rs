use std::{cell::RefCell, pin::Pin, rc::Rc};

use sctk::reexports::calloop::{
    EventSource, PostAction,
    ping::{Ping, PingError, PingSource, make_ping},
};

use crate::{
    futures::futures::{
        Sink,
        channel::mpsc,
        task::{Context, Poll},
    },
    runtime::Action,
};

const MAX_SIZE: usize = 100;

#[derive(Debug)]
pub struct ProxySink<T: 'static> {
    sender: mpsc::Sender<Action<T>>,
    ping: Ping,
}

impl<T: 'static> Clone for ProxySink<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            ping: self.ping.clone(),
        }
    }
}

#[derive(Debug)]
pub struct ProxySourceInner<T: 'static> {
    receiver: RefCell<mpsc::Receiver<Action<T>>>,
    source: RefCell<PingSource>,
    ping: Ping,
}

impl<T> Drop for ProxySourceInner<T> {
    fn drop(&mut self) {
        // Ping on drop, to notify about channel closure. We wrap ProxySourceInner in a Rc so this
        // is only called when there are no senders remaiining.
        self.ping.ping();
    }
}

#[derive(Debug)]
pub struct ProxySource<T: 'static>(Rc<ProxySourceInner<T>>);

pub fn new<T: 'static>() -> (ProxySink<T>, ProxySource<T>) {
    let (sender, receiver) = mpsc::channel(MAX_SIZE);
    let (ping, source) = make_ping().unwrap();
    (
        ProxySink {
            sender,
            ping: ping.clone(),
        },
        ProxySource(Rc::new(ProxySourceInner {
            receiver: RefCell::new(receiver),
            source: RefCell::new(source),
            ping,
        })),
    )
}

impl<T: std::fmt::Debug + 'static> Sink<Action<T>> for ProxySink<T> {
    type Error = mpsc::SendError;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.sender.poll_ready(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, action: Action<T>) -> Result<(), Self::Error> {
        self.sender.start_send(action).map(|()| {
            self.ping.ping();
        })
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.sender.poll_ready(cx) {
            Poll::Ready(Err(ref e)) if e.is_disconnected() => {
                // If the receiver disconnected, we consider the sink to be flushed.
                Poll::Ready(Ok(()))
            }
            x => x,
        }
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.sender.disconnect();
        Poll::Ready(Ok(()))
    }
}

impl<T: 'static> EventSource for ProxySource<T> {
    type Event = Action<T>;
    type Metadata = ();
    type Ret = ();
    type Error = ProxyError;

    fn process_events<F>(
        &mut self,
        readiness: sctk::reexports::calloop::Readiness,
        token: sctk::reexports::calloop::Token,
        mut callback: F,
    ) -> Result<sctk::reexports::calloop::PostAction, Self::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        let mut clear_readiness = false;
        let mut disconnected = false;

        let mut source = self.0.source.borrow_mut();
        let mut receiver = self.0.receiver.borrow_mut();

        let action = source
            .process_events(readiness, token, |(), &mut ()| {
                for _ in 0..MAX_SIZE {
                    match receiver.try_next() {
                        Ok(Some(val)) => callback(val, &mut ()),
                        Err(_) => {
                            clear_readiness = true;
                            break;
                        }
                        Ok(None) => {
                            callback(Action::Exit, &mut ());
                            disconnected = true;
                            break;
                        }
                    }
                }
            })
            .map_err(ProxyError)?;

        if disconnected {
            Ok(PostAction::Remove)
        } else if clear_readiness {
            Ok(action)
        } else {
            // Re-notify the ping source so we can try again.
            self.0.ping.ping();
            Ok(PostAction::Continue)
        }
    }

    fn register(
        &mut self,
        poll: &mut sctk::reexports::calloop::Poll,
        token_factory: &mut sctk::reexports::calloop::TokenFactory,
    ) -> sctk::reexports::calloop::Result<()> {
        let mut source = self.0.source.borrow_mut();
        source.register(poll, token_factory)
    }

    fn reregister(
        &mut self,
        poll: &mut sctk::reexports::calloop::Poll,
        token_factory: &mut sctk::reexports::calloop::TokenFactory,
    ) -> sctk::reexports::calloop::Result<()> {
        let mut source = self.0.source.borrow_mut();
        source.reregister(poll, token_factory)
    }

    fn unregister(
        &mut self,
        poll: &mut sctk::reexports::calloop::Poll,
    ) -> sctk::reexports::calloop::Result<()> {
        let mut source = self.0.source.borrow_mut();
        source.unregister(poll)
    }
}

#[derive(Debug)]
pub struct ProxyError(PingError);

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for ProxyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

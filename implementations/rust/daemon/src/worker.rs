use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use crate::config::Config;

use ockam_message::message::{AddressType, Message as OckamMessage, MessageType, RouterAddress};
use ockam_system::commands::{OckamCommand, RouterCommand, WorkerCommand};

type WorkFn = fn(self_worker: &Worker, msg: OckamMessage);

#[allow(dead_code)]
pub struct Worker {
    router_tx: Sender<OckamCommand>,
    rx: Receiver<OckamCommand>,
    tx: Sender<OckamCommand>,
    addr: RouterAddress,
    work_fn: WorkFn,
    config: Config,
}

impl Worker {
    pub fn new(
        addr: RouterAddress,
        router_tx: Sender<OckamCommand>,
        config: Config,
        work_fn: WorkFn,
    ) -> Self {
        debug_assert!(matches!(addr.a_type, AddressType::Worker));

        let (tx, rx) = mpsc::channel();

        // register the worker with the router
        let cmd = OckamCommand::Router(RouterCommand::Register(AddressType::Worker, tx.clone()));
        router_tx.send(cmd).expect("failed to register worker");

        println!("Service address: {}", addr.address.as_string());

        Worker {
            router_tx,
            rx,
            tx,
            addr,
            config,
            work_fn,
        }
    }

    pub fn sender(&self) -> Sender<OckamCommand> {
        self.tx.clone()
    }

    pub fn config(&self) -> Config {
        self.config.clone()
    }

    pub fn poll(&self) -> bool {
        match self.rx.try_recv() {
            Ok(cmd) => match cmd {
                OckamCommand::Worker(WorkerCommand::ReceiveMessage(msg)) => {
                    match msg.message_type {
                        MessageType::Payload => {
                            // Confirm address
                            if self.addr != msg.onward_route.addresses[0] {
                                println!("Received bad worker address");
                                return true;
                            }
                            (self.work_fn)(&self, msg);
                            true
                        }
                        MessageType::None => true,
                        _ => unimplemented!(),
                    }
                }
                _ => {
                    eprintln!("unrecognized worker command: {:?}", cmd);
                    false
                }
            },
            Err(e) => match e {
                TryRecvError::Empty => true,
                _ => {
                    eprintln!("failed to recv worker rx: {:?}", e);
                    false
                }
            },
        }
    }
}

#[test]
fn test_ockamd_worker() {
    let addr = RouterAddress::worker_router_address_from_str("01242020").unwrap();
    let (fake_router_tx, fake_router_rx) = mpsc::channel();
    Worker::new(addr, fake_router_tx, Default::default(), |_, _| {});

    assert!(fake_router_rx.recv().is_ok());
}

// M1 send
// M2 send
#![allow(dead_code)]
#![deny(
    missing_docs,
    missing_debug_implementations,
    trivial_casts,
    trivial_numeric_casts,
    unconditional_recursion,
    unused_import_braces,
    unused_lifetimes,
    unused_qualifications,
    unused_extern_crates,
    unused_parens,
    while_true
)]

//! Implements the Ockam channels interface and provides
//! a C FFI version.
//!
//! Channels are where parties can send messages securely

#![cfg_attr(feature = "nightly", feature(doc_cfg))]

#[macro_use]
extern crate ockam_common;

use core::marker::PhantomData;
use error::*;
use ockam_kex::{CompletedKeyExchange, KeyExchanger, NewKeyExchanger};
use ockam_message::message::{
    Address, AddressType, Codec, Message, MessageType, Route, RouterAddress,
};
use ockam_system::commands::OckamCommand::Router;
use ockam_system::commands::{ChannelCommand, OckamCommand, RouterCommand};
use ockam_vault::types::{PublicKey, SecretKeyContext};
use ockam_vault::DynVault;
use rand::{thread_rng, Rng};
use std::{
    collections::BTreeMap,
    sync::{
        mpsc::{Receiver, Sender},
        Arc, Mutex,
    },
};

/// A channel address of zero indicates to the channel manager that
/// a new channel is being initiated
pub static CHANNEL_ZERO: &str = "00000000";

enum ExchangerRole {
    Initiator,
    Responder,
}

/// A Channel Manager creates secure channels on demand using the specified key exchange
/// generic. All keys will be created in the associated vault object
pub struct ChannelManager<
    I: KeyExchanger + 'static,
    R: KeyExchanger + 'static,
    E: NewKeyExchanger<I, R>,
> {
    channels: BTreeMap<String, Arc<Mutex<Channel>>>,
    rx: Receiver<OckamCommand>,
    tx: Sender<OckamCommand>,
    router_tx: Sender<OckamCommand>,
    vault: Arc<Mutex<dyn DynVault + Send>>,
    new_key_exchanger: E,
    phantom_i: PhantomData<I>,
    phantom_r: PhantomData<R>,
    resp_key_ctx: Option<SecretKeyContext>,
    init_key_ctx: Option<SecretKeyContext>,
}

impl<I: KeyExchanger, R: KeyExchanger, E: NewKeyExchanger<I, R>> std::fmt::Debug
    for ChannelManager<I, R, E>
{
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "ChannelManager {{ channels: {:?}, receiver, sender, router, vault }}",
            self.channels
        )
    }
}

impl<I: KeyExchanger, R: KeyExchanger, E: NewKeyExchanger<I, R>> ChannelManager<I, R, E> {
    /// Create a new Channel Manager
    pub fn new(
        rx: Receiver<OckamCommand>,
        tx: Sender<OckamCommand>,
        router_tx: Sender<OckamCommand>,
        vault: Arc<Mutex<dyn DynVault + Send>>,
        new_key_exchanger: E,
        resp_key_ctx: Option<SecretKeyContext>,
        init_key_ctx: Option<SecretKeyContext>,
    ) -> Result<Self, ChannelError> {
        // register ChannelManager with the router as the handler for all Channel address types
        if let Err(_error) = router_tx.send(Router(RouterCommand::Register(
            AddressType::Channel,
            tx.clone(),
        ))) {
            println!("Channel failed ro register with router");
            return Err(ChannelErrorKind::CantSend.into());
        }

        Ok(Self {
            channels: BTreeMap::new(),
            tx,
            rx,
            router_tx,
            vault,
            new_key_exchanger,
            phantom_i: PhantomData,
            phantom_r: PhantomData,
            resp_key_ctx,
            init_key_ctx,
        })
    }

    /// Check for work to be done and do it
    pub fn poll(&mut self) -> Result<bool, ChannelError> {
        let keep_going = true;
        let mut got_message = true;
        while got_message {
            match self.rx.try_recv() {
                Ok(c) => match c {
                    OckamCommand::Channel(ChannelCommand::Initiate(
                        mut route,
                        return_address,
                        key,
                    )) => {
                        if let AddressType::Channel = route.addresses[0].a_type {
                            if route.addresses[0].address.as_string() == *CHANNEL_ZERO {
                                route.addresses.remove(0);
                            }
                        }
                        self.init_key_ctx = key;
                        self.initiate_new_channel(route, return_address)?;
                    }
                    OckamCommand::Channel(ChannelCommand::Stop) => {
                        self.channels.clear();
                        break;
                    }
                    OckamCommand::Channel(ChannelCommand::SendMessage(m)) => {
                        self.handle_send(m)?;
                    }
                    OckamCommand::Channel(ChannelCommand::ReceiveMessage(m)) => {
                        self.handle_recv(m)?;
                    }
                    _ => return Err(ChannelErrorKind::InvalidParam(0).into()),
                },
                Err(_) => {
                    got_message = false;
                }
            }
        }
        Ok(keep_going)
    }

    fn handle_send(&mut self, mut m: Message) -> Result<(), ChannelError> {
        if m.onward_route.addresses.is_empty() {
            return Err(ChannelErrorKind::CantSend.into());
        }
        match m.message_type {
            MessageType::Payload => {
                let address = m.onward_route.addresses[0].address.as_string();
                match self.channels.get_mut(&address) {
                    Some(channel) => {
                        let mut channel = channel.lock().unwrap();

                        if !channel.agreement.is_complete() {
                            debug_assert!(channel.completed_key_exchange.is_none());
                            return Ok(());
                        }

                        // remove this channel's address, encode message
                        m.onward_route.addresses.remove(0);
                        let mut m_encoded: Vec<u8> = vec![];

                        if Message::encode(&m, &mut m_encoded).is_err() {
                            return Err(ChannelErrorKind::CantSend.into());
                        }

                        debug_assert!(channel.completed_key_exchange.is_some());
                        let cke = channel.completed_key_exchange.as_ref().unwrap();
                        let mut vault = self.vault.lock().unwrap();

                        let mut new_message_body: Vec<u8> = vec![];
                        if let Err(e) = u16::encode(&channel.nonce, &mut new_message_body)
                            .map_err(|e| ChannelError::from_msg(ChannelErrorKind::CantSend, e))
                        {
                            return Err(e);
                        }

                        let nonce = Channel::nonce_16_to_96(channel.nonce);
                        let mut ciphertext_and_tag = vault.aead_aes_gcm_encrypt(
                            cke.encrypt_key,
                            &m_encoded,
                            &nonce,
                            &cke.h,
                        )?;
                        channel.nonce += 1;
                        //TODO: check if key rotation needs to happen

                        new_message_body.append(&mut ciphertext_and_tag);
                        let new_m = Message {
                            onward_route: channel.route.clone(),
                            return_route: Route {
                                addresses: vec![RouterAddress::from_address(
                                    channel.as_ciphertext_address(),
                                )
                                .unwrap()],
                            },
                            message_type: MessageType::Payload,
                            message_body: new_message_body,
                        };
                        self.router_tx
                            .send(Router(RouterCommand::SendMessage(new_m)))?;
                        Ok(())
                    }
                    None => Err(ChannelErrorKind::NotImplemented.into()),
                }
            }
            _ => Err(ChannelErrorKind::NotImplemented.into()),
        }
    }

    /// Initiates key exchange to create new secure channel over supplied route.
    /// Upon completion of key exchange, a message is sent to return_address with
    /// MessageType::None and the channel address in the return route.
    fn initiate_new_channel(
        &mut self,
        mut route: Route,
        return_address: Address,
    ) -> Result<Address, ChannelError> {
        // Remember who to notify when the channel is secure
        let pending_return = RouterAddress::from_address(return_address).unwrap();

        // Generate 2 channel addresses, one each for clear and cipher text
        let mut clear_address = String::from(CHANNEL_ZERO);
        let mut cipher_address = String::from(CHANNEL_ZERO);
        if let Some((clear, cipher)) = self.create_channel(ExchangerRole::Initiator) {
            clear_address = clear;
            cipher_address = cipher;
        }

        let channel = self.channels.get_mut(&cipher_address).unwrap();
        let mut channel = &mut *channel.lock().unwrap();
        channel.pending = Some(Message {
            onward_route: Route {
                addresses: vec![pending_return],
            },
            return_route: Route {
                addresses: vec![
                    RouterAddress::channel_router_address_from_str(&clear_address).unwrap(),
                ],
            },
            message_type: MessageType::None,
            message_body: vec![],
        });
        let ka_m1 = channel.agreement.process(&[])?;
        route
            .addresses
            .push(RouterAddress::channel_router_address_from_str(CHANNEL_ZERO).unwrap());
        let m = Message {
            onward_route: route,
            return_route: Route {
                addresses: vec![
                    RouterAddress::channel_router_address_from_str(&cipher_address).unwrap(),
                ],
            },
            message_type: MessageType::KeyAgreementM1,
            message_body: ka_m1,
        };
        self.router_tx.send(Router(RouterCommand::SendMessage(m)))?;
        Ok(Address::channel_address_from_string(&clear_address).unwrap())
    }

    fn handle_recv(&mut self, m: Message) -> Result<(), ChannelError> {
        if m.onward_route.addresses.is_empty() {
            // no onward route, how to determine which channel to decrypt message?
            // can't so drop
            return Err(ChannelErrorKind::RecvError.into());
        }
        // Pop the first onward address off to get the channel id.
        // If it's 0, we expect the message to be M1 of a key exchange
        // Respond accordingly
        let mut cipher_address = m.onward_route.addresses[0].address.as_string();
        if cipher_address == CHANNEL_ZERO {
            if let Some((_clear, cipher)) = self.create_channel(ExchangerRole::Responder) {
                cipher_address = cipher;
            } else {
                return Err(ChannelErrorKind::State.into());
            }
        }
        match self.channels.get_mut(&cipher_address) {
            Some(channel) => {
                let channel = channel.clone();
                return match m.message_type {
                    MessageType::KeyAgreementM1 => {
                        self.handle_m1_recv(channel, m)?;
                        Ok(())
                    }
                    MessageType::KeyAgreementM2 => {
                        self.handle_m2_recv(channel, m)?;
                        Ok(())
                    }
                    MessageType::KeyAgreementM3 => {
                        self.handle_m3_recv(channel, m)?;
                        Ok(())
                    }
                    MessageType::Payload => {
                        self.handle_payload_recv(channel, m)?;
                        Ok(())
                    }
                    _ => {
                        debug_assert!(false);
                        Err(ChannelErrorKind::NotImplemented.into())
                    }
                };
            }
            None => {
                debug_assert!(false, "unknown channel address");
                //Ok(())
                // Do nothing and drop message
            }
        }
        Ok(())
    }

    fn handle_payload_recv(
        &self,
        channel: Arc<Mutex<Channel>>,
        m: Message,
    ) -> Result<(), ChannelError> {
        // Decrypt, put address on onward route at 0 and send
        let mut channel = channel.lock().unwrap();
        if m.message_body.len() < 2 {
            return Err(ChannelErrorKind::RecvError.into());
        }
        let kex = channel.completed_key_exchange.as_ref().unwrap();

        return match u16::decode(&m.message_body) {
            Ok((nonce, cipher_text)) => {
                let nonce_96 = Channel::nonce_16_to_96(nonce);
                let mut vault = self.vault.lock().unwrap();
                let new_m_encoded =
                    vault.aead_aes_gcm_decrypt(kex.decrypt_key, cipher_text, &nonce_96, &kex.h)?;
                let (new_m, _) = Message::decode(&new_m_encoded).unwrap();
                channel.nonce += 1;
                self.router_tx
                    .send(Router(RouterCommand::ReceiveMessage(new_m)))?;
                Ok(())
            }
            _ => Err(ChannelErrorKind::InvalidParam(0).into()),
        };
    }

    fn handle_m1_recv(&self, channel: Arc<Mutex<Channel>>, m: Message) -> Result<(), ChannelError> {
        let channel = &mut *channel.lock().unwrap();
        channel.agreement.process(&m.message_body)?;
        let m2 = channel.agreement.process(&[])?;
        let m = Message {
            onward_route: m.return_route,
            return_route: Route {
                addresses: vec![
                    RouterAddress::from_address(channel.as_ciphertext_address()).unwrap()
                ],
            },
            message_type: MessageType::KeyAgreementM2,
            message_body: m2,
        };
        self.router_tx
            .send(Router(RouterCommand::SendMessage(m)))
            .unwrap();
        Ok(())
    }

    fn handle_m2_recv(&self, channel: Arc<Mutex<Channel>>, m: Message) -> Result<(), ChannelError> {
        let mut channel = &mut *channel.lock().unwrap();
        let return_route = m.return_route.clone();
        channel.agreement.process(&m.message_body)?;
        let m3 = channel.agreement.process(&[])?;
        let m = Message {
            onward_route: return_route.clone(),
            return_route: Route {
                addresses: vec![m.onward_route.addresses[0].clone()],
            },
            message_type: MessageType::KeyAgreementM3,
            message_body: m3,
        };
        self.router_tx
            .send(Router(RouterCommand::SendMessage(m)))
            .unwrap();
        channel.completed_key_exchange = Some(channel.agreement.finalize()?);
        channel.route = return_route;

        // let the worker know the key exchange is done
        let pending = channel.pending.clone();
        match pending {
            Some(mut p) => {
                // send the remote public key as the message body
                let static_public_key = channel
                    .completed_key_exchange
                    .unwrap()
                    .remote_static_public_key;
                p.message_body = static_public_key.as_ref().to_vec();
                self.router_tx
                    .send(Router(RouterCommand::ReceiveMessage(p)))
                    .unwrap();
            }
            None => {
                return Err(ChannelErrorKind::NotImplemented.into());
            }
        }
        Ok(())
    }

    fn handle_m3_recv(&self, channel: Arc<Mutex<Channel>>, m: Message) -> Result<(), ChannelError> {
        let mut channel = channel.lock().unwrap();
        let return_route = m.return_route.clone();
        // For now ignore anything returned from M3
        let _ = channel.agreement.process(&m.message_body)?;
        debug_assert!(channel.agreement.is_complete());
        if channel.completed_key_exchange.is_none() {
            // key agreement has finished, now can process any pending messages
            let pending = channel.pending.clone();
            channel.completed_key_exchange = Some(channel.agreement.finalize()?);
            channel.route = return_route;
            match pending {
                Some(mut p) => {
                    p.return_route = channel.route.clone();
                    p.return_route.addresses.insert(
                        0,
                        RouterAddress::from_address(channel.as_cleartext_address()).unwrap(),
                    );
                    // add the channel's remote public key as the message body
                    p.message_body = channel
                        .completed_key_exchange
                        .unwrap()
                        .remote_static_public_key
                        .as_ref()
                        .to_vec();

                    self.router_tx
                        .send(Router(RouterCommand::ReceiveMessage(p)))
                        .unwrap();
                    channel.pending = None;
                }
                _ => {
                    let mut return_route = channel.route.clone();
                    return_route.addresses.insert(
                        0,
                        RouterAddress::from_address(channel.as_cleartext_address()).unwrap(),
                    );
                    let new_m = Message {
                        onward_route: Route {
                            addresses: vec![RouterAddress::worker_router_address_from_str(
                                CHANNEL_ZERO,
                            )
                            .unwrap()],
                        },
                        return_route,
                        message_type: MessageType::None,
                        message_body: vec![],
                    };
                    self.router_tx
                        .send(Router(RouterCommand::ReceiveMessage(new_m)))
                        .unwrap();
                }
            }
        }
        Ok(())
    }

    fn create_channel(&mut self, role: ExchangerRole) -> Option<(String, String)> {
        let mut rng = thread_rng();
        let clear_u32 = rng.gen::<u32>();
        let cipher_u32 = rng.gen::<u32>();
        let channel = match role {
            ExchangerRole::Initiator => Arc::new(Mutex::new(Channel::new(
                clear_u32,
                cipher_u32,
                Box::new(self.new_key_exchanger.initiator(self.init_key_ctx)),
            ))),
            ExchangerRole::Responder => Arc::new(Mutex::new(Channel::new(
                clear_u32,
                cipher_u32,
                Box::new(self.new_key_exchanger.responder(self.resp_key_ctx)),
            ))),
        };
        let clear_address = Address::ChannelAddress(clear_u32.to_le_bytes().to_vec());
        let cipher_address = Address::ChannelAddress(cipher_u32.to_le_bytes().to_vec());
        self.channels
            .insert(clear_address.as_string(), channel.clone());
        self.channels.insert(cipher_address.as_string(), channel);
        Some((clear_address.as_string(), cipher_address.as_string()))
    }
}

struct Channel {
    completed_key_exchange: Option<CompletedKeyExchange>,
    remote_public_key: Option<PublicKey>,
    cleartext_address: u32,
    ciphertext_address: u32,
    agreement: Box<dyn KeyExchanger>,
    nonce: u16,
    route: Route,
    pending: Option<Message>,
}

impl std::fmt::Debug for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "Channel {{ completed_key_exchange: {:?}, id: {:?}, nonce: {:?}, agreement }}",
            self.completed_key_exchange, self.cleartext_address, self.nonce
        )
    }
}

impl Channel {
    //   pub fn new(cleartext_address: u32, ciphertext_address: u32, agreement: Box<dyn
    // KeyExchanger>) -> Self {
    pub fn new(
        cleartext_address: u32,
        ciphertext_address: u32,
        agreement: Box<dyn KeyExchanger>,
    ) -> Self {
        Self {
            cleartext_address,
            ciphertext_address,
            agreement,
            completed_key_exchange: None,
            nonce: 0,
            route: Route { addresses: vec![] },
            pending: None,
            remote_public_key: None,
        }
    }

    pub fn as_cleartext_address(&self) -> Address {
        Address::ChannelAddress(self.cleartext_address.to_le_bytes().to_vec())
    }

    pub fn as_ciphertext_address(&self) -> Address {
        Address::ChannelAddress(self.ciphertext_address.to_le_bytes().to_vec())
    }

    pub fn nonce_16_to_96(n16: u16) -> [u8; 12] {
        // the nonce value is an le u16, whereas the nonce
        // byte array is 10 bytes of 0's follow by the be
        // representation of the nonce
        let mut n: [u8; 12] = [0; 12];
        let b = n16.to_be_bytes();
        n[10] = b[0];
        n[11] = b[1];
        n
    }

    pub fn nonce_from_96(n: &[u8; 12]) -> u16 {
        let bytes: [u8; 2] = [n[10], n[11]];
        u16::from_be_bytes(bytes)
    }
}

/// Represents the errors that occur within a channel
pub mod error;
// #[cfg(test)]
// mod tests {
//     use super::*;
//     use ockam_kex::xx::{XXInitiator, XXNewKeyExchanger, XXResponder};
//     use ockam_kex::CipherSuite;
//     use ockam_message::message::AddressType;
//     use ockam_vault::software::DefaultVault;
//     use std::sync::mpsc::channel;
//
//     type XXInitiatorChannelManager = ChannelManager<XXInitiator, XXResponder, XXNewKeyExchanger>;
//     type XXResponderChannelManager = ChannelManager<XXInitiator, XXResponder, XXNewKeyExchanger>;
//
//     #[test]
//     fn new_channel_initiator() {
//         let (tx_router, rx_router) = channel();
//         let (tx_channel, rx_channel) = channel();
//
//         let new_key_exchanger = XXNewKeyExchanger::new(CipherSuite::Curve25519AesGcmSha256);
//         let vault = Arc::new(Mutex::new(DefaultVault::default()));
//
//         let mut router = ockam_router::router::Router::new(rx_router);
//         let mut channel = XXInitiatorChannelManager::new(
//             tx_channel.clone(),
//             rx_channel,
//             tx_router.clone(),
//             vault.clone(),
//             new_key_exchanger,
//         );
//
//         tx_router
//             .send(OckamCommand::Router(RouterCommand::Register(
//                 AddressType::Channel,
//                 tx_channel.clone(),
//             )))
//             .unwrap();
//
//         let message = Message {
//             onward_route: Route {
//                 addresses:
// vec![RouterAddress::channel_router_address_from_str("deadbeef").unwrap()],             },
//             return_route: Route { addresses: vec![] },
//             message_type: MessageType::Payload,
//             message_body: b"Hello Bob".to_vec(),
//         };
//
//         tx_router
//             .send(OckamCommand::Router(RouterCommand::SendMessage(message)))
//             .unwrap();
//         assert!(router.poll());
//         let res = channel.poll();
//         assert!(res.is_ok());
//         assert!(res.unwrap());
//     }
//
//     #[test]
//     fn new_channel_responder() {
//         let (tx_router, rx_router) = channel();
//         let (tx_channel, rx_channel) = channel();
//
//         let new_key_exchanger = XXNewKeyExchanger::new(CipherSuite::Curve25519AesGcmSha256);
//         let vault = Arc::new(Mutex::new(DefaultVault::default()));
//
//         let mut router = ockam_router::router::Router::new(rx_router);
//         let mut channel = XXResponderChannelManager::new(
//             tx_channel.clone(),
//             rx_channel,
//             tx_router.clone(),
//             vault.clone(),
//             new_key_exchanger,
//         );
//
//         tx_router
//             .send(OckamCommand::Router(RouterCommand::Register(
//                 AddressType::Channel,
//                 tx_channel.clone(),
//             )))
//             .unwrap();
//
//         let message = Message {
//             onward_route: Route {
//                 addresses: vec![RouterAddress::channel_router_address_from_str("00").unwrap()],
//             },
//             return_route: Route { addresses: vec![] },
//             message_type: MessageType::KeyAgreementM1,
//             message_body: vec![
//                 79, 30, 59, 197, 255, 25, 84, 22, 3, 63, 63, 45, 98, 206, 16, 137, 39, 108, 13,
//                 171, 237, 191, 172, 115, 63, 124, 209, 114, 59, 97, 28, 82,
//             ],
//         };
//
//         tx_router
//             .send(OckamCommand::Router(RouterCommand::SendMessage(message)))
//             .unwrap();
//         assert!(router.poll());
//         let res = channel.poll();
//         assert!(res.is_ok());
//         assert!(res.unwrap());
//     }
// }

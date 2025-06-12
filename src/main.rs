use std::error::Error;

use futures::prelude::*;
use libp2p::{development_transport, identity, mdns::{Mdns, MdnsEvent}, NetworkBehaviour, PeerId, ping::{Ping, PingConfig, PingEvent}, swarm::{SwarmBuilder, SwarmEvent}, Swarm};
use libp2p::kad::{
    GetRecordError,
    GetRecordOk, Kademlia, KademliaEvent, PutRecordError, PutRecordOk, Quorum, Record, record::Key, record::store::MemoryStore,
};
use libp2p::mdns::MdnsConfig;
use libp2p::multiaddr::Protocol;
use tokio::sync::oneshot;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    #[derive(NetworkBehaviour)]
    #[behaviour(event_process = true)]
    struct MyBehaviour {
        ping: Ping,
        mdns: Mdns,
        kademlia: Kademlia<MemoryStore>,
    }

    impl MyBehaviour {
        async fn new(local_peer_id: PeerId) -> Result<Self, Box<dyn Error>> {
            Ok(Self {
                ping: Ping::new(PingConfig::new()),
                mdns: Mdns::new(MdnsConfig::default()).await?,
                kademlia: Kademlia::new(local_peer_id, MemoryStore::new(local_peer_id)),
            })
        }
    }

    impl libp2p::swarm::NetworkBehaviourEventProcess<PingEvent> for MyBehaviour {
        fn inject_event(&mut self, event: PingEvent) {
            println!("Ping event: {:?}", event);
        }
    }

    impl libp2p::swarm::NetworkBehaviourEventProcess<MdnsEvent> for MyBehaviour {
        fn inject_event(&mut self, event: MdnsEvent) {
            match event {
                MdnsEvent::Discovered(list) => {
                    for (peer, addr) in list {
                        println!("Discovered peer {} at {}", peer, addr);
                    }
                }
                MdnsEvent::Expired(list) => {
                    for (peer, addr) in list {
                        println!("Expired peer {} at {}", peer, addr);
                    }
                }
            }
        }
    }

    impl libp2p::swarm::NetworkBehaviourEventProcess<KademliaEvent> for MyBehaviour {
        fn inject_event(&mut self, event: KademliaEvent) {
            match event {
                KademliaEvent::OutboundQueryCompleted { result, .. } => match result {
                    libp2p::kad::QueryResult::GetRecord(Ok(GetRecordOk { records, .. })) => {
                        for record in records {
                            println!(
                                "Got record for key {:?}: {:?}",
                                record.record.key,
                                String::from_utf8_lossy(&record.record.value)
                            );
                        }
                    }
                    libp2p::kad::QueryResult::GetRecord(Err(GetRecordError::NotFound { key, .. })) => {
                        println!("Record not found for key {:?}", key);
                    }
                    libp2p::kad::QueryResult::PutRecord(Ok(PutRecordOk { key })) => {
                        println!("Successfully stored record for key {:?}", key);
                    }
                    libp2p::kad::QueryResult::PutRecord(Err(PutRecordError::QuorumFailed { quorum, .. })) => {
                        println!("Failed to store record for QuorumFailed {:?}", quorum);
                    }
                    _ => {}
                },
                KademliaEvent::RoutingUpdated { peer, .. } => {
                    println!("Routing table updated with {:?}", peer);
                }
                _ => {}
            }
        }
    }
    let first_key = identity::Keypair::generate_ed25519();
    let first_peer_id = PeerId::from(first_key.public());
    println!("first peer ID: {:?}", first_peer_id);

    let second_key = identity::Keypair::generate_ed25519();
    let second_peer_id = PeerId::from(second_key.public());
    println!("Second peer ID: {:?}", second_peer_id);


    // first peer
    let first_peer_transport = development_transport(first_key.clone()).await?;
    let first_peer_behaviour = MyBehaviour::new(first_peer_id).await?;

    // second peer
    let second_peer_transport = development_transport(second_key.clone()).await?;
    let second_peer_behaviour = MyBehaviour::new(second_peer_id).await?;

    let mut first_peer_swarm = SwarmBuilder::new(first_peer_transport, first_peer_behaviour, first_peer_id)
        .executor(Box::new(|fut| {
            // Spawn the task and ignore the JoinHandle.
            tokio::spawn(fut);
        }))
        .build();
    first_peer_swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;


    let mut second_peer_swarm = SwarmBuilder::new(second_peer_transport, second_peer_behaviour, second_peer_id)
        .executor(Box::new(|fut| {
            // Spawn the task and ignore the JoinHandle.
            tokio::spawn(fut);
        }))
        .build();
    second_peer_swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    // === Run both peers in separate tasks ===
    let (addr_sender, addr_receiver) = oneshot::channel();

    let swarm1_fut = {
        let mut sent = false;
        let mut addr_sender = Some(addr_sender); // Wrap in Option to move out safely

        Box::pin(async move {
            loop {
                match first_peer_swarm.select_next_some().await {
                    SwarmEvent::NewListenAddr { address, .. } => {
                        println!("Peer 1 listening on: {:?}", address);
                        if !sent {
                            if let Some(sender) = addr_sender.take() {
                                if sender.send(address.clone()).is_err() {
                                    eprintln!("Failed to send address to Peer 2");
                                }
                                sent = true;
                            }
                        }
                    }
                    SwarmEvent::Behaviour(_) => {}
                    _ => {}
                }
            }
        })
    };

    let swarm2_fut = {
        let mut dialed = false;
        let mut maybe_addr = Some(addr_receiver); // Store receiver inside Option

        Box::pin(async move {
            loop {
                match second_peer_swarm.select_next_some().await {
                    evt => {
                        println!("Peer 2 event: {:?}", evt);

                        if let SwarmEvent::NewListenAddr { .. } = &evt {
                            if !dialed {
                                if let Some(receiver) = maybe_addr.take() {
                                    match receiver.await {
                                        Ok(peer1_addr) => {
                                            let dial_addr = peer1_addr.with(Protocol::P2p(first_peer_id.into()));
                                            println!("Peer 2 dialing Peer 1 at: {:?}", dial_addr);

                                            second_peer_swarm.behaviour_mut().kademlia.add_address(&first_peer_id, dial_addr.clone());

                                            if let Err(e) = Swarm::dial(&mut second_peer_swarm, dial_addr) {
                                                eprintln!("Dial error: {:?}", e);
                                            }

                                            dialed = true;
                                        }
                                        Err(e) => {
                                            eprintln!("Failed to receive address: {:?}", e);
                                        }
                                    }
                                }
                            }
                        }

                        if let SwarmEvent::ConnectionEstablished { peer_id, .. } = &evt {
                            if *peer_id == first_peer_id {
                                println!("Connection established with first peer");

                                let record = Record {
                                    key: Key::new(&"my-key"),
                                    value: b"hello-libp2p".to_vec(),
                                    publisher: Some(second_peer_id),
                                    expires: None,
                                };
                                if let Err(e) = second_peer_swarm.behaviour_mut().kademlia.put_record(record, Quorum::One) {
                                    println!("Failed to start put_record: {:?}", e);
                                } else {
                                    println!("Started storing record.");
                                }

                                // query the record right after storing
                                let key = Key::new(&"my-key");
                                second_peer_swarm.behaviour_mut().kademlia.get_record(key, Quorum::One);
                            }
                        }
                    }
                }
            }
        })
    };

    // === Run both peers in separate tasks ===
    futures::future::join(swarm1_fut, swarm2_fut).await;

    Ok(())
}


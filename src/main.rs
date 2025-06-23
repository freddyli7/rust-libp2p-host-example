use std::error::Error;
use std::time::{Duration, Instant};

use ed25519_dalek::{Keypair as DalekKeypair, PublicKey, SecretKey};
use futures::prelude::*;
use libp2p::{development_transport, mdns::{Mdns, MdnsEvent}, NetworkBehaviour, PeerId, ping::{Ping, PingConfig, PingEvent}, swarm::{SwarmBuilder, SwarmEvent}, Swarm};
use libp2p::identity::{ed25519, Keypair};
use libp2p::kad::{GetRecordError, GetRecordOk, Kademlia, KademliaConfig, KademliaEvent, PutRecordError, PutRecordOk, Quorum, Record, record::Key, record::store::MemoryStore};
use libp2p::mdns::MdnsConfig;
use libp2p::multiaddr::Protocol;
use tokio::sync::oneshot;
use tokio::time::{self, Interval};

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
            let mut config = KademliaConfig::default();
            config.set_protocol_name(b"/record/kad/1.0.0".to_vec());
            let store = MemoryStore::new(local_peer_id);
            let kademlia = Kademlia::with_config(local_peer_id, store, config);

            Ok(Self {
                ping: Ping::new(PingConfig::new().with_keep_alive(true)),
                mdns: Mdns::new(MdnsConfig::default()).await?,
                kademlia,
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
                            // println!(
                            //     "Got record for key {:?}: {:?}",
                            //     record.record.key,
                            //     String::from_utf8_lossy(&record.record.value)
                            // );
                            println!(
                                "Got record for key {:?}: {:?}",
                                record.record.key,
                                 &record.record
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

    let alice_seed: [u8; 32] = [
        0x90, 0x75, 0x4e, 0x4e, 0x4a, 0x7f, 0xd2, 0x7d,
        0xd2, 0x1d, 0x5f, 0x99, 0x45, 0x1c, 0x4f, 0x6d,
        0x4a, 0x39, 0x49, 0x03, 0xf7, 0x49, 0x3a, 0x5c,
        0x9f, 0x53, 0x9e, 0xd3, 0x7b, 0x9c, 0x5a, 0x0f,
    ];
    let (alice_keypair, alice_peer_id) = generate_the_key(alice_seed);
    println!("Alice PeerId: {}", alice_peer_id);

    let bob_seed: [u8; 32] = [
        0x8e, 0xaf, 0x04, 0x8b, 0x9b, 0x29, 0xa9, 0xdd,
        0xf4, 0x3a, 0xc2, 0x7f, 0x25, 0xc9, 0x0c, 0x0f,
        0x2e, 0x85, 0x8e, 0xa3, 0xb7, 0x4e, 0xf6, 0x03,
        0x52, 0x4c, 0x6a, 0x45, 0xa1, 0x0d, 0x9d, 0x64,
    ];
    let (bob_keypair, bob_peer_id) = generate_the_key(bob_seed);
    println!("Bob PeerId: {}", bob_peer_id);

    // first peer
    let first_peer_transport = development_transport(alice_keypair.clone()).await?;
    let first_peer_behaviour = MyBehaviour::new(alice_peer_id).await?;

    // second peer
    let second_peer_transport = development_transport(bob_keypair.clone()).await?;
    let second_peer_behaviour = MyBehaviour::new(bob_peer_id).await?;

    let mut first_peer_swarm = SwarmBuilder::new(first_peer_transport, first_peer_behaviour, alice_peer_id)
        .executor(Box::new(|fut| {
            // Spawn the task and ignore the JoinHandle.
            tokio::spawn(fut);
        }))
        .build();
    first_peer_swarm.listen_on("/ip4/0.0.0.0/tcp/8080".parse()?)?;


    let mut second_peer_swarm = SwarmBuilder::new(second_peer_transport, second_peer_behaviour, bob_peer_id)
        .executor(Box::new(|fut| {
            // Spawn the task and ignore the JoinHandle.
            tokio::spawn(fut);
        }))
        .build();
    second_peer_swarm.listen_on("/ip4/0.0.0.0/tcp/8081".parse()?)?;

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
            let mut interval: Option<Interval> = None;
            let rust_side_key = Key::new(&"/record/my-key-rust");

            loop {
                tokio::select! {
            event = second_peer_swarm.select_next_some() => {
                println!("Peer 2 event: {:?}", event);

                if let SwarmEvent::NewListenAddr { .. } = &event {
                    if !dialed {
                        if let Some(receiver) = maybe_addr.take() {
                            match receiver.await {
                                Ok(peer1_addr) => {
                                    let dial_addr = peer1_addr.with(Protocol::P2p(alice_peer_id.into()));
                                    println!("Peer 2 dialing Peer 1 at: {:?}", dial_addr);

                                    second_peer_swarm.behaviour_mut().kademlia.add_address(&alice_peer_id, dial_addr.clone());

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

                if let SwarmEvent::ConnectionEstablished { peer_id, .. } = &event {
                    if *peer_id == alice_peer_id {
                        println!("Connection established with first peer");

                        let record = Record {
                            key: rust_side_key.clone(),
                            value: b"hello-libp2p-rust".to_vec(),
                            publisher: Some(bob_peer_id),
                            expires: Some(Instant::now() + Duration::from_secs(3000)),
                        };

                        if let Err(e) = second_peer_swarm.behaviour_mut().kademlia.put_record(record, Quorum::One) {
                            println!("Failed to start put_record: {:?}", e);
                        } else {
                            println!("Started storing record.");
                        }

                        second_peer_swarm.behaviour_mut().kademlia.get_record(rust_side_key.clone(), Quorum::One);
                        interval = Some(time::interval(Duration::from_secs(5)));
                    }
                }
            }

            // Triggered every 5 seconds to query the key again
                    // Query against Bob's peer
            _ = async {
                if let Some(i) = interval.as_mut() {
                    i.tick().await;
                }
            }, if interval.is_some() => {
                        // put_value and put_value_at_peer key
                        let query_key = Key::new(&"/record/12D3KooWP2F2DdjvoPbgC8VLU1PH9WB1NTnjAXFNiWhbpioWYSbR");

                        // local record key
                        // let query_key = Key::new(&"/record/my-key-go");
                println!("Querying record from Go side for key {:?}", query_key);
                second_peer_swarm.behaviour_mut().kademlia.get_record(query_key.clone(), Quorum::One);
            }
        }
            }
        })
    };

    futures::future::join(swarm1_fut, swarm2_fut).await;

    Ok(())
}

fn generate_the_key(b: [u8; 32]) -> (Keypair, PeerId) {
    // Create SecretKey from seed bytes
    let secret = SecretKey::from_bytes(&b).expect("valid secret key");
    // Derive PublicKey from SecretKey
    let public = PublicKey::from(&secret);
    // Construct the full Keypair (secret + public)
    let dalek_kp = DalekKeypair { secret, public };
    // Encode dalek keypair into bytes
    let mut dalek_bytes = dalek_kp.to_bytes();
    // Decode bytes into libp2p's ed25519 keypair
    let libp2p_kp = ed25519::Keypair::decode(&mut dalek_bytes).expect("valid libp2p ed25519 keypair");
    // Wrap in libp2p Keypair enum
    let keypair = Keypair::Ed25519(libp2p_kp);

    (keypair.clone(), keypair.public().to_peer_id())
}
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::thread;

use eyre::{eyre, Result};
use flume::{unbounded, Receiver, Sender};

use tendermint::node;
use tendermint::public_key::PublicKey;

use crate::message;
use crate::peer;
use crate::transport::{self, Endpoint as _};

pub enum Direction {
    Incoming,
    Outgoing,
}

pub enum Command {
    Accept,
    Connect(SocketAddr),
    Disconnect(node::Id),
    Msg(node::Id, message::Send),
}

pub enum Event {
    Connected(node::Id, Direction),
    Disconnected(node::Id),
    Message(node::Id, message::Receive),
}

enum Internal<Conn> {
    Accepted(Conn),
    Command(Command),
    Connected(Conn),
}

pub struct Supervisor {
    command: Sender<Command>,
    events: Receiver<Event>,
}

impl Supervisor {
    pub fn run<Conn, T>(transport: T) -> Result<Self>
    where
        T: transport::Transport + Send + 'static,
    {
        let (command, commands) = unbounded();
        let (event, events) = unbounded();
        let supervisor = Self { command, events };

        let (endpoint, mut incoming) = transport.bind(transport::BindInfo {
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 12345),
            advertise_addrs: vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
                12345,
            )],
            public_key: PublicKey::from_raw_ed25519(&[
                215, 90, 152, 1, 130, 177, 10, 183, 213, 75, 254, 211, 201, 100, 7, 58, 14, 225,
                114, 243, 218, 166, 35, 37, 175, 2, 26, 104, 247, 7, 81, 26,
            ])
            .unwrap(),
        })?;

        // ACCEPT
        let (accept_tx, accept_rx) = unbounded::<()>();
        let (accepted_tx, accepted_rx) = unbounded();
        thread::spawn(move || loop {
            accept_rx.recv().unwrap();

            let conn = incoming.next().unwrap().unwrap();

            accepted_tx.send(Internal::Accepted(conn)).unwrap();
        });

        // CONNECT
        let (_connect_tx, connect_rx) = unbounded::<transport::ConnectInfo>();
        let (connected_tx, connected_rx) = unbounded();
        thread::spawn(move || loop {
            let info = connect_rx.recv().unwrap();
            let conn = endpoint.connect(info).unwrap();

            connected_tx.send(Internal::Connected(conn)).unwrap();
        });

        // MAIN
        thread::spawn(move || {
            let mut state: State<T> = State {
                connected: HashMap::new(),
                stopped: HashMap::new(),
            };
            loop {
                let selector = flume::Selector::new()
                    .recv(&accepted_rx, |accepted| accepted.unwrap())
                    .recv(&commands, |res| Internal::Command(res.unwrap()))
                    .recv(&connected_rx, |connected| connected.unwrap());

                let _commands = state.transition(selector.wait());
            }
        });

        Ok(supervisor)
    }

    pub fn recv(&self) -> Result<Event> {
        match self.events.recv() {
            Ok(msg) => Ok(msg),
            Err(err) => Err(eyre!("sender disconnected: {}", err)),
        }
    }

    pub fn command(&self, cmd: Command) -> Result<()> {
        Ok(self.command.send(cmd).unwrap())
    }
}

struct State<T>
where
    T: transport::Transport,
{
    connected:
        HashMap<node::Id, peer::Peer<peer::Running<<T as transport::Transport>::Connection>>>,
    stopped: HashMap<node::Id, peer::Peer<peer::Stopped>>,
}

impl<T> State<T>
where
    T: transport::Transport,
{
    fn transition(
        &mut self,
        input: Internal<<T as transport::Transport>::Connection>,
    ) -> Vec<Event> {
        match input {
            Internal::Accepted(conn) => self.handle_accepted(conn),
            Internal::Command(command) => self.handle_command(command),
            Internal::Connected(conn) => self.handle_connected(conn),
        }
    }

    fn handle_accepted(&mut self, conn: <T as transport::Transport>::Connection) -> Vec<Event> {
        // TODO(xla): Separate upgrade procedure into own routine.
        let peer = peer::Peer::from(transport::Direction::Incoming(conn));
        // TODO(xla): Wire up stream (f.k.a channels) configuration.
        let peer = peer.run(vec![]).unwrap();
        let id = peer.id;
        self.connected.insert(peer.id, peer);

        vec![Event::Connected(id, Direction::Incoming)]
    }

    fn handle_command(&mut self, command: Command) -> Vec<Event> {
        match command {
            Command::Accept => vec![],
            Command::Connect(_addr) => vec![],
            Command::Disconnect(id) => {
                let peer = self.connected.remove(&id).unwrap();
                let stopped = peer.stop().unwrap();
                self.stopped.insert(id, stopped);

                vec![Event::Disconnected(id)]
            }
            Command::Msg(peer_id, msg) => {
                let peer = self.connected.get(&peer_id).unwrap();

                peer.send(msg).unwrap();

                vec![]
            }
        }
    }

    fn handle_connected(&mut self, conn: <T as transport::Transport>::Connection) -> Vec<Event> {
        // TODO(xla): Separate upgrade procedure into own routine.
        let peer = peer::Peer::from(transport::Direction::Outgoing(conn));
        // TODO(xla): Wire up stream (f.k.a channels) configuration.
        let peer = peer.run(vec![]).unwrap();
        let id = peer.id;
        self.connected.insert(peer.id, peer);

        vec![Event::Connected(id, Direction::Outgoing)]
    }
}

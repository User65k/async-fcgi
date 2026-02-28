use std::process::Output;

use tokio::task::JoinSet;

use crate::client::connection::{Connection, PreparedConnection};

//take connection from pool
//run prep_con
//add new conns to pool
//add conns to pool once the request is done with them
struct Pool {
    available: Vec<Connection>,
    probed: JoinSet<Result<PreparedConnection, std::io::Error>>,
    connecting: JoinSet<Result<Connection, std::io::Error>>,//JoinSet gets aborted on drop
}
impl Pool {
    fn gimme(&mut self) -> Result<(Connection, PreparedConnection), std::io::Error> {
        for c in self.available.drain(..) {

        }
    }
}

//where F: Future<Output=Result<Connection, std::io::Error>>
enum ConSlot<F> {
    Esta(Connection),
    New(F),

}
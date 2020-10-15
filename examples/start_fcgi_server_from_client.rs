//! start php on localhost 1236 and connect to it

#![deny(warnings)]
use async_fcgi::client::con_pool::ConPool;
use async_fcgi::stream::FCGIAddr;
use std::collections::HashMap;
use tokio::process::Child;

#[tokio::main]
async fn main() {
    pretty_env_logger::init();

    let mut env = HashMap::new();
    env.insert("PHP_FCGI_CHILDREN",     "16");
    env.insert("PHP_FCGI_MAX_REQUESTS", "10000");
    
    let addr: FCGIAddr = "127.0.0.1:1236".parse().expect("FCGIAddr");
    let php: Child = ConPool::prep_server("/usr/bin/php-cgi7.4", &addr)
        .await.expect("command")
        //.args(&["/usr/bin/echo", "2"])
        .env_clear().envs(env)
        .kill_on_drop(true)
        .spawn()
        .expect("command failed to start");
    
    ConPool::new(&addr).await.unwrap();
    print!("{}\t!!\r\n", php.await.expect("cmd failed"));
}

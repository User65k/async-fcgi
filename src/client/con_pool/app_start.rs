
#[cfg(unix)]
use async_stream_connection::Listener;
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::{ffi::OsStr, process::Stdio};
use tokio::process::Command;
use async_stream_connection::Addr;

impl super::ConPool {
    /// Setup a [`Command`] to spin up a FCGI server / application
    /// and make it listen on `sock_addr`.
    /// ```no_run
    /// # use async_fcgi::{client::con_pool::ConPool,FCGIAddr};
    /// # use std::collections::HashMap;
    /// # use std::error::Error;
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(),Box<dyn Error>> {
    /// let mut env = HashMap::new();
    /// env.insert("PHP_FCGI_CHILDREN", "16");
    /// env.insert("PHP_FCGI_MAX_REQUESTS", "10000");
    /// let addr: FCGIAddr = "127.0.0.1:1236".parse()?;
    /// let php = ConPool::prep_server("/usr/bin/php-cgi7.4", &addr)
    ///             .await?
    ///             .env_clear().envs(env)
    ///             .spawn()?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn prep_server<S>(program: S, sock_addr: &Addr) -> Result<Command, std::io::Error>
    where
        S: AsRef<OsStr>,
    {
        // The Web server leaves a single file descriptor, FCGI_LISTENSOCK_FILENO, open when the application begins execution.
        // This descriptor refers to a listening socket created by the Web server.
        #[cfg(not(unix))]
        let stdin = Stdio::null();
        #[cfg(unix)]
        let stdin = {
            let l = Listener::bind(sock_addr).await?;
            let fd = unsafe { Stdio::from_raw_fd(l.as_raw_fd()) };
            std::mem::forget(l); // FCGI App closes this - at least php-cgi7.4 does it
            fd
        };

        let mut command = Command::new(program);
        command
                .stdin(stdin) // FCGI_LISTENSOCK_FILENO equals STDIN_FILENO.
                //.stdout(Stdio::null()).stderr(Stdio::null()) // The standard descriptors STDOUT_FILENO and STDERR_FILENO are closed when the application begins execution.
                ;
        Ok(command)
    }
}
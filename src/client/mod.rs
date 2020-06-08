/*! Fast CGI client/webserver side
 * 
*/

#[cfg(feature = "web_server")]
pub mod connection;
#[cfg(feature = "con_pool")]
pub mod con_pool;
